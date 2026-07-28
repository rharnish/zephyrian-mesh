use axum::{
    extract::{
        ws::{Message, WebSocket, WebSocketUpgrade},
        Path, State,
    },
    response::IntoResponse,
    routing::{delete, get, post},
    Json, Router,
};
use serde::Deserialize;
use sim_server::config;
use sim_server::sim::{Command, World};
use sim_server::wind_field::WindField;
use std::sync::Arc;
use tokio::sync::{broadcast, mpsc};
use tower_http::cors::CorsLayer;

#[derive(Clone)]
struct AppState {
    commands: mpsc::UnboundedSender<Command>,
    snapshots: broadcast::Sender<String>,
    // The wind field, shared with the sim task (World also holds this Arc).
    // Served to the browser via GET /api/wind-levels so the frontend fetches
    // wind from sim-server instead of hitting wind_backend.py directly.
    wind: Arc<WindField>,
}

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt::init();

    let wind = Arc::new(fetch_wind_field().await);

    let (command_tx, mut command_rx) = mpsc::unbounded_channel::<Command>();
    // Deliberately shallow (~0.8s at 20 Hz). This is a live view, not a log: a
    // client that falls behind wants the *newest* state, not a backlog of stale
    // frames replayed at it. Overflow is therefore normal and handled by
    // skipping frames in `handle_socket`, not by growing this buffer.
    let (snapshot_tx, _) = broadcast::channel::<String>(16);

    let state = AppState {
        commands: command_tx,
        snapshots: snapshot_tx.clone(),
        wind: wind.clone(),
    };

    // The single task that owns `World`. Everything else only talks to it
    // through `command_tx` (mutations) or `snapshot_tx` (read-only state).
    tokio::spawn(async move {
        let mut world = World::new(wind);
        // `wind` (the Arc) was moved into World; the HTTP layer keeps its own
        // clone in AppState.
        world.spawn_balloon_pool(config::BALLOON_POOL_SIZE);
        world.set_visible_count(config::DEFAULT_NUM_BALLOONS);
        for &(lon, lat, height_m) in config::INITIAL_TOWERS {
            world.add_tower(lon, lat, height_m);
        }

        let dt_seconds = config::TICK_DT_SECONDS * config::TIME_SCALE;
        let mut interval = tokio::time::interval(std::time::Duration::from_millis(config::TICK_INTERVAL_MS));

        loop {
            interval.tick().await;
            while let Ok(cmd) = command_rx.try_recv() {
                world.apply(cmd);
            }
            let snapshot = world.tick(dt_seconds);
            match serde_json::to_string(&snapshot) {
                Ok(json) => {
                    // Ignore send errors — they just mean no clients are connected.
                    let _ = snapshot_tx.send(json);
                }
                Err(e) => tracing::error!("failed to serialize snapshot: {e}"),
            }
        }
    });

    let app = Router::new()
        .route("/ws", get(ws_handler))
        .route("/api/towers", post(add_tower))
        .route("/api/towers/:id", delete(remove_tower))
        .route("/api/balloons/count", post(set_balloon_count))
        .route("/api/horizon-coeff", post(set_horizon_coeff))
        .route("/api/paused", post(set_paused))
        .route("/api/wind-levels", get(get_wind_levels))
        .route("/api/balloons/:id/comms", get(get_balloon_comms))
        .layer(CorsLayer::permissive())
        .with_state(state);

    let listener = tokio::net::TcpListener::bind("0.0.0.0:8080").await.unwrap();
    tracing::info!("sim-server listening on ws://127.0.0.1:8080/ws");
    axum::serve(listener, app).await.unwrap();
}

async fn fetch_wind_field() -> WindField {
    match reqwest::get(config::WIND_API_URL).await {
        Ok(resp) => match resp.json::<WindField>().await {
            Ok(wind) => {
                tracing::info!("loaded wind field from {}", config::WIND_API_URL);
                wind
            }
            Err(e) => {
                tracing::warn!("failed to parse wind field response, using zero wind: {e}");
                WindField::zero()
            }
        },
        Err(e) => {
            tracing::warn!(
                "failed to fetch wind field from {} (is wind_backend.py running?), using zero wind: {e}",
                config::WIND_API_URL
            );
            WindField::zero()
        }
    }
}

// Serves the wind field to the browser in the same JSON shape wind_backend.py
// returns (WindField re-serializes to it). The field is static after startup,
// so this just hands back the shared Arc — no recomputation per request.
async fn get_wind_levels(State(state): State<AppState>) -> impl IntoResponse {
    Json(state.wind.clone())
}

async fn ws_handler(ws: WebSocketUpgrade, State(state): State<AppState>) -> impl IntoResponse {
    ws.on_upgrade(move |socket| handle_socket(socket, state))
}

