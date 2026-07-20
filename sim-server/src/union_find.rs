// Ported from cesium-app/src/unionFind.js. Union-find over string node keys,
// rebuilt fresh each tick.

use std::collections::HashMap;

#[derive(Default)]
pub struct UnionFind {
    parent: HashMap<String, String>,
}

impl UnionFind {
    pub fn new() -> Self {
        UnionFind { parent: HashMap::new() }
    }

    pub fn clear(&mut self) {
        self.parent.clear();
    }

    pub fn make_set(&mut self, key: &str) {
        self.parent.entry(key.to_string()).or_insert_with(|| key.to_string());
    }

    pub fn find(&mut self, key: &str) -> String {
        self.make_set(key);
        let mut root = key.to_string();
        while self.parent[&root] != root {
            root = self.parent[&root].clone();
        }
        // Path compression.
        let mut cur = key.to_string();
        while self.parent[&cur] != root {
            let next = self.parent[&cur].clone();
            self.parent.insert(cur, root.clone());
            cur = next;
        }
        root
    }

    pub fn union(&mut self, a: &str, b: &str) {
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
        uf.make_set("a");
        uf.make_set("b");
        uf.make_set("c");
        uf.union("a", "b");
        assert_eq!(uf.find("a"), uf.find("b"));
        assert_ne!(uf.find("a"), uf.find("c"));
        uf.union("b", "c");
        assert_eq!(uf.find("a"), uf.find("c"));
    }
}
