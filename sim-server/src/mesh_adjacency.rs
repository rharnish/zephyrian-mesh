// Who can hear whom, derived from the link-detection edge list.
//
// This is physical-layer fact, not protocol state: it says which radios are
// in range of which, which is true regardless of what protocol is running on
// top. It lives outside `protocol/` for that reason — every protocol needs it
// and none of them owns it.

use crate::link_detection::{Edge, NodeKey};
use crate::tower::Tower;
use std::collections::HashMap;

/// Who can hear whom. Rebuilt from the edge list whenever links are
/// recomputed. Balloon slots are indexed by position in the visible slice
/// (== balloon id, since ids are assigned sequentially and never reused);
/// tower slots by position in the tower vec, since tower ids *can* be removed.
#[derive(Default)]
pub struct MeshAdjacency {
    balloon_adj: Vec<Vec<u32>>,
    tower_adj: Vec<Vec<u32>>,
    /// A tower this balloon can currently hear directly, if any. Maintained
    /// alongside `tower_adj` so bundle forwarding can test "can I hand this
    /// straight to the ground?" without scanning every tower.
    balloon_tower: Vec<Option<u32>>,
}

impl MeshAdjacency {
    pub fn rebuild(&mut self, edges: &[Edge], n_balloons: usize, towers: &[Tower]) {
        self.balloon_adj.clear();
        self.balloon_adj.resize(n_balloons, Vec::new());
        self.tower_adj.clear();
        self.tower_adj.resize(towers.len(), Vec::new());
        self.balloon_tower.clear();
        self.balloon_tower.resize(n_balloons, None);

        let tower_slot: HashMap<u32, usize> =
            towers.iter().enumerate().map(|(i, t)| (t.id, i)).collect();

        for e in edges {
            match (e.a, e.b) {
                (NodeKey::Balloon(x), NodeKey::Balloon(y)) => {
                    // Balloon-to-balloon: symmetric, both directions.
                    if let Some(v) = self.balloon_adj.get_mut(x as usize) {
                        v.push(y);
                    }
                    if let Some(v) = self.balloon_adj.get_mut(y as usize) {
                        v.push(x);
                    }
                }
                // Tower-to-balloon is only ever used in the tower->balloon
                // direction: towers originate beacons, they don't relay them.
                (NodeKey::Balloon(b_id), NodeKey::Tower(t_id))
                | (NodeKey::Tower(t_id), NodeKey::Balloon(b_id)) => {
                    if let Some(&slot) = tower_slot.get(&t_id) {
                        self.tower_adj[slot].push(b_id);
                        if let Some(e) = self.balloon_tower.get_mut(b_id as usize) {
                            *e = Some(t_id);
                        }
                    }
                }
                (NodeKey::Tower(_), NodeKey::Tower(_)) => {}
            }
        }
    }

    /// Balloons in range of the tower at `slot` (index into the tower vec, not
    /// a tower id). One-directional by construction: towers originate, they
    /// don't relay.
    pub fn tower_neighbors(&self, slot: usize) -> &[u32] {
        self.tower_adj.get(slot).map_or(&[], |v| v.as_slice())
    }

    /// Balloons this one can currently hear. Empty if it has no live links.
    pub fn neighbors(&self, i: usize) -> &[u32] {
        self.balloon_adj.get(i).map_or(&[], |v| v.as_slice())
    }

    pub fn is_neighbor(&self, i: usize, id: u32) -> bool {
        self.neighbors(i).contains(&id)
    }

    /// A tower this balloon can hand a bundle straight to, if any.
    pub fn tower_in_range(&self, i: usize) -> Option<u32> {
        self.balloon_tower.get(i).copied().flatten()
    }
}
