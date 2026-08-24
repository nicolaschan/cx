use std::collections::BTreeMap;

pub struct Entry<'a> {
    pub path: &'a str,
    pub bytes: f64,
    pub lines: u64,
    pub delta: Option<f64>,
    pub marker: Option<String>,
}

pub struct Node {
    pub name: String,
    pub bytes: f64,
    pub lines: u64,
    pub delta: Option<f64>,
    pub marker: Option<String>,
    pub is_dir: bool,
    pub children: Vec<Node>,
    pub elided: Option<Elided>,
}

pub struct Elided {
    pub count: usize,
    pub bytes: f64,
    pub delta: Option<f64>,
}

pub fn breakdown<'a>(entries: impl IntoIterator<Item = Entry<'a>>, top: usize) -> Node {
    let mut root = Builder::default();
    for entry in entries {
        root.insert(entry.path.split('/'), entry);
    }

    let mut sizes = Vec::new();
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
    fn insert<'a>(&mut self, mut segments: impl Iterator<Item = &'a str>, entry: Entry<'a>) {
        self.bytes += entry.bytes;
        self.lines += entry.lines;
        self.delta_sum += entry.delta.unwrap_or(0.0);
        self.touched |= entry.delta.is_some();
        match segments.next() {
            None => {
                self.is_file = true;
                self.marker = entry.marker;
            }
            Some(seg) => self
                .children
                .entry(seg.to_owned())
                .or_default()
                .insert(segments, entry),
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
        let (kept, pruned): (Vec<_>, Vec<_>) = self
            .children
            .into_iter()
            .partition(|(_, child)| child.bytes >= threshold);
        let elided = (!pruned.is_empty()).then(|| Elided {
            count: pruned.len(),
            bytes: pruned.iter().map(|(_, c)| c.bytes).sum(),
            delta: pruned
                .iter()
                .any(|(_, c)| c.touched)
                .then(|| pruned.iter().map(|(_, c)| c.delta_sum).sum()),
        });
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
            elided,
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
        let elided = big.elided.as_ref().unwrap();
        assert_eq!((elided.count, elided.bytes), (2, 3.0));
        assert_eq!(
            root.elided.as_ref().unwrap().count,
            1,
            "small.rs elided at root"
        );
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
