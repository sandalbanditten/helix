//! The rows the file tree shows: the expanded part of the [`Tree`] flattened in tree order.

use std::{collections::HashMap, ffi::OsStr, path::PathBuf};

use helix_core::unicode::width::UnicodeWidthStr;

use super::tree::{Kind, NodeId, Tree};

#[derive(Debug)]
pub struct Row {
    /// The entry the row stands for. For a single-child run this is its last directory.
    pub node: NodeId,
    /// The first directory of a single-child run, else `node`.
    pub head: NodeId,
    /// The row this one hangs from; `None` only for the root row.
    pub parent: Option<usize>,
    /// The number of rows between this one and the root row: its ancestor lanes.
    pub depth: usize,
    /// Whether no later row hangs from the same parent.
    pub last: bool,
    /// The path of `node` relative to the root.
    pub path: PathBuf,
    /// What the row reads: the entry's name, or the path of a run like `src/main/java`.
    pub label: String,
    /// The display width of `label`.
    pub label_width: usize,
}

#[derive(Debug, Default)]
pub struct Rows {
    rows: Vec<Row>,
    /// The row of every node that has one, including every directory of a run.
    index: HashMap<NodeId, usize>,
}

impl Rows {
    /// Flattens the expanded part of `tree`. With `flatten_dirs` a run of single-child
    /// directories becomes one row.
    pub fn build(tree: &Tree, flatten_dirs: bool) -> Self {
        let root = tree.root();
        let label = display_name(&tree.node(root).name);
        let mut rows = Self {
            rows: vec![Row {
                node: root,
                head: root,
                parent: None,
                depth: 0,
                last: true,
                path: PathBuf::new(),
                label_width: label.width(),
                label,
            }],
            index: HashMap::from([(root, 0)]),
        };
        rows.push_children(tree, 0, flatten_dirs);
        rows
    }

    fn push_children(&mut self, tree: &Tree, parent: usize, flatten_dirs: bool) {
        let dir = self.rows[parent].node;
        let depth = if parent == 0 {
            0
        } else {
            self.rows[parent].depth + 1
        };
        let children = tree.children(dir);
        for (i, &head) in children.iter().enumerate() {
            let index = self.rows.len();
            let mut node = head;
            let mut path = self.rows[parent].path.join(&tree.node(head).name);
            let mut label = display_name(&tree.node(head).name);
            self.index.insert(head, index);
            if flatten_dirs && tree.node(head).kind == Kind::Directory {
                while let Some(next) = tree.only_directory_child(node) {
                    node = next;
                    path.push(&tree.node(next).name);
                    label.push('/');
                    label.push_str(&display_name(&tree.node(next).name));
                    self.index.insert(next, index);
                }
            }
            self.rows.push(Row {
                node,
                head,
                parent: Some(parent),
                depth,
                last: i + 1 == children.len(),
                path,
                label_width: label.width(),
                label,
            });
            if tree.node(node).expanded {
                self.push_children(tree, index, flatten_dirs);
            }
        }
    }

    pub fn len(&self) -> usize {
        self.rows.len()
    }

    pub fn get(&self, index: usize) -> Option<&Row> {
        self.rows.get(index)
    }

    pub fn iter(&self) -> impl Iterator<Item = &Row> {
        self.rows.iter()
    }

    /// The row showing `node`.
    pub fn index_of(&self, node: NodeId) -> Option<usize> {
        self.index.get(&node).copied()
    }

    /// The rows `index` hangs from, outermost (the root row) first.
    pub fn ancestors(&self, index: usize) -> Vec<usize> {
        let mut ancestors = Vec::new();
        let mut current = self.rows[index].parent;
        while let Some(row) = current {
            ancestors.push(row);
            current = self.rows[row].parent;
        }
        ancestors.reverse();
        ancestors
    }