/// Feeds one browser, sending only the newest snapshot and never queueing
/// ahead of what that client can actually take.
///
/// The subtlety is what `socket.send(..).await` means: it hands the message to
/// this connection's write sink and returns, *not* when the browser has
/// received or drawn anything. The previous loop therefore pulled a snapshot
/// and pushed it at the socket as fast as the broadcast channel would yield,
/// which for a slow client meant piling world states into that sink for ever.
///
/// Measured before this change, with one software-rendered browser attached:
/// the client sat 752 ticks (~37s) behind while receiving a perfectly
/// contiguous tick sequence — nothing dropped anywhere — and the server held
/// 251 MB resident. Only ~3 MB of that was in the kernel socket buffer; the
/// rest was queued in userspace here. So this was never only a latency
/// problem: a slow client made this process grow without bound.
///
/// Now at most one snapshot is outstanding at a time, and before each send we
/// skip to the newest one available. A snapshot is a complete picture of the
/// world, so an older one still waiting to go out has no value once a newer
/// one exists — sending it means spending bandwidth to show a state that is
/// already wrong, and delaying the state that is right.
///
/// A client that keeps up sees no change: `try_recv` finds nothing waiting and
/// every snapshot goes out as before.
async fn handle_socket(mut socket: WebSocket, state: AppState) {
    let mut rx = state.snapshots.subscribe();
    loop {
        // A slow client must be allowed to *skip* frames. Treating `Lagged` as
        // end-of-stream (as `while let Ok(..)` does) hangs up on it instead,
        // and since the frontend reconnects 2s later that turns a few dropped
        // frames into a visible freeze-then-jump every ~10 seconds.
        let mut json = match rx.recv().await {
            Ok(json) => json,
            Err(broadcast::error::RecvError::Lagged(n)) => {
                tracing::debug!("client lagged, skipping {n} snapshots");
                continue;
            }
            Err(broadcast::error::RecvError::Closed) => break,
        };

        // Skip to the newest snapshot already waiting. This is what bounds the
        // queue: whatever accumulated while the last send was in flight
        // collapses to a single message rather than becoming a backlog.
        let mut skipped: u64 = 0;
        loop {
            match rx.try_recv() {
                Ok(newer) => {
                    json = newer;
                    skipped += 1;
                }
                // The ring wrapped while we were busy; try again for whatever
                // survives, which is by definition newer than what we hold.
                Err(broadcast::error::TryRecvError::Lagged(n)) => skipped += n,
                Err(_) => break, // Empty, or Closed and handled on the next recv
            }
        }
        if skipped > 0 {
            tracing::trace!("skipped {skipped} superseded snapshots for a slow client");
        }

        if socket.send(Message::Text(json)).await.is_err() {
            break; // client actually disconnected
        }
    }
}

#[derive(Deserialize)]
struct AddTowerBody {
    lon: f64,
    lat: f64,
    #[serde(rename = "heightM")]
    height_m: f64,
}

async fn add_tower(State(state): State<AppState>, Json(body): Json<AddTowerBody>) -> impl IntoResponse {
    let _ = state.commands.send(Command::AddTower { lon: body.lon, lat: body.lat, height_m: body.height_m });
    axum::http::StatusCode::ACCEPTED
}

async fn remove_tower(State(state): State<AppState>, Path(id): Path<u32>) -> impl IntoResponse {
    let _ = state.commands.send(Command::RemoveTower { id });
    axum::http::StatusCode::ACCEPTED
}

#[derive(Deserialize)]
struct SetBalloonCountBody {
    n: u32,
}

async fn set_balloon_count(
    State(state): State<AppState>,
    Json(body): Json<SetBalloonCountBody>,
) -> impl IntoResponse {
    let _ = state.commands.send(Command::SetBalloonCount(body.n));
    axum::http::StatusCode::ACCEPTED
}

#[derive(Deserialize)]
struct SetHorizonCoeffBody {
    coeff: f64,
}

async fn set_horizon_coeff(
    State(state): State<AppState>,
    Json(body): Json<SetHorizonCoeffBody>,
) -> impl IntoResponse {
    let _ = state.commands.send(Command::SetHorizonRefractionCoeff(body.coeff));
    axum::http::StatusCode::ACCEPTED
}

#[derive(Deserialize)]
struct SetPausedBody {
    paused: bool,
}

async fn set_paused(State(state): State<AppState>, Json(body): Json<SetPausedBody>) -> impl IntoResponse {
    let _ = state.commands.send(Command::SetPaused(body.paused));
    axum::http::StatusCode::ACCEPTED
}

// The first *query* endpoint (§3/C4 of MESH_COMMS_DESIGN.md) — every handler
// above just fires a Command and returns 202. `World` lives entirely inside
// the sim task (see the `tokio::spawn` in `main`), so reading it means
// round-tripping a request through the same command channel and waiting on a
// oneshot for the answer.
async fn get_balloon_comms(State(state): State<AppState>, Path(id): Path<u32>) -> impl IntoResponse {
    let (respond_to, rx) = tokio::sync::oneshot::channel();
    if state.commands.send(Command::QueryBalloonComms { id, respond_to }).is_err() {
        return axum::http::StatusCode::INTERNAL_SERVER_ERROR.into_response();
    }
    match rx.await {
        Ok(Some(comms)) => Json(comms).into_response(),
        Ok(None) => axum::http::StatusCode::NOT_FOUND.into_response(),
        Err(_) => axum::http::StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    }
}
