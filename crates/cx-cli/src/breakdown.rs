//! dust-style hierarchical breakdown of per-file tree contributions:
//! aggregate bytes up the directory tree, keep only the globally biggest
//! nodes, elide the rest per directory. Display-side only — the JSON
//! contract stays the full flat file list.
//!
//! (dust's own renderer lives inside its binary, not a library; this
//! mirrors the approach rather than importing it.)

use std::collections::BTreeMap;

/// One path's inputs to the breakdown. `delta` is `Some` only for paths
/// the diff touched — `None` means "not part of the diff", which is
/// different from a net delta of zero.
pub struct Entry<'a> {
    pub path: &'a str,
    pub bytes: f64,
    pub lines: u64,
    pub delta: Option<f64>,
    /// Status indicator for this file ("+", "−", "→", …); files only.
    pub marker: Option<String>,
}

pub struct Node {
    pub name: String,
    pub bytes: f64,
    pub lines: u64,
    /// Sum of the deltas under this node; `None` when nothing under it
    /// was touched by the diff.
    pub delta: Option<f64>,
    /// The file's status indicator; never set on directories.
    pub marker: Option<String>,
    pub is_dir: bool,
    /// Children that survived pruning, sorted by bytes descending.
    pub children: Vec<Node>,
    /// Per-directory summary of pruned children.
    pub elided_count: usize,
    pub elided_bytes: f64,
    pub elided_delta: Option<f64>,
}

/// Build the aggregated tree and prune it to (roughly) the `top` biggest
/// nodes. Because a directory's bytes are the sum of its children's, any
/// kept node's ancestors are at least as big and therefore also kept —
/// the tree stays connected by construction. Ties at the threshold keep
/// slightly more than `top` rather than dropping arbitrarily.
pub fn breakdown<'a>(entries: impl IntoIterator<Item = Entry<'a>>, top: usize) -> Node {
    let mut root = Builder::default();
    for entry in entries {
        root.insert(
            entry.path.split('/'),
            entry.bytes,
            entry.lines,
            entry.delta,
            entry.marker,
        );
    }

    let mut sizes: Vec<f64> = Vec::new();
    root.collect_sizes(&mut sizes);
    sizes.sort_by(|a, b| b.total_cmp(a));
    let threshold = sizes.get(top.saturating_sub(1)).copied().unwrap_or(0.0);

    root.into_node(String::new(), threshold)
}

#[derive(Default)]
struct Builder {
    bytes: f64,
    lines: u64,
    delta_sum: f64,
    touched: bool,
    marker: Option<String>,
    children: BTreeMap<String, Builder>,
    is_file: bool,
}

impl Builder {
    fn insert<'a>(
        &mut self,
        mut segments: impl Iterator<Item = &'a str>,
        bytes: f64,
        lines: u64,
        delta: Option<f64>,
        marker: Option<String>,
    ) {
        self.bytes += bytes;
        self.lines += lines;
        self.delta_sum += delta.unwrap_or(0.0);
        self.touched |= delta.is_some();
        match segments.next() {
            None => {
                self.is_file = true;
                self.marker = marker;
            }
            Some(seg) => self
                .children
                .entry(seg.to_owned())
                .or_default()
                .insert(segments, bytes, lines, delta, marker),
        }
    }

    fn collect_sizes(&self, out: &mut Vec<f64>) {
        for child in self.children.values() {
            out.push(child.bytes);
            child.collect_sizes(out);
        }
    }

    fn into_node(self, name: String, threshold: f64) -> Node {
        let is_dir = !self.children.is_empty();
        let (kept, elided): (Vec<_>, Vec<_>) = self
            .children
            .into_iter()
            .partition(|(_, child)| child.bytes >= threshold);
        let elided_count = elided.len();
        let elided_bytes = elided.iter().map(|(_, c)| c.bytes).sum();
        let elided_touched = elided.iter().any(|(_, c)| c.touched);
        let elided_delta = elided_touched.then(|| elided.iter().map(|(_, c)| c.delta_sum).sum());
        let mut children: Vec<Node> = kept
            .into_iter()
            .map(|(name, child)| child.into_node(name, threshold))
            .collect();
        children.sort_by(|a, b| b.bytes.total_cmp(&a.bytes));
        Node {
            name,
            bytes: self.bytes,
            lines: self.lines,
            delta: self.touched.then_some(self.delta_sum),
            marker: self.marker,
            is_dir,
            children,
            elided_count,
            elided_bytes,
            elided_delta,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn file(path: &'static str, bytes: f64, lines: u64) -> Entry<'static> {
        Entry {
            path,
            bytes,
            lines,
            delta: None,
            marker: None,
        }
    }

    #[test]
    fn aggregates_directories() {
        let root = breakdown(
            [
                file("src/a.rs", 100.0, 10),
                file("src/b.rs", 50.0, 5),
                file("README.md", 25.0, 2),
            ],
            100,
        );
        assert_eq!(root.bytes, 175.0);
        assert_eq!(root.children.len(), 2);
        let src = &root.children[0];
        assert_eq!(
            (src.name.as_str(), src.bytes, src.lines),
            ("src", 150.0, 15)
        );
        assert!(src.is_dir);
        assert_eq!(src.children[0].name, "a.rs");
        assert!(!src.children[0].is_dir);
    }

    #[test]
    fn aggregates_deltas_only_where_touched() {
        let touched = |path, bytes, delta| Entry {
            path,
            bytes,
            lines: 1,
            delta: Some(delta),
            marker: None,
        };
        let root = breakdown(
            [
                touched("src/new.rs", 100.0, 80.0),
                file("src/old.rs", 50.0, 5),
                touched("src/gone.rs", 0.0, -40.0), // deleted: no tree bytes
                file("README.md", 25.0, 2),
            ],
            100,
        );
        let src = &root.children[0];
        assert_eq!(src.name, "src");
        assert_eq!(src.delta, Some(40.0), "dir sums deltas incl. deletions");
        assert_eq!(root.children[1].name, "README.md");
        assert_eq!(root.children[1].delta, None, "untouched stays None");
        let gone = src.children.iter().find(|c| c.name == "gone.rs").unwrap();
        assert_eq!((gone.bytes, gone.delta), (0.0, Some(-40.0)));
    }

    #[test]
    fn prunes_to_top_nodes_and_elides_the_rest() {
        let root = breakdown(
            [
                file("big/huge.rs", 1000.0, 1),
                file("big/tiny1.rs", 1.0, 1),
                file("big/tiny2.rs", 2.0, 1),
                file("small.rs", 5.0, 1),
            ],
            2, // keep the 2 biggest nodes: big/ (1003) and huge.rs (1000)
        );
        let big = &root.children[0];
        assert_eq!(big.name, "big");
        assert_eq!(big.children.len(), 1, "only huge.rs survives");
        assert_eq!(big.elided_count, 2);
        assert_eq!(big.elided_bytes, 3.0);
        assert_eq!(root.elided_count, 1, "small.rs elided at root");
    }

    #[test]
    fn ancestors_of_kept_nodes_are_kept() {
        let root = breakdown(
            [
                file("a/b/c/deep.rs", 500.0, 1),
                file("x.rs", 400.0, 1),
                file("y.rs", 1.0, 1),
            ],
            4, // a, a/b, a/b/c, deep.rs all ≥ threshold; x.rs ties are kept
        );
        let mut node = &root.children[0];
        for expected in ["a", "b", "c", "deep.rs"] {
            assert_eq!(node.name, expected);
            if expected != "deep.rs" {
                node = &node.children[0];
            }
        }
    }
}