    /// For each ancestor lane of row `index`, outermost first: whether it continues below the
    /// row, i.e. whether that ancestor has later siblings.
    pub fn lanes(&self, index: usize) -> Vec<bool> {
        let mut lanes = Vec::with_capacity(self.rows[index].depth);
        let mut current = self.rows[index].parent;
        while let Some(row) = current.filter(|row| *row != 0) {
            lanes.push(!self.rows[row].last);
            current = self.rows[row].parent;
        }
        lanes.reverse();
        lanes
    }
}

impl std::ops::Index<usize> for Rows {
    type Output = Row;

    fn index(&self, index: usize) -> &Row {
        &self.rows[index]
    }
}

/// A file name as the tree shows it: lossily decoded, with control characters replaced so they
/// cannot corrupt the terminal.
fn display_name(name: &OsStr) -> String {
    name.to_string_lossy()
        .chars()
        .map(|c| if c.is_control() { '?' } else { c })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::super::tree::tests::{dir, file, run, tree_with};
    use super::*;

    fn labels(rows: &Rows) -> Vec<&str> {
        rows.iter().map(|row| row.label.as_str()).collect()
    }

    #[test]
    fn runs_become_one_row() {
        let mut tree = tree_with(vec![
            run("src", &["main", "java", "app"]),
            dir("docs"),
            file("README.md"),
        ]);
        let rows = Rows::build(&tree, true);
        assert_eq!(
            labels(&rows),
            ["root", "docs", "src/main/java/app", "README.md"]
        );
        let app = tree.find("src/main/java/app".as_ref()).unwrap();
        let main = tree.find("src/main".as_ref()).unwrap();
        assert_eq!(rows[2].node, app);
        assert_eq!(rows[2].path, PathBuf::from("src/main/java/app"));
        assert_eq!(rows.index_of(main), Some(2));

        let rows = Rows::build(&tree, false);
        assert_eq!(labels(&rows), ["root", "docs", "src", "README.md"]);

        // Expanding the run shows the last directory's entries one level down.
        for path in ["src", "src/main", "src/main/java", "src/main/java/app"] {
            let id = tree.find(path.as_ref()).unwrap();
            tree.expand(id);
        }
        tree.apply_listing(app, Some(vec![file("Foo.java")]));
        let rows = Rows::build(&tree, true);
        assert_eq!(
            labels(&rows),
            ["root", "docs", "src/main/java/app", "Foo.java", "README.md"]
        );
        assert_eq!(rows[3].depth, 1);
        assert_eq!(rows[3].path, PathBuf::from("src/main/java/app/Foo.java"));
    }

    #[test]
    fn guides_follow_presented_rows() {
        let mut tree = tree_with(vec![dir("a"), dir("b"), file("c")]);
        let a = tree.find("a".as_ref()).unwrap();
        tree.expand(a);
        tree.apply_listing(a, Some(vec![dir("x"), file("y")]));
        let x = tree.find("a/x".as_ref()).unwrap();
        tree.expand(x);
        tree.apply_listing(x, Some(vec![file("deep")]));
        let rows = Rows::build(&tree, true);
        assert_eq!(labels(&rows), ["root", "a", "x", "deep", "y", "b", "c"]);
        let lasts: Vec<_> = rows.iter().map(|row| row.last).collect();
        assert_eq!(lasts, [true, false, false, true, true, false, true]);
        // `deep` sits under `a` (which has later siblings) and `x` (which has `y` after it).
        assert_eq!(rows.lanes(3), [true, true]);
        assert_eq!(rows.lanes(4), [true]);
        assert_eq!(rows.ancestors(3), [0, 1, 2]);
        assert!(rows.lanes(5).is_empty());
    }

    #[test]
    fn control_characters_are_replaced() {
        let tree = tree_with(vec![file("evil\u{1b}[2Jname")]);
        let rows = Rows::build(&tree, true);
        assert_eq!(rows[1].label, "evil?[2Jname");
        assert_eq!(rows[1].path, PathBuf::from("evil\u{1b}[2Jname"));
    }
}
