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
static PREFER_NEARER: AtomicBool = AtomicBool::new(false);

pub fn prefer_nearer() -> bool {
    PREFER_NEARER.load(Ordering::Relaxed)
}

pub fn set_prefer_nearer(v: bool) {
    PREFER_NEARER.store(v, Ordering::Relaxed);
}
