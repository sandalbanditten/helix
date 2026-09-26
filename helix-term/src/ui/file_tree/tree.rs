//! The entries the file tree knows about and which directories are expanded.
//!
//! Directories are listed in the background and merged in with [`Tree::apply_listing`]. A
//! collapsed directory holds nothing but its single-child run (see [`Entry::only_child`]), so a
//! run of single-child directories can be shown as one row before it is expanded. Merging keeps
//! the [`NodeId`] of every entry that survives, so the cursor and expansion survive refreshes.

use std::{
    collections::{HashMap, HashSet},
    ffi::{OsStr, OsString},
    mem,
    path::{Component, Path, PathBuf},
};

use helix_view::editor::FileTreeSort;
use slotmap::{new_key_type, SlotMap};

use super::order::{entry_cmp, Group};

new_key_type! {
    pub struct NodeId;
}

/// What a directory entry is. Links are never followed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Directory,
    File { executable: bool },
    Link(LinkTarget),
    Special(Special),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LinkTarget {
    File,
    Directory,
    /// The target is missing, unreadable or neither a file nor a directory.
    Broken,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Special {
    Fifo,
    Socket,
    BlockDevice,
    CharDevice,
}

impl Kind {
    pub fn group(self) -> Group {
        match self {
            Self::Directory | Self::Link(LinkTarget::Directory) => Group::Directory,
            Self::File { .. } | Self::Link(LinkTarget::File) => Group::File,
            Self::Link(LinkTarget::Broken) | Self::Special(_) => Group::Other,
        }
    }

    /// Whether the entry can be opened in a buffer.
    pub fn is_file(self) -> bool {
        matches!(self, Self::File { .. } | Self::Link(LinkTarget::File))
    }
}

#[derive(Debug)]
pub enum Children {
    /// Not known yet: a collapsed directory, or a listing that has not arrived.
    Unloaded,
    /// Every entry of the directory, in tree order.
    Loaded(Vec<NodeId>),
    Unreadable,
}

#[derive(Debug)]
pub struct Node {
    pub name: OsString,
    pub kind: Kind,
    pub parent: Option<NodeId>,
    pub children: Children,
    pub expanded: bool,
    /// Whether `children` come from a listing of this directory rather than a probe.
    listed: bool,
}

/// One entry of a directory listing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    pub name: OsString,
    pub kind: Kind,
    /// For a directory whose only entry is a directory: that directory, probed the same way.
    pub only_child: Option<Box<Entry>>,
}

impl Entry {
    pub fn new(name: impl Into<OsString>, kind: Kind) -> Self {
        Self {
            name: name.into(),
            kind,
            only_child: None,
        }
    }
}

/// The entries of a directory, or `None` if it could not be read.
pub type Listing = Option<Vec<Entry>>;

pub struct Tree {
    nodes: SlotMap<NodeId, Node>,
    root: NodeId,
    sort: FileTreeSort,
    /// Directories that were expanded since the last [`Tree::take_listing_requests`].
    listing_requests: Vec<NodeId>,
}

impl Tree {
    /// A tree holding only its (expanded, unlisted) root, named `root_name`.
    pub fn new(root_name: OsString, sort: FileTreeSort) -> Self {
        let mut nodes = SlotMap::with_key();
        let root = nodes.insert(Node {
            name: root_name,
            kind: Kind::Directory,
            parent: None,
            children: Children::Unloaded,
            expanded: true,
            listed: false,
        });
        Self {
            nodes,
            root,
            sort,
            listing_requests: vec![root],
        }
    }

    pub fn root(&self) -> NodeId {
        self.root
    }

    pub fn get(&self, id: NodeId) -> Option<&Node> {
        self.nodes.get(id)
    }

    /// The node `id`, which must exist.
    pub fn node(&self, id: NodeId) -> &Node {
        &self.nodes[id]
    }

    pub fn contains(&self, id: NodeId) -> bool {
        self.nodes.contains_key(id)
    }

