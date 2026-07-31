// Ported from cesium-app/src/unionFind.js. Union-find over node keys,
// rebuilt fresh each tick. Keyed by the Copy `NodeKey` enum rather than
// strings — see NodeKey's doc comment in link_detection.rs.

use crate::link_detection::NodeKey;
use std::collections::HashMap;

#[derive(Default)]
pub struct UnionFind {
    parent: HashMap<NodeKey, NodeKey>,
}

impl UnionFind {
    pub fn new() -> Self {
        UnionFind { parent: HashMap::new() }
    }

    pub fn clear(&mut self) {
        self.parent.clear();
    }

    pub fn make_set(&mut self, key: NodeKey) {
        self.parent.entry(key).or_insert(key);
    }

    pub fn find(&mut self, key: NodeKey) -> NodeKey {
        self.make_set(key);
        let mut root = key;
        while self.parent[&root] != root {
            root = self.parent[&root];
        }
        // Path compression.
        let mut cur = key;
        while self.parent[&cur] != root {
            let next = self.parent[&cur];
            self.parent.insert(cur, root);
            cur = next;
        }
        root
    }

    pub fn union(&mut self, a: NodeKey, b: NodeKey) {
        let root_a = self.find(a);
        let root_b = self.find(b);
        if root_a != root_b {
            self.parent.insert(root_a, root_b);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unions_and_finds() {
        let mut uf = UnionFind::new();
        let a = NodeKey::Balloon(0);
        let b = NodeKey::Balloon(1);
        let c = NodeKey::Balloon(2);
        uf.make_set(a);
        uf.make_set(b);
        uf.make_set(c);
        uf.union(a, b);
        assert_eq!(uf.find(a), uf.find(b));
        assert_ne!(uf.find(a), uf.find(c));
        uf.union(b, c);
        assert_eq!(uf.find(a), uf.find(c));
    }
}
