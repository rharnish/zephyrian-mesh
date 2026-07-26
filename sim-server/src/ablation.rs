// Protocol ablation switches, for offline harnesses only.
//
// Unrelated to the top-level `experiments/` directory, which holds the
// connectivity parameter sweep and its results. These are in-process toggles
// that flip a *protocol rule*, not a parameter.
//
// They exist so bin/bundle_delivery.rs can run the same protocol code under a
// changed policy and compare, rather than editing a rule, eyeballing a number,
// and editing it back. Nothing in the live server ever sets them, so the shipped
// behaviour is whatever the `false` branch does.
//
// If an ablation wins convincingly it should stop being an ablation: fold it into
// the real rule and delete the switch. A permanent flag here is a smell.

use std::sync::atomic::{AtomicBool, Ordering};

/// Route on hop count first, freshness only as a tie-break — instead of the
/// shipped rule, which lets any fresher wave win regardless of distance.
/// See beacon.rs `should_adopt`.
///
/// **Settled: this ablation lost, and the hypothesis behind it was wrong.** It
/// existed to test whether freshness-first routing inflates path length, since
/// a balloon three hops from one tower can adopt a fifteen-hop belief from
/// another purely because that wave is newer. It doesn't: believed depth tracks
/// `bin/mesh_depth`'s omniscient median within a hop. Routing nearest-first
/// helps below the percolation threshold (32.1% → 39.3% completion) and *hurts*
/// above it (56.0% → 50.5%), which is where the mesh ships.
///
/// Kept rather than deleted, against the "a permanent flag here is a smell"
/// rule above, for one reason: the disproof is a measurement, not an argument,
/// and freshness-first looks wrong enough on inspection that it will be
/// re-proposed. Re-running `PREFER_NEARER=1 bundle_delivery` answers that in a
/// minute. Delete it once §4 of the design doc is trusted on its own.
static PREFER_NEARER: AtomicBool = AtomicBool::new(false);

pub fn prefer_nearer() -> bool {
    PREFER_NEARER.load(Ordering::Relaxed)
}

pub fn set_prefer_nearer(v: bool) {
    PREFER_NEARER.store(v, Ordering::Relaxed);
}
