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
use tokio::sync::{broadcast, mpsc};
use tower_http::cors::CorsLayer;

#[derive(Clone)]
struct AppState {
    commands: mpsc::UnboundedSender<Command>,
    snapshots: broadcast::Sender<String>,
}

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt::init();

    let wind = fetch_wind_field().await;

    let (command_tx, mut command_rx) = mpsc::unbounded_channel::<Command>();
    let (snapshot_tx, _) = broadcast::channel::<String>(16);

    let state = AppState { commands: command_tx, snapshots: snapshot_tx.clone() };

    // The single task that owns `World`. Everything else only talks to it
    // through `command_tx` (mutations) or `snapshot_tx` (read-only state).
    tokio::spawn(async move {
        let mut world = World::new(wind);
        world.spawn_balloons(config::DEFAULT_NUM_BALLOONS);
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

async fn ws_handler(ws: WebSocketUpgrade, State(state): State<AppState>) -> impl IntoResponse {
    ws.on_upgrade(move |socket| handle_socket(socket, state))
}

async fn handle_socket(mut socket: WebSocket, state: AppState) {
    let mut rx = state.snapshots.subscribe();
    while let Ok(json) = rx.recv().await {
        if socket.send(Message::Text(json)).await.is_err() {
            break; // client disconnected
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
