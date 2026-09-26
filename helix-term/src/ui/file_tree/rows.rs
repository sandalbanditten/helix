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
    /// Whether this is the row a new entry's name is typed in rather than an entry.
    pub input: bool,
    /// Whether git ignores the entry, or a directory holding it.
    pub ignored: bool,
}

/// A row to type the name of a new entry in, shown among the entries of the directory `dir`
/// before the `at`th one.
#[derive(Debug, Clone, Copy)]
pub struct InputRow {
    pub dir: NodeId,
    pub at: usize,
}

#[derive(Debug, Default)]
pub struct Rows {
    rows: Vec<Row>,
    /// The row of every node that has one, including every directory of a run.
    index: HashMap<NodeId, usize>,
    input: Option<usize>,
}

impl Rows {
    /// Flattens the expanded part of `tree`. With `flatten_dirs` a run of single-child
    /// directories becomes one row.
    pub fn build(tree: &Tree, flatten_dirs: bool, input: Option<InputRow>) -> Self {
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
                input: false,
                ignored: false,
            }],
            index: HashMap::from([(root, 0)]),
            input: None,
        };
        rows.push_children(tree, 0, flatten_dirs, input);
        rows
    }

    fn push_children(
        &mut self,
        tree: &Tree,
        parent: usize,
        flatten_dirs: bool,
        input: Option<InputRow>,
    ) {
        let dir = self.rows[parent].node;
        let depth = if parent == 0 {
            0
        } else {
            self.rows[parent].depth + 1
        };
        let children = tree.children(dir);
        let input_at = input
            .filter(|input| input.dir == dir)
            .map(|input| input.at.min(children.len()));
        let len = children.len() + usize::from(input_at.is_some());
        for (i, &head) in children.iter().enumerate() {
            if input_at == Some(i) {
                self.push_input(parent, depth, false);
            }
            let i = i + usize::from(input_at.is_some_and(|at| at <= i));
            let index = self.rows.len();
            let mut node = head;
            let mut path = self.rows[parent].path.join(&tree.node(head).name);
            let mut label = display_name(&tree.node(head).name);
            let mut ignored = self.rows[parent].ignored || tree.node(head).ignored;
            self.index.insert(head, index);
            if flatten_dirs && tree.node(head).kind == Kind::Directory {
                while let Some(next) = tree.only_directory_child(node) {
                    node = next;
                    path.push(&tree.node(next).name);
                    label.push('/');
                    label.push_str(&display_name(&tree.node(next).name));
                    ignored |= tree.node(next).ignored;
                    self.index.insert(next, index);
                }
            }
            self.rows.push(Row {
                node,
                head,
                parent: Some(parent),
                depth,
                last: i + 1 == len,
                path,
                label_width: label.width(),
                label,
                input: false,
                ignored,
            });
            if tree.node(node).expanded {
                self.push_children(tree, index, flatten_dirs, input);
            }
        }
        if input_at == Some(children.len()) {
            self.push_input(parent, depth, true);
        }
    }

    fn push_input(&mut self, parent: usize, depth: usize, last: bool) {
        self.input = Some(self.rows.len());
        self.rows.push(Row {
            node: self.rows[parent].node,
            head: self.rows[parent].node,
            parent: Some(parent),
            depth,
            last,
            path: self.rows[parent].path.clone(),
            label: String::new(),
            label_width: 0,
            input: true,
            ignored: false,
        });
    }

    /// The index of the row for typing a new entry's name.
    pub fn input(&self) -> Option<usize> {
        self.input
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
    use super::super::tree::{
        tests::{dir, file, run, tree_with},
        Entry,
    };
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
        let rows = Rows::build(&tree, true, None);
        assert_eq!(
            labels(&rows),
            ["root", "docs", "src/main/java/app", "README.md"]
        );
        let app = tree.find("src/main/java/app".as_ref()).unwrap();
        let main = tree.find("src/main".as_ref()).unwrap();
        assert_eq!(rows[2].node, app);
        assert_eq!(rows[2].path, PathBuf::from("src/main/java/app"));
        assert_eq!(rows.index_of(main), Some(2));

        let rows = Rows::build(&tree, false, None);
        assert_eq!(labels(&rows), ["root", "docs", "src", "README.md"]);

        // Expanding the run shows the last directory's entries one level down.
        for path in ["src", "src/main", "src/main/java", "src/main/java/app"] {
            let id = tree.find(path.as_ref()).unwrap();
            tree.expand(id);
        }
        tree.apply_listing(app, Some(vec![file("Foo.java")]));
        let rows = Rows::build(&tree, true, None);
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
        let rows = Rows::build(&tree, true, None);
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
    fn the_input_row_sits_among_the_entries() {
        let mut tree = tree_with(vec![dir("a"), dir("b"), file("c")]);
        let root = tree.root();
        let labels = |rows: &Rows| -> Vec<(String, bool)> {
            rows.iter()
                .map(|row| (row.label.clone(), row.last))
                .collect()
        };
        let rows = Rows::build(&tree, true, Some(InputRow { dir: root, at: 2 }));
        assert_eq!(rows.input(), Some(3));
        assert_eq!(
            labels(&rows),
            [
                ("root".to_owned(), true),
                ("a".to_owned(), false),
                ("b".to_owned(), false),
                (String::new(), false),
                ("c".to_owned(), true),
            ]
        );
        let a = tree.find("a".as_ref()).unwrap();
        tree.expand(a);
        tree.apply_listing(a, Some(vec![]));
        let rows = Rows::build(&tree, true, Some(InputRow { dir: a, at: 0 }));
        assert_eq!(rows.input(), Some(2));
        assert!(rows[2].last);
        assert_eq!(rows[2].depth, 1);
        assert_eq!(rows.index_of(a), Some(1));
    }

    #[test]
    fn rows_below_an_ignored_directory_are_ignored() {
        let mut tree = tree_with(vec![
            Entry {
                ignored: true,
                ..dir("target")
            },
            file("lib.rs"),
        ]);
        let target = tree.find("target".as_ref()).unwrap();
        tree.expand(target);
        tree.apply_listing(target, Some(vec![file("hx")]));
        let rows = Rows::build(&tree, true, None);
        let ignored: Vec<_> = rows.iter().map(|row| (&*row.label, row.ignored)).collect();
        assert_eq!(
            ignored,
            [
                ("root", false),
                ("target", true),
                ("hx", true),
                ("lib.rs", false)
            ]
        );
    }

    #[test]
    fn control_characters_are_replaced() {
        let tree = tree_with(vec![file("evil\u{1b}[2Jname")]);
        let rows = Rows::build(&tree, true, None);
        assert_eq!(rows[1].label, "evil?[2Jname");
        assert_eq!(rows[1].path, PathBuf::from("evil\u{1b}[2Jname"));
    }
}