    /// The listed children of `id`, in tree order.
    pub fn children(&self, id: NodeId) -> &[NodeId] {
        match &self.nodes[id].children {
            Children::Loaded(children) => children,
            Children::Unloaded | Children::Unreadable => &[],
        }
    }

    /// The only child of `id` if that is a directory: the next step of a single-child run.
    pub fn only_directory_child(&self, id: NodeId) -> Option<NodeId> {
        match self.children(id) {
            [child] if self.nodes[*child].kind == Kind::Directory => Some(*child),
            _ => None,
        }
    }

    /// The path of `id` relative to the root; empty for the root itself.
    pub fn path(&self, id: NodeId) -> PathBuf {
        let mut names = Vec::new();
        let mut current = id;
        while let Some(parent) = self.nodes[current].parent {
            names.push(self.nodes[current].name.as_os_str());
            current = parent;
        }
        names.iter().rev().collect()
    }

    /// The node at `path`, relative to the root.
    pub fn find(&self, path: &Path) -> Option<NodeId> {
        let mut current = self.root;
        for component in path.components() {
            let Component::Normal(name) = component else {
                return None;
            };
            current = self.child_named(current, name)?;
        }
        Some(current)
    }

    fn child_named(&self, id: NodeId, name: &OsStr) -> Option<NodeId> {
        self.children(id)
            .iter()
            .copied()
            .find(|child| self.nodes[*child].name == name)
    }

    /// Whether `id` is `ancestor` or lies below it.
    pub fn is_within(&self, id: NodeId, ancestor: NodeId) -> bool {
        let mut current = Some(id);
        while let Some(node) = current {
            if node == ancestor {
                return true;
            }
            current = self.nodes[node].parent;
        }
        false
    }

    /// The directories that were expanded since the last call and so need a (fresh) listing.
    pub fn take_listing_requests(&mut self) -> Vec<NodeId> {
        let mut requests = mem::take(&mut self.listing_requests);
        let mut seen = HashSet::new();
        requests
            .retain(|id| seen.insert(*id) && self.nodes.get(*id).is_some_and(|node| node.expanded));
        requests
    }

