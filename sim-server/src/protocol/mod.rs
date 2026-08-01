// The seam between the simulation and whatever comms protocol is running on
// top of it.
//
// `World` owns physics, topology, and ground truth; a `MeshProtocol` owns
// everything about how balloons discover routes and move data, including all
// of its own per-node state. The two communicate through this module and
// nowhere else.
//
// The constraint that shaped this: the abstraction must fit protocols that
// have no routing table at all. Epidemic/spray-and-wait replicates a bundle to
// every contact and keeps a copy count rather than a next hop; gossip keeps a
// partial topology map. So there is deliberately no "route" or "next hop" in
// the trait — a protocol publishes a `route_hops` only if the notion means
// something to it, and `None` is a legitimate permanent answer, not a
// not-yet-known.
//
// What is *not* here, on purpose: MeshAdjacency, link detection, union-find
// and `grounded` are physical/topological facts shared by every protocol, and
// `Channel { radio | satellite }` is a link-layer fact rather than a routing
// choice. Those stay outside.

pub mod dv_dtn;
pub mod epidemic;
pub mod stats;

use crate::balloon::Balloon;
use crate::link_detection::NodeKey;
use crate::mesh_adjacency::MeshAdjacency;
use crate::telemetry::TelemetryRecord;
use crate::tower::Tower;

/// Everything a protocol may read to advance one comms round. Read-only: a
/// protocol may not move a balloon or change the topology, which is enforced
/// here by the types rather than by convention.
pub struct StepCtx<'a> {
    pub round: u64,
    /// The visible slice. Index `i` is the same node as the protocol's own
    /// node `i` — the correspondence `MeshAdjacency` already relies on.
    pub balloons: &'a [Balloon],
    pub towers: &'a [Tower],
    pub adj: &'a MeshAdjacency,
}

/// What kind of transmission an event was, so the frontend can style it
/// without knowing which protocol produced it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub enum EventKind {
    /// An unsolicited route advertisement — the shipped protocol's tower
    /// beacon, or any periodic proactive equivalent.
    RouteAd,
    /// On-demand discovery (AODV-style): a request flooding outward...
    RouteRequest,
    /// ...and the reply coming back.
    RouteReply,
    /// Payload moving one hop.
    Bundle,
    /// A receipt moving one hop.
    Ack,
    /// A neighbour-table or digest exchange.
    Gossip,
}

/// One transmission that actually happened this round, for the frontend's
/// wavefront animation. Emitting these is optional — a protocol that has
/// nothing beacon-like simply returns fewer kinds.
#[derive(Debug, Clone)]
pub struct CommsEvent {
    pub kind: EventKind,
    pub from: NodeKey,
    pub to: NodeKey,
    /// How many records/bundles rode this one transmission. Normally 1; higher
    /// once batching is in play, so a batched hop can be drawn as one heavier
    /// event rather than N identical ones.
    pub payload: u32,
    /// Which tower's wave this belongs to, where that means anything. `None`
    /// under protocols with no tower-rooted routing.
    pub tower_id: Option<u32>,
    pub hop_count: Option<u32>,
    /// Monotonic per-wave counter, for deduplicating a replayed wave.
    pub epoch: Option<u64>,
}

/// What a protocol can and cannot express, so the frontend can hide UI that
/// would otherwise show meaningless values rather than no values.
///
/// This exists because the interesting protocols genuinely disagree about
/// which concepts exist. Epidemic has no route belief, so a belief-vs-truth
/// overlay under it is not "empty" — it is a category error. Declaring
/// capabilities lets the UI say nothing rather than say something false.
#[derive(Debug, Clone, Copy, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Capabilities {
    /// Stable identifier, matching `ProtocolSpec`'s parse name.
    pub name: &'static str,
    /// Human-readable, for the controls panel.
    pub label: &'static str,
    /// Nodes hold a believed route with a hop count. Gates the belief overlay,
    /// the belief/truth mesh-health readouts, and the inspector's
    /// Believes/Verdict rows.
    pub route_belief: bool,
    /// A delivered payload has one recorded path that can be replayed hop by
    /// hop. False for anything replication-based, where "the path" isn't a
    /// single thing.
    pub next_hop_paths: bool,
    /// Receipts travel back to the origin, so "delivered but unacknowledged"
    /// is a state that can exist.
    pub acks: bool,
    /// Payloads that age out leave via satellite rather than being dropped.
    pub satellite_fallback: bool,
    /// Which event kinds this protocol actually emits, so the animation
    /// legend can list only what will appear.
    pub event_kinds: &'static [EventKind],
}

