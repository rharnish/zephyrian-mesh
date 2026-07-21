// ---------------------------------------------------------------------------
// Union-find (disjoint set) over string node keys, rebuilt fresh each tick.
// Used to find connected components (clusters) so we can tell which
// clusters include a tower (grounded) vs. balloon-only (ungrounded).
// ---------------------------------------------------------------------------
export class UnionFind {
  constructor() {
    this.parent = new Map();
  }

  makeSet(key) {
    if (!this.parent.has(key)) this.parent.set(key, key);
  }

  find(key) {
    this.makeSet(key);
    let root = key;
    while (this.parent.get(root) !== root) root = this.parent.get(root);
    // Path compression.
    let cur = key;
    while (this.parent.get(cur) !== root) {
      const next = this.parent.get(cur);
      this.parent.set(cur, root);
      cur = next;
    }
    return root;
  }

  union(a, b) {
    const rootA = this.find(a);
    const rootB = this.find(b);
    if (rootA !== rootB) this.parent.set(rootA, rootB);
  }
}