    /// Every directory whose entries are loaded, i.e. the directories worth watching.
    pub fn loaded_directories(&self) -> impl Iterator<Item = NodeId> + '_ {
        self.nodes
            .iter()
            .filter(|(_, node)| matches!(node.children, Children::Loaded(_)))
            .map(|(id, _)| id)
    }

    pub fn set_sort(&mut self, sort: FileTreeSort) {
        if self.sort == sort {
            return;
        }
        self.sort = sort;
        let ids: Vec<_> = self.nodes.keys().collect();
        for id in ids {
            if let Children::Loaded(mut children) =
                mem::replace(&mut self.nodes[id].children, Children::Unloaded)
            {
                self.sort_children(&mut children);
                self.nodes[id].children = Children::Loaded(children);
            }
        }
    }

    fn sort_children(&self, children: &mut [NodeId]) {
        let key = |id: &NodeId| {
            let node = &self.nodes[*id];
            (node.name.to_string_lossy(), node.kind.group())
        };
        let cmp = |a: &NodeId, b: &NodeId| {
            let (a, a_group) = key(a);
            let (b, b_group) = key(b);
            entry_cmp(self.sort, (&a, a_group), (&b, b_group))
        };
        if !children.is_sorted_by(|a, b| cmp(a, b).is_le()) {
            children.sort_by(cmp);
        }
    }

    /// Merges a listing of the directory `dir`. Entries that are still present keep their ids.
    pub fn apply_listing(&mut self, dir: NodeId, listing: Listing) {
        let old = mem::replace(&mut self.nodes[dir].children, Children::Unloaded);
        let old = match old {
            Children::Loaded(children) => children,
            Children::Unloaded | Children::Unreadable => Vec::new(),
        };
        let Some(entries) = listing else {
            for child in old {
                self.remove_subtree(child);
            }
            self.nodes[dir].children = Children::Unreadable;
            self.nodes[dir].listed = true;
            return;
        };

        let mut old: HashMap<OsString, NodeId> = old
            .into_iter()
            .map(|id| (self.nodes[id].name.clone(), id))
            .collect();
        let mut children = Vec::with_capacity(entries.len());
        for entry in entries {
            let is_directory = entry.kind == Kind::Directory;
            let id = match old.remove(&entry.name) {
                Some(id) if (self.nodes[id].kind == Kind::Directory) == is_directory => id,
                Some(id) => {
                    self.remove_subtree(id);
                    self.insert(dir, &entry)
                }
                None => self.insert(dir, &entry),
            };
            self.nodes[id].kind = entry.kind;
            if is_directory && !self.nodes[id].expanded {
                self.set_run(id, entry.only_child.as_deref());
            }
            children.push(id);
        }
        for id in old.into_values() {
            self.remove_subtree(id);
        }
        self.sort_children(&mut children);
        self.nodes[dir].children = Children::Loaded(children);
        self.nodes[dir].listed = true;
    }

    fn insert(&mut self, parent: NodeId, entry: &Entry) -> NodeId {
        self.nodes.insert(Node {
            name: entry.name.clone(),
            kind: entry.kind,
            parent: Some(parent),
            children: Children::Unloaded,
            expanded: false,
            listed: false,
        })
    }

    /// Makes the collapsed directory `dir` hold exactly its probed single-child run.
    fn set_run(&mut self, dir: NodeId, only_child: Option<&Entry>) {
        self.nodes[dir].listed = false;
        let old = mem::replace(&mut self.nodes[dir].children, Children::Unloaded);
        let mut old = match old {
            Children::Loaded(children) => children,
            Children::Unloaded | Children::Unreadable => Vec::new(),
        };
        let Some(entry) = only_child else {
            for child in old {
                self.remove_subtree(child);
            }
            return;
        };
        let reused = match old.as_slice() {
            [child]
                if self.nodes[*child].name == entry.name
                    && self.nodes[*child].kind == Kind::Directory =>
            {
                old.pop()
            }
            _ => None,
        };
        for child in old {
            self.remove_subtree(child);
        }
        let child = reused.unwrap_or_else(|| self.insert(dir, entry));
        self.nodes[child].expanded = false;
        self.set_run(child, entry.only_child.as_deref());
        self.nodes[dir].children = Children::Loaded(vec![child]);
    }

    fn remove_subtree(&mut self, id: NodeId) {
        let mut stack = vec![id];
        while let Some(id) = stack.pop() {
            if let Some(node) = self.nodes.remove(id) {
                if let Children::Loaded(children) = node.children {
                    stack.extend(children);
                }
            }
        }
    }

    /// Expands the directory `id`. Unless it has been listed, it is requested by
    /// [`take_listing_requests`](Self::take_listing_requests).
    pub fn expand(&mut self, id: NodeId) {
        let node = &mut self.nodes[id];
        if node.kind == Kind::Directory && !node.expanded {
            node.expanded = true;
            if !node.listed {
                self.listing_requests.push(id);
            }
        }
    }

    /// Collapses the directory `id` and everything below it, forgetting all entries below it but
    /// its single-child run.
    pub fn collapse(&mut self, id: NodeId) {
        if id == self.root {
            return;
        }
        self.nodes[id].expanded = false;
        self.nodes[id].listed = false;
        match mem::replace(&mut self.nodes[id].children, Children::Unloaded) {
            Children::Loaded(children) => {
                if let [child] = children[..] {
                    if self.nodes[child].kind == Kind::Directory {
                        self.nodes[id].children = Children::Loaded(children);
                        self.collapse(child);
                        return;
                    }
                }
                for child in children {
                    self.remove_subtree(child);
                }
            }
            children @ Children::Unreadable => self.nodes[id].children = children,
            Children::Unloaded => {}
        }
    }

    /// Expands the directories leading to `path` as far as they are listed. Returns the node at
    /// `path`, or where the search stopped: [`Reveal::Unlisted`] means the listing of that
    /// directory has to arrive first.
    pub fn reveal(&mut self, path: &Path) -> Reveal {
        let mut current = self.root;
        for component in path.components() {
            let Component::Normal(name) = component else {
                return Reveal::Missing;
            };
            if self.nodes[current].kind != Kind::Directory {
                return Reveal::Missing;
            }
            self.expand(current);
            current = match &self.nodes[current].children {
                Children::Unloaded => return Reveal::Unlisted(current),
                Children::Unreadable => return Reveal::Missing,
                Children::Loaded(_) => match self.child_named(current, name) {
                    Some(child) => child,
                    None => return Reveal::Missing,
                },
            };
        }
        Reveal::Found(current)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Reveal {
    Found(NodeId),
    Unlisted(NodeId),
    Missing,
}

#[cfg(test)]
pub(super) mod tests {
    use super::*;

    pub const FILE: Kind = Kind::File { executable: false };

    pub fn file(name: &str) -> Entry {
        Entry::new(name, FILE)
    }

    pub fn dir(name: &str) -> Entry {
        Entry::new(name, Kind::Directory)
    }

    /// A directory whose only entries are the directories `run`, nested in that order.
    pub fn run(name: &str, run: &[&str]) -> Entry {
        let only_child = run.iter().rev().fold(None, |only_child, name| {
            Some(Box::new(Entry {
                only_child,
                ..dir(name)
            }))
        });
        Entry {
            only_child,
            ..dir(name)
        }
    }

    pub fn names(tree: &Tree, id: NodeId) -> Vec<String> {
        tree.children(id)
            .iter()
            .map(|id| tree.node(*id).name.to_string_lossy().into_owned())
            .collect()
    }

    pub fn tree_with(entries: Vec<Entry>) -> Tree {
        let mut tree = Tree::new("root".into(), FileTreeSort::DirectoriesFirst);
        let root = tree.root();
        tree.apply_listing(root, Some(entries));
        tree
    }

    #[test]
    fn listings_are_sorted_and_keep_ids() {
        let mut tree = tree_with(vec![file("b.txt"), dir("src"), file("a.txt")]);
        let root = tree.root();
        assert_eq!(names(&tree, root), ["src", "a.txt", "b.txt"]);
        let src = tree.find("src".as_ref()).unwrap();

        tree.apply_listing(root, Some(vec![dir("src"), file("c.txt")]));
        assert_eq!(names(&tree, root), ["src", "c.txt"]);
        assert_eq!(tree.find("src".as_ref()), Some(src));
        assert_eq!(tree.find("a.txt".as_ref()), None);
    }

    #[test]
    fn expanding_requests_listings() {
        let mut tree = tree_with(vec![dir("src")]);
        assert_eq!(tree.take_listing_requests(), [tree.root()]);
        let src = tree.find("src".as_ref()).unwrap();
        tree.expand(src);
        tree.expand(src);
        assert_eq!(tree.take_listing_requests(), [src]);
        tree.apply_listing(src, Some(vec![file("main.rs")]));
        assert_eq!(
            tree.path(tree.find("src/main.rs".as_ref()).unwrap()),
            Path::new("src/main.rs")
        );
        // A directory that is collapsed and expanded again is listed afresh.
        tree.collapse(src);
        tree.expand(src);
        assert_eq!(tree.take_listing_requests(), [src]);
    }

    #[test]
    fn collapsed_directories_hold_their_run() {
        let mut tree = tree_with(vec![run("src", &["main", "java"]), file("README.md")]);
        let java = tree.find("src/main/java".as_ref()).unwrap();
        let src = tree.find("src".as_ref()).unwrap();
        assert_eq!(
            tree.only_directory_child(src),
            tree.find("src/main".as_ref())
        );
        assert!(!tree.node(java).expanded);

        // The run stays as long as the probe finds it, keeping its ids.
        let root = tree.root();
        tree.apply_listing(root, Some(vec![run("src", &["main", "java"])]));
        assert_eq!(tree.find("src/main/java".as_ref()), Some(java));

        // A file in `src/main` ends the run.
        tree.apply_listing(root, Some(vec![run("src", &["main"])]));
        assert_eq!(tree.find("src/main/java".as_ref()), None);
        assert!(tree.find("src/main".as_ref()).is_some());
    }

    #[test]
    fn collapsing_forgets_everything_but_the_run() {
        let mut tree = tree_with(vec![dir("outer")]);
        let outer = tree.find("outer".as_ref()).unwrap();
        tree.expand(outer);
        tree.apply_listing(outer, Some(vec![dir("inner"), file("a.txt")]));
        let inner = tree.find("outer/inner".as_ref()).unwrap();
        tree.expand(inner);
        tree.apply_listing(inner, Some(vec![file("b.txt")]));

        tree.collapse(outer);
        assert!(!tree.node(outer).expanded);
        assert!(!tree.contains(inner));
        assert!(tree.children(outer).is_empty());

        tree.expand(outer);
        tree.apply_listing(outer, Some(vec![dir("inner")]));
        let inner = tree.find("outer/inner".as_ref()).unwrap();
        tree.expand(inner);
        tree.apply_listing(inner, Some(vec![file("b.txt")]));
        tree.collapse(outer);
        // `outer` holds only `inner`, a run: kept, but collapsed and without its contents.
        assert_eq!(tree.find("outer/inner".as_ref()), Some(inner));
        assert!(!tree.node(inner).expanded);
        assert!(tree.children(inner).is_empty());
    }

    #[test]
    fn unreadable_directories_stay_expandable() {
        let mut tree = tree_with(vec![dir("locked")]);
        let locked = tree.find("locked".as_ref()).unwrap();
        tree.expand(locked);
        tree.apply_listing(locked, None);
        assert!(matches!(tree.node(locked).children, Children::Unreadable));
        assert!(tree.node(locked).expanded);
        tree.apply_listing(locked, Some(vec![file("inside.txt")]));
        assert_eq!(names(&tree, locked), ["inside.txt"]);
    }

    #[test]
    fn reveal_expands_what_is_listed() {
        let mut tree = tree_with(vec![dir("outer"), file("anchor.txt")]);
        let outer = tree.find("outer".as_ref()).unwrap();
        tree.take_listing_requests();

        assert_eq!(
            tree.reveal("outer/inner/a.txt".as_ref()),
            Reveal::Unlisted(outer)
        );
        assert_eq!(tree.take_listing_requests(), [outer]);
        tree.apply_listing(outer, Some(vec![dir("inner")]));
        let Reveal::Unlisted(inner) = tree.reveal("outer/inner/a.txt".as_ref()) else {
            panic!("`inner` is not listed yet");
        };
        tree.apply_listing(inner, Some(vec![file("a.txt")]));
        let a = tree.find("outer/inner/a.txt".as_ref()).unwrap();
        assert_eq!(tree.reveal("outer/inner/a.txt".as_ref()), Reveal::Found(a));
        assert_eq!(tree.reveal("outer/missing".as_ref()), Reveal::Missing);
        assert_eq!(tree.reveal("".as_ref()), Reveal::Found(tree.root()));
    }

    #[test]
    fn refreshed_files_keep_their_ids() {
        let mut tree = tree_with(vec![file("a.txt")]);
        let a = tree.find("a.txt".as_ref()).unwrap();
        let root = tree.root();
        tree.apply_listing(
            root,
            Some(vec![Entry::new("a.txt", Kind::Link(LinkTarget::File))]),
        );
        assert_eq!(tree.find("a.txt".as_ref()), Some(a));
        assert_eq!(tree.node(a).kind, Kind::Link(LinkTarget::File));
    }

    #[test]
    fn replaced_kinds_get_new_nodes() {
        let mut tree = tree_with(vec![dir("thing")]);
        let thing = tree.find("thing".as_ref()).unwrap();
        let root = tree.root();
        tree.apply_listing(root, Some(vec![file("thing")]));
        let new = tree.find("thing".as_ref()).unwrap();
        assert_ne!(new, thing);
        assert!(tree.node(new).kind.is_file());
    }
}