/// The protocol's published per-node view, copied onto the wire each tick.
/// Deliberately small: anything richer is answered on demand per balloon.
#[derive(Debug, Clone, Copy, Default)]
pub struct NodeCommsView {
    /// Hops to a tower as this node believes. `None` means "no route known",
    /// and under a protocol without routes it is always `None`.
    pub route_hops: Option<u32>,
    /// How this node's most recent resolved payload actually got through —
    /// server truth, not something the node itself could know.
    pub last_channel: Option<crate::protocol::dv_dtn::bundle::Channel>,
}

/// A protocol's own most-recently-resolved payload for one node, for the
/// inspector's replay view. Shaped around a single recorded path, which is
/// what a unicast protocol produces; replication-based protocols return
/// `None` until there is a generalization worth making.
pub type LastBundleView = crate::sim::LastBundleView;

pub trait MeshProtocol: Send {
    /// Stable identifier, for logs and sweep output.
    fn spec_name(&self) -> &'static str;
    /// What this protocol can express — see `Capabilities`.
    fn capabilities(&self) -> &'static Capabilities;

    // --- Lifecycle ----------------------------------------------------------
    /// Pin the protocol's own randomness. Deliberately a *separate* stream
    /// from the world's: a protocol that draws more or fewer numbers than
    /// another would otherwise shift every subsequent physics draw, so two
    /// protocols run at "the same seed" would not even see the same balloon
    /// field — confounding the comparison this whole seam exists to enable.
    fn reseed(&mut self, seed: u64);
    fn clear_nodes(&mut self);
    /// Add state for one newly spawned balloon.
    fn spawn_node(&mut self);
    fn add_tower(&mut self, id: u32);
    fn remove_tower(&mut self, id: u32);

    // --- The round ----------------------------------------------------------
    fn step(&mut self, ctx: StepCtx<'_>) -> Vec<CommsEvent>;

    // --- Published state ----------------------------------------------------
    fn node_view(&self, i: usize) -> NodeCommsView;
    /// Payloads currently held across the visible slice.
    fn carrying(&self, visible: usize) -> u64;
    /// Payloads held by a node that currently has nowhere to send them —
    /// waiting rather than lost.
    fn stranded(&self, visible: usize) -> u64;
    fn delivered(&self) -> u64;
    /// Everything that has left circulation one way or another.
    fn resolved(&self) -> u64;
    /// This protocol's counters, keyed, for consumers that don't know which
    /// protocol they are driving. See `stats::StatsTable` for the handful of
    /// key names that are expected to mean the same thing everywhere.
    fn stats(&self) -> stats::StatsTable;

    // --- On-demand detail, for GET /api/balloons/:id/comms ------------------
    fn last_bundle(&self, i: usize) -> Option<LastBundleView>;
    fn log(&self, i: usize) -> Vec<TelemetryRecord>;

    // --- Offline harness support -------------------------------------------
    /// Stop all origination so what is already in the mesh can be watched to
    /// completion. Used by the drain-phase conservation checks; the live
    /// server never calls it.
    fn halt_origination(&mut self);
    /// Escape hatch for harnesses that legitimately need protocol internals
    /// (see bin/telemetry_records.rs). Not for use on the serving path.
    fn as_any(&self) -> &dyn std::any::Any;
}

/// Which protocol a `World` should run, chosen at construction.
///
/// Parsed from a string so sweep binaries can take it on the command line and
/// iterate over several in one run without a match arm per family.
#[derive(Debug, Clone, PartialEq)]
pub enum ProtocolSpec {
    DvDtn(dv_dtn::params::DvDtnParams),
    Epidemic(epidemic::params::EpidemicParams),
}

impl Default for ProtocolSpec {
    fn default() -> Self {
        ProtocolSpec::DvDtn(Default::default())
    }
}

impl ProtocolSpec {
    pub fn build(&self) -> Box<dyn MeshProtocol> {
        match self {
            ProtocolSpec::DvDtn(p) => Box::new(dv_dtn::DvDtn::with_params(*p)),
            ProtocolSpec::Epidemic(p) => Box::new(epidemic::Epidemic::with_params(*p)),
        }
    }
}

impl std::str::FromStr for ProtocolSpec {
    type Err = String;

    /// `name` for defaults, or `name:key=value,key=value` to override
    /// parameters — so a live server or a harness can be pointed at a variant
    /// without a rebuild.
    ///
    /// ```text
    /// dv-dtn
    /// dv-dtn:ack=digest
    /// dv-dtn:ack=digest,mesh=4,tower=4
    /// dv-dtn:metric=nearest,queue=lifo
    /// ```
    ///
    /// Only the parameters worth varying from outside are exposed; the rest
    /// are measured constants that want a code change and a comment, not a
    /// command line.
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        use dv_dtn::params::{
            AckPolicy, Discovery, DvDtnParams, Metric, QueueDiscipline, ReplyPolicy,
        };

