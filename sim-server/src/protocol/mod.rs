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

use crate::balloon::Balloon;
use crate::link_detection::NodeKey;
use crate::mesh_adjacency::MeshAdjacency;
use crate::telemetry::TelemetryRecord;
use crate::tower::Tower;
use rand::RngCore;

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

    // --- Lifecycle ----------------------------------------------------------
    fn clear_nodes(&mut self);
    /// Add state for one newly spawned balloon. Called inside the spawn loop,
    /// so any RNG draws here interleave with the balloon's own — see
    /// `World::spawn_balloon_pool`.
    fn spawn_node(&mut self, rng: &mut dyn RngCore);
    fn add_tower(&mut self, id: u32);
    fn remove_tower(&mut self, id: u32);

    // --- The round ----------------------------------------------------------
    fn step(&mut self, ctx: StepCtx<'_>, rng: &mut dyn RngCore) -> Vec<CommsEvent>;

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
        }
    }
}

impl std::str::FromStr for ProtocolSpec {
    type Err = String;

    /// Bare name selects a protocol with its default parameters; parameter
    /// overrides are deliberately not parsed here yet, since nothing needs
    /// them from a command line and a half-built syntax is worse than none.
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "dv-dtn" => Ok(ProtocolSpec::DvDtn(Default::default())),
            other => Err(format!("unknown protocol {other:?} (known: dv-dtn)")),
        }
    }
}