        let (name, rest) = match s.split_once(':') {
            Some((n, r)) => (n, Some(r)),
            None => (s, None),
        };
        if name == "epidemic" {
            return parse_epidemic(rest.unwrap_or(""));
        }
        if name != "dv-dtn" {
            return Err(format!("unknown protocol {name:?} (known: dv-dtn, epidemic)"));
        }

        let mut p = DvDtnParams::default();
        for pair in rest.unwrap_or("").split(',').filter(|x| !x.is_empty()) {
            let (k, v) = pair
                .split_once('=')
                .ok_or_else(|| format!("expected key=value, got {pair:?}"))?;
            let num = || -> Result<usize, String> {
                v.parse::<usize>().map_err(|_| format!("{k}: expected a number, got {v:?}"))
            };
            match k {
                "metric" => {
                    p.metric = match v {
                        "freshest" => Metric::FreshestFirst,
                        "nearest" => Metric::NearestFirst,
                        _ => return Err(format!("metric: expected freshest|nearest, got {v:?}")),
                    }
                }
                "queue" => {
                    p.queue_discipline = match v {
                        "fifo" => QueueDiscipline::Fifo,
                        "lifo" => QueueDiscipline::Lifo,
                        _ => return Err(format!("queue: expected fifo|lifo, got {v:?}")),
                    }
                }
                "ack" => {
                    p.ack_policy = match v {
                        "source-routed" | "source" => AckPolicy::SourceRouted,
                        "digest" => AckPolicy::Digest,
                        _ => return Err(format!("ack: expected source-routed|digest, got {v:?}")),
                    }
                }
                "discovery" => {
                    p.discovery = match v {
                        "proactive" => Discovery::Proactive,
                        "reactive" => Discovery::Reactive,
                        _ => {
                            return Err(format!(
                                "discovery: expected proactive|reactive, got {v:?}"
                            ))
                        }
                    }
                }
                "reply" => {
                    p.reply_policy = match v {
                        "intermediate" | "any" => ReplyPolicy::Intermediate,
                        "tower" | "tower-adjacent" => ReplyPolicy::TowerAdjacent,
                        _ => {
                            return Err(format!(
                                "reply: expected intermediate|tower, got {v:?}"
                            ))
                        }
                    }
                }
                "mesh" => p.batch.mesh_hop = num()?.max(1),
                "tower" => p.batch.tower_contact = num()?.max(1),
                "digest_entries" => p.ack_digest_entries = num()?,
                "originate" => p.bundle_interval_rounds = num()? as u64,
                other => {
                    return Err(format!(
                        "unknown parameter {other:?} (known: metric, queue, ack, mesh, tower, \
                         digest_entries, originate, discovery, reply)"
                    ))
                }
            }
        }
        // The digest rides tower beacons. Reactive discovery doesn't send any,
        // so the combination would silently never acknowledge anything —
        // which would look like a protocol result rather than a missing
        // mechanism. Refuse it instead.
        if p.discovery == Discovery::Reactive && p.ack_policy == AckPolicy::Digest {
            return Err(
                "ack=digest needs tower beacons to ride on, which discovery=reactive \
                 does not send; use ack=source-routed with reactive discovery"
                    .to_string(),
            );
        }
        Ok(ProtocolSpec::DvDtn(p))
    }
}

fn parse_epidemic(rest: &str) -> Result<ProtocolSpec, String> {
    use epidemic::params::EpidemicParams;

    let mut p = EpidemicParams::default();
    for pair in rest.split(',').filter(|x| !x.is_empty()) {
        let (k, v) =
            pair.split_once('=').ok_or_else(|| format!("expected key=value, got {pair:?}"))?;
        let num = || -> Result<u64, String> {
            v.parse::<u64>().map_err(|_| format!("{k}: expected a number, got {v:?}"))
        };
        match k {
            "copies" => p.copies = num()?.max(1) as u32,
            "tower" => p.tower_contact = num()?.max(1) as usize,
            "originate" => p.bundle_interval_rounds = num()?.max(1),
            "wake" => p.wake_interval_rounds = num()?.max(1),
            other => {
                return Err(format!(
                    "unknown parameter {other:?} for epidemic \
                     (known: copies, tower, originate, wake)"
                ))
            }
        }
    }
    Ok(ProtocolSpec::Epidemic(p))
}
