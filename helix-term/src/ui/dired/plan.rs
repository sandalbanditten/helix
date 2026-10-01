//! Working out what the edits of a dired buffer ask for, and whether it can be done.
//!
//! Which entry an edited line belongs to follows from the edits themselves, the changes since
//! the buffer was listed: a line is the entry whose line break survived in it, wherever it was
//! moved to. A line whose text was all deleted is a deletion, unless new lines took its place, as
//! changing whole lines (`xc`) does: those then edit the entries they replaced, in order.

use std::{
    collections::{HashMap, HashSet},
    fs,
    ops::Range,
    path::{Path, PathBuf},
    time::SystemTime,
};

use helix_core::{Assoc, ChangeSet, Operation, Rope, RopeSlice};
use helix_view::dired::{Entry, Kind, Listing, Source};

use super::format::{self, Clock};

/// What a write does to the files, in the order it does it.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Plan {
    pub moves: Vec<Move>,
    /// Paths whose entries go, with everything in them.
    pub deletions: Vec<PathBuf>,
    pub changes: Vec<Change>,
}

impl Plan {
    pub fn len(&self) -> usize {
        self.moves.len() + self.deletions.len() + self.changes.len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

/// Moving or renaming the entry at `from` to `to`, both absolute as listed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Move {
    pub from: PathBuf,
    pub to: PathBuf,
}

/// Changing something about the entry listed at `path` (absolute), after it moved.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Change {
    pub path: PathBuf,
    pub metadata: Metadata,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Metadata {
    /// Pointing a link somewhere else.
    Link(PathBuf),
    Owner {
        uid: Option<u32>,
        gid: Option<u32>,
    },
    Mode(u32),
    Modified(SystemTime),
}

/// Something about an edit that keeps a write from applying it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Problem {
    /// The chars of the buffer it is about.
    pub range: Range<usize>,
    pub message: String,
    /// Whether `:w!` applies it anyway.
    pub forced: bool,
}

/// The plan for the edits of `text` since it was `listing.text`, which `changes` turned it into,
/// and the problems found. The plan is only to be carried out without problems, or with only
/// forced ones under `:w!`.
pub fn plan(
    listing: &Listing,
    text: RopeSlice,
    changes: &ChangeSet,
    clock: &Clock,
) -> (Plan, Vec<Problem>) {
    let mut planner = Planner {
        listing,
        text,
        clock,
        root: listing.source.root(),
        tree: matches!(listing.source, Source::Tree { .. }),
        plan: Plan::default(),
        problems: Vec::new(),
        move_ranges: Vec::new(),
        touched: Vec::new(),
    };
    let lines = lines(&listing.text, text, changes);
    for &(line, _) in &lines.joined {
        planner.problem_at_line(line, "Joined with the line of another entry", false);
    }
    for &line in &lines.added {
        planner.problem_at_line(line, "Lines cannot be added", false);
    }
    let joined: HashSet<usize> = lines.joined.iter().map(|(_, entry)| *entry).collect();
    for (index, line) in lines.entries.iter().enumerate() {
        match *line {
            Some(line) => planner.edited(index, line),
            None if joined.contains(&index) => {}
            None => planner.deleted(index, lines.collapsed[index]),
        }
    }
    planner.validate();
    (planner.plan, planner.problems)
}

/// Where the listed entries went.
#[derive(Debug, Default, PartialEq, Eq)]
struct Lines {
    /// For each entry, the line it is on now, or `None` if its line was deleted.
    entries: Vec<Option<usize>>,
    /// For each entry, the line where its text was before it was deleted.
    collapsed: Vec<usize>,
    /// Lines of new text that replace no entry.
    added: Vec<usize>,
    /// Lines holding the text of an entry whose line break went: `(line, entry)`.
    joined: Vec<(usize, usize)>,
}

/// A stretch of the listed text that `changes` kept: the chars `old`, now from `new`.
struct Kept {
    old: Range<usize>,
    new: usize,
}

fn kept(changes: &ChangeSet) -> Vec<Kept> {
    let (mut old, mut new) = (0, 0);
    let mut kept = Vec::new();
    for operation in changes.changes() {
        match operation {
            Operation::Retain(len) => {
                kept.push(Kept {
                    old: old..old + len,
                    new,
                });
                old += len;
                new += len;
            }
            Operation::Delete(len) => old += len,
            Operation::Insert(text) => new += text.chars().count(),
        }
    }
    kept
}

/// Where the first char of `range` (of the listed text) that was kept is now.
fn first_kept(kept: &[Kept], range: Range<usize>) -> Option<usize> {
    let first = kept.partition_point(|kept| kept.old.end <= range.start);
    let kept = kept.get(first)?;
    let start = kept.old.start.max(range.start);
    (start < range.end).then(|| kept.new + start - kept.old.start)
}

fn lines(listed: &Rope, text: RopeSlice, changes: &ChangeSet) -> Lines {
    let kept = kept(changes);
    let count = listed.len_lines() - 1;
    let mut lines = Lines {
        entries: vec![None; count],
        collapsed: vec![0; count],
        ..Lines::default()
    };
    let mut owners: HashMap<usize, usize> = HashMap::new();
    let range = |entry: usize| {
        let start = listed.line_to_char(entry);
        start..listed.line_to_char(entry + 1)
    };

    // A line is the entry whose line break it ends with.
    for entry in 0..count {
        let line_break = range(entry).end - 1;
        if let Some(pos) = first_kept(&kept, line_break..line_break + 1) {
            let line = text.char_to_line(pos);
            owners.insert(line, entry);
            lines.entries[entry] = Some(line);
        }
    }
    // An entry that lost its line break but kept some text is on that text's line, unless that
    // is the line of another entry.
    for entry in 0..count {
        if lines.entries[entry].is_some() {
            continue;
        }
        let range = range(entry);
        let start = range.start;
        lines.collapsed[entry] = text.char_to_line(changes.map_pos(start, Assoc::Before));
        let Some(pos) = first_kept(&kept, range.start..range.end - 1) else {
            continue;
        };
        let line = text.char_to_line(pos);
        match owners.get(&line) {
            Some(_) => lines.joined.push((line, entry)),
            None => {
                owners.insert(line, entry);
                lines.entries[entry] = Some(line);
            }
        }
    }

    // New lines where deleted entries were replace them, as many as there were.
    let blank = |line: usize| text.line(line).chars().all(char::is_whitespace);
    let mut added: Vec<usize> = (0..text.len_lines())
        .filter(|line| !owners.contains_key(line) && !blank(*line))
        .collect();
    let joined: HashSet<usize> = lines.joined.iter().map(|(_, entry)| *entry).collect();
    let mut entry = 0;
    while entry < count {
        if lines.entries[entry].is_some() || joined.contains(&entry) {
            entry += 1;
            continue;
        }
        let line = lines.collapsed[entry];
        let deleted = (entry..count)
            .take_while(|&next| {
                lines.entries[next].is_none()
                    && !joined.contains(&next)
                    && lines.collapsed[next] == line
            })
            .count();
        let new = added.iter().position(|&added| added == line);
        if let Some(new) = new {
            let run = added[new..]
                .iter()
                .enumerate()
                .take_while(|(i, added)| **added == line + i)
                .count();
            if run == deleted {
                for i in 0..deleted {
                    lines.entries[entry + i] = Some(line + i);
                }
                added.drain(new..new + run);
            }
        }
        entry += deleted;
    }
    lines.added = added;
    lines
}

struct Planner<'a> {
    listing: &'a Listing,
    text: RopeSlice<'a>,
    clock: &'a Clock,
    root: &'a Path,
    tree: bool,
    plan: Plan,
    problems: Vec<Problem>,
    /// Where each move of the plan was typed, for problems about it.
    move_ranges: Vec<Range<usize>>,
    /// The entries something is done to, which must still be what was listed, and their lines.
    touched: Vec<(usize, Range<usize>)>,
}

impl Planner<'_> {
    fn problem(&mut self, range: Range<usize>, message: impl Into<String>, forced: bool) {
        self.problems.push(Problem {
            range,
            message: message.into(),
            forced,
        });
    }

    /// The chars of `line` but its line break.
    fn line_range(&self, line: usize) -> Range<usize> {
        let line = line.min(self.text.len_lines().saturating_sub(1));
        let start = self.text.line_to_char(line);
        start..start + self.text.line(line).len_chars().saturating_sub(1)
    }

    fn problem_at_line(&mut self, line: usize, message: impl Into<String>, forced: bool) {
        let range = self.line_range(line);
        self.problem(range, message, forced);
    }

    /// Plans the edits of entry `index`, now on `line`.
    fn edited(&mut self, index: usize, line: usize) {
        let listing = self.listing;
        let entry = &listing.entries[index];
        let listed_line = listing.text.line(index).to_string();
        let line_text = self.text.line(line).to_string();
        if listed_line.trim_end() == line_text.trim_end() {
            return;
        }
        let start = self.text.line_to_char(line);
        let chars = |byte: Range<usize>| {
            start + line_text[..byte.start].chars().count()
                ..start + line_text[..byte.end].chars().count()
        };
        let Ok(listed) = format::parse(&listed_line, listing.columns, self.tree) else {
            return;
        };
        let new = match format::parse(&line_text, listing.columns, self.tree) {
            Ok(new) => new,
            Err(message) => return self.problem_at_line(line, message, false),
        };
        let listed_field = |range: &Range<usize>| &listed_line[range.clone()];
        let field = |range: &Range<usize>| &line_text[range.clone()];
        let path = self.root.join(&entry.path);
        let planned = self.plan.len();

        if listed_field(&listed.size) != field(&new.size) {
            self.problem(chars(new.size.clone()), "The size cannot be edited", false);
        }
        if listing.columns.unix {
            let octal = listed_field(&listed.octal) != field(&new.octal);
            let permissions = listed_field(&listed.permissions) != field(&new.permissions);
            if octal || permissions {
                let range = chars(new.octal.start..new.permissions.end);
                self.mode(
                    entry,
                    &path,
                    field(&new.octal),
                    field(&new.permissions),
                    [octal, permissions],
                    range,
                );
            }
            let user = (listed_field(&listed.user) != field(&new.user)).then(|| field(&new.user));
            let group =
                (listed_field(&listed.group) != field(&new.group)).then(|| field(&new.group));
            if user.is_some() || group.is_some() {
                self.owner(&path, user, group, chars(new.user.start..new.group.end));
            }
        }
        if listed_field(&listed.date) != field(&new.date) {
            match format::parse_date(field(&new.date), entry.modified, self.clock) {
                Some(time) => self.change(&path, Metadata::Modified(time)),
                None => self.problem(chars(new.date.clone()), "Unreadable date", false),
            }
        }
        if listed.git.as_ref().map(listed_field) != new.git.as_ref().map(field) {
            let range = new.git.clone().map(chars).unwrap_or(start..start);
            self.problem(range, "The git status cannot be edited yet", false);
        }

        let name = format::unquote(field(&new.name));
        if name.trim().is_empty() {
            self.delete(index, chars(new.name.clone()));
        } else if field(&new.name) != listed_field(&listed.name) {
            self.rename(entry, &path, &name, chars(new.name.clone()));
        }
        if entry.kind == Kind::Link
            && listed.target.as_ref().map(listed_field) != new.target.as_ref().map(field)
        {
            match new
                .target
                .clone()
                .filter(|target| !field(target).trim().is_empty())
            {
                Some(target) => {
                    let target = PathBuf::from(format::unquote(field(&target)));
                    self.change(&path, Metadata::Link(target));
                }
                None => self.problem(chars(new.name.clone()), "A link needs a target", false),
            }
        }
        if self.plan.len() > planned {
            self.touched.push((index, self.line_range(line)));
        }
    }

    /// Plans changing the mode of `entry` to the `octal` or symbolic `permissions`, of which
    /// `changed` tells which were edited.
    fn mode(
        &mut self,
        entry: &Entry,
        path: &Path,
        octal: &str,
        permissions: &str,
        changed: [bool; 2],
        range: Range<usize>,
    ) {
        let octal = changed[0].then(|| format::parse_octal(octal));
        let permissions = changed[1].then(|| format::parse_permissions(permissions));
        let mode = match (octal, permissions) {
            (Some(None), _) => return self.problem(range, "Unreadable octal permissions", false),
            (_, Some(None)) => return self.problem(range, "Unreadable permissions", false),
            (_, Some(Some((kind, _)))) if kind != format::kind_letter(entry.kind) => {
                return self.problem(range, "The kind of an entry cannot be changed", false)
            }
            (Some(Some(octal)), Some(Some((_, mode)))) if octal != mode => {
                return self.problem(range, "The octal and the other permissions disagree", false)
            }
            (Some(Some(mode)), _) | (None, Some(Some((_, mode)))) => mode,
            (None, None) => return,
        };
        if entry.kind == Kind::Link {
            return self.problem(range, "Links have no permissions of their own", false);
        }
        if mode != entry.mode {
            self.change(path, Metadata::Mode(mode));
        }
    }

    /// Plans changing the owner of `path` to the edited `user` and `group`, names or ids.
    fn owner(&mut self, path: &Path, user: Option<&str>, group: Option<&str>, range: Range<usize>) {
        let uid = user.map(|user| user.parse().ok().or_else(|| user_id(user)));
        let gid = group.map(|group| group.parse().ok().or_else(|| group_id(group)));
        match (uid, gid) {
            (Some(None), _) => {
                let user = user.unwrap_or_default();
                self.problem(range, format!("No user is called `{user}`"), false)
            }
            (_, Some(None)) => {
                let group = group.unwrap_or_default();
                self.problem(range, format!("No group is called `{group}`"), false)
            }
            (uid, gid) => self.change(
                path,
                Metadata::Owner {
                    uid: uid.flatten(),
                    gid: gid.flatten(),
                },
            ),
        }
    }

    fn change(&mut self, path: &Path, metadata: Metadata) {
        self.plan.changes.push(Change {
            path: path.to_path_buf(),
            metadata,
        });
    }

    fn rename(&mut self, entry: &Entry, path: &Path, name: &str, range: Range<usize>) {
        if entry.path.as_os_str().is_empty() {
            return self.problem(range, "The root cannot be renamed", false);
        }
        let to = target(path, name);
        if to.file_name().is_none() || name.contains('\0') {
            return self.problem(range, format!("`{name}` is not a file name"), false);
        }
        if to != path {
            self.plan.moves.push(Move {
                from: path.to_path_buf(),
                to,
            });
            self.move_ranges.push(range);
        }
    }

    /// Entry `index` was deleted with its whole line, which was at `line`.
    fn deleted(&mut self, index: usize, line: usize) {
        let line = line.min(self.text.len_lines().saturating_sub(1));
        let start = self.text.line_to_char(line);
        self.delete(index, start..start);
    }

    /// Plans deleting entry `index`, whose name or line went at `range`.
    fn delete(&mut self, index: usize, range: Range<usize>) {
        let listing = self.listing;
        let entry = &listing.entries[index];
        if entry.path.as_os_str().is_empty() {
            return self.problem(range, "The root cannot be deleted", false);
        }
        let name = format::quote(&entry.path.to_string_lossy());
        self.problem(
            range.clone(),
            format!("Deletes {name} (use :w! to apply)"),
            true,
        );
        self.plan.deletions.push(self.root.join(&entry.path));
        self.touched.push((index, range));
    }

    /// Checks the plan as a whole: what the moves run into, and what is still as listed.
    fn validate(&mut self) {
        // Deleting a directory deletes what it holds; only the outermost deletion is needed.
        let deleted = std::mem::take(&mut self.plan.deletions);
        self.plan.deletions = deleted
            .iter()
            .filter(|path| {
                !deleted
                    .iter()
                    .any(|dir| dir != *path && path.starts_with(dir))
            })
            .cloned()
            .collect();

        let sources: HashSet<PathBuf> = self.plan.moves.iter().map(|m| m.from.clone()).collect();
        let targets: HashSet<PathBuf> = self.plan.moves.iter().map(|m| m.to.clone()).collect();
        let mut seen = HashSet::new();
        let moves = self.plan.moves.clone();
        for (Move { from, to }, range) in moves.into_iter().zip(self.move_ranges.clone()) {
            let name = shown(&to);
            if !seen.insert(to.clone()) {
                self.problem(range, format!("Another entry moves to {name} too"), false);
            } else if to.starts_with(&from) {
                self.problem(range, "A directory cannot move into itself", false);
            } else if deleted.iter().any(|dir| to.starts_with(dir)) {
                self.problem(range, format!("{name} is deleted by the same write"), false);
            } else if fs::symlink_metadata(&to).is_ok() && !sources.contains(&to) {
                self.problem(range, format!("{name} already exists"), false);
            } else if let Some(parent) = to.parent().filter(|parent| {
                !parent.is_dir() && !targets.contains(*parent) && !sources.contains(*parent)
            }) {
                let parent = shown(parent);
                self.problem(
                    range,
                    format!("Creates the directory {parent} (use :w! to apply)"),
                    true,
                );
            }
        }

        for (index, range) in std::mem::take(&mut self.touched) {
            let entry = &self.listing.entries[index];
            let path = self.root.join(&entry.path);
            if deleted
                .iter()
                .any(|dir| path != *dir && path.starts_with(dir))
            {
                self.problem(
                    range.clone(),
                    "Inside a directory deleted by the same write",
                    false,
                );
            }
            if !still_listed(&path, entry) {
                self.problem(
                    range,
                    "Changed on disk since it was listed (use :reload)",
                    false,
                );
            }
        }
    }
}

/// A path for a message, relative to the working directory when it is below it.
fn shown(path: &Path) -> String {
    format::quote(&helix_stdx::path::get_relative_path(path).to_string_lossy())
}

/// Where the entry at `path` goes when renamed to `name`: relative to its directory, absolute,
/// or below the home directory with `~`.
fn target(path: &Path, name: &str) -> PathBuf {
    let name = helix_stdx::path::expand_tilde(Path::new(name));
    let dir = path.parent().unwrap_or(path);
    helix_stdx::path::normalize(dir.join(name))
}

/// Whether `path` still holds the file `entry` listed.
fn still_listed(path: &Path, entry: &Entry) -> bool {
    let Ok(metadata) = fs::symlink_metadata(path) else {
        return false;
    };
    identity(&metadata) == entry.id
}

#[cfg(unix)]
fn identity(metadata: &fs::Metadata) -> (u64, u64) {
    use std::os::unix::fs::MetadataExt;
    (metadata.dev(), metadata.ino())
}

#[cfg(not(unix))]
fn identity(_metadata: &fs::Metadata) -> (u64, u64) {
    (0, 0)
}

#[cfg(unix)]
use helix_stdx::users::{group_id, user_id};

#[cfg(not(unix))]
fn user_id(_name: &str) -> Option<u32> {
    None
}

#[cfg(not(unix))]
fn group_id(_name: &str) -> Option<u32> {
    None
}

#[cfg(test)]
mod tests {
    use helix_core::Transaction;

    use super::*;

    /// The lines entries are on after `listed` became `edited` by `edits`, a list of
    /// `(from, to, replacement)` char changes applied one after another.
    fn lines_after(listed: &str, edits: &[(usize, usize, &str)]) -> (Lines, String) {
        let listed = Rope::from(listed);
        let mut text = listed.clone();
        let mut changes = ChangeSet::new(listed.slice(..));
        for &(from, to, replacement) in edits {
            let transaction =
                Transaction::change(&text, [(from, to, Some(replacement.into()))].into_iter());
            transaction.apply(&mut text);
            changes = changes.compose(transaction.changes().clone());
        }
        (lines(&listed, text.slice(..), &changes), text.to_string())
    }

    const LISTED: &str = "a\nb\nc\nd\n";

    #[test]
    fn edited_lines_keep_their_entries() {
        // `b` renamed.
        let (lines, text) = lines_after(LISTED, &[(2, 3, "bee")]);
        assert_eq!(text, "a\nbee\nc\nd\n");
        assert_eq!(lines.entries, [Some(0), Some(1), Some(2), Some(3)]);
        assert!(lines.added.is_empty() && lines.joined.is_empty());
    }

    #[test]
    fn deleted_lines_are_deleted_entries() {
        let (lines, _) = lines_after(LISTED, &[(2, 4, "")]);
        assert_eq!(lines.entries, [Some(0), None, Some(1), Some(2)]);
        assert!(lines.added.is_empty());
    }

    #[test]
    fn retyped_lines_still_edit_their_entries() {
        // `xc` on `b` and `c`: both lines deleted, then two new lines typed in their place.
        let (lines, text) = lines_after(LISTED, &[(2, 6, ""), (2, 2, "B\nC\n")]);
        assert_eq!(text, "a\nB\nC\nd\n");
        assert_eq!(lines.entries, [Some(0), Some(1), Some(2), Some(3)]);
        assert!(lines.added.is_empty());
    }

    #[test]
    fn new_lines_elsewhere_are_added_lines() {
        // `b` deleted, and a line pasted after `d`.
        let (lines, text) = lines_after(LISTED, &[(2, 4, ""), (6, 6, "b\n")]);
        assert_eq!(text, "a\nc\nd\nb\n");
        assert_eq!(lines.entries, [Some(0), None, Some(1), Some(2)]);
        assert_eq!(lines.added, [3]);
        // Blank lines are no entries.
        let (lines, _) = lines_after(LISTED, &[(2, 2, "  \n")]);
        assert!(lines.added.is_empty());
    }

    #[test]
    fn joined_lines_are_noticed() {
        let (lines, text) = lines_after(LISTED, &[(1, 2, " ")]);
        assert_eq!(text, "a b\nc\nd\n");
        assert_eq!(lines.entries, [None, Some(0), Some(1), Some(2)]);
        assert_eq!(lines.joined, [(0, 0)]);
        // The last line may lose its line break.
        let (lines, _) = lines_after(LISTED, &[(7, 8, "")]);
        assert_eq!(lines.entries, [Some(0), Some(1), Some(2), Some(3)]);
    }

    #[test]
    fn targets_are_relative_to_the_entry() {
        let path = Path::new("/repo/src/main.rs");
        assert_eq!(target(path, "lib.rs"), Path::new("/repo/src/lib.rs"));
        assert_eq!(target(path, "../main.rs"), Path::new("/repo/main.rs"));
        assert_eq!(
            target(path, "new/dir/main.rs"),
            Path::new("/repo/src/new/dir/main.rs")
        );
        assert_eq!(target(path, "./x"), Path::new("/repo/src/x"));
        assert_eq!(target(path, "/tmp/x"), Path::new("/tmp/x"));
    }

    /// A listing of a temporary directory holding `a.txt`, `b.txt` and `sub/`.
    fn listing() -> (tempfile::TempDir, Listing) {
        let dir = tempfile::tempdir().unwrap();
        let root = helix_stdx::path::canonicalize(dir.path());
        fs::write(root.join("a.txt"), "a").unwrap();
        fs::write(root.join("b.txt"), "b").unwrap();
        fs::create_dir(root.join("sub")).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(root.join("b.txt"), fs::Permissions::from_mode(0o644)).unwrap();
        }
        let options = super::super::listing::Options {
            sort: helix_view::editor::FileTreeSort::DirectoriesFirst,
            icons: false,
            providers: helix_vcs::DiffProviderRegistry::default(),
            trust_git: true,
        };
        let mut listing = super::super::listing::read(&Source::Directory(root), &options);
        listing.text = Rope::from(format::text(&listing, &Clock::system()));
        (dir, listing)
    }

    /// Plans `edit` turning each line of the listing into the line it returns.
    fn plan_lines(listing: &Listing, edit: impl Fn(usize, &str) -> String) -> (Plan, Vec<Problem>) {
        let listed = &listing.text;
        let mut changes = Vec::new();
        for line in 0..listed.len_lines() - 1 {
            let text = listed.line(line).to_string();
            let text = text.trim_end_matches('\n');
            let edited = edit(line, text);
            if edited != text {
                let start = listed.line_to_char(line);
                changes.push((start, start + text.chars().count(), Some(edited.into())));
            }
        }
        let transaction = Transaction::change(listed, changes.into_iter());
        let mut text = listed.clone();
        transaction.apply(&mut text);
        plan(
            listing,
            text.slice(..),
            transaction.changes(),
            &Clock::system(),
        )
    }

    fn messages(problems: &[Problem]) -> Vec<(&str, bool)> {
        problems
            .iter()
            .map(|problem| (problem.message.as_str(), problem.forced))
            .collect()
    }

    #[test]
    fn edited_columns_become_changes() {
        let (_dir, listing) = listing();
        let root = listing.source.root().to_path_buf();
        // `sub`, `a.txt`, `b.txt`
        let (plan, problems) = plan_lines(&listing, |line, text| match line {
            1 => text.replace("a.txt", "../c.txt"),
            2 => text.replacen("0644", "0755", 1),
            _ => text.to_owned(),
        });
        assert_eq!(messages(&problems), []);
        assert_eq!(
            plan.moves,
            [Move {
                from: root.join("a.txt"),
                to: root.parent().unwrap().join("c.txt"),
            }]
        );
        assert_eq!(
            plan.changes,
            [Change {
                path: root.join("b.txt"),
                metadata: Metadata::Mode(0o755),
            }]
        );
    }

    #[test]
    fn problems_keep_writes_from_happening() {
        let (_dir, listing) = listing();
        let (_, problems) = plan_lines(&listing, |line, text| match line {
            // An existing name, an unreadable mode, an edited size.
            1 => text.replace("a.txt", "b.txt"),
            2 => text.replacen(".rw-r--r--", ".rwqr--r--", 1),
            _ => text.to_owned(),
        });
        let messages = messages(&problems);
        assert_eq!(messages[0], ("Unreadable permissions", false));
        // Paths outside the working directory are shown whole.
        assert!(
            messages[1].0.ends_with("/b.txt already exists"),
            "{messages:?}"
        );
        assert_eq!(messages.len(), 2);

        // Swapping two names is fine, a missing directory takes `:w!`.
        let (plan, problems) = plan_lines(&listing, |line, text| match line {
            1 => text.replace("a.txt", "b.txt"),
            2 => text.replace("b.txt", "new/a.txt"),
            _ => text.to_owned(),
        });
        assert_eq!(plan.moves.len(), 2);
        assert_eq!(problems.len(), 1);
        assert!(problems[0].forced, "{:?}", problems[0]);
        assert!(problems[0].message.starts_with("Creates the directory"));
    }

    #[test]
    fn emptied_names_delete_with_force() {
        let (_dir, listing) = listing();
        let root = listing.source.root().to_path_buf();
        let (plan, problems) = plan_lines(&listing, |line, text| match line {
            0 => text.replace("sub", ""),
            _ => text.to_owned(),
        });
        assert_eq!(plan.deletions, [root.join("sub")]);
        assert_eq!(
            messages(&problems),
            [("Deletes sub (use :w! to apply)", true)]
        );
    }

    #[test]
    fn entries_changed_on_disk_are_left_alone() {
        let (dir, listing) = listing();
        fs::remove_file(dir.path().join("a.txt")).unwrap();
        fs::write(dir.path().join("a.txt"), "new").unwrap();
        let (_, problems) = plan_lines(&listing, |line, text| match line {
            1 => text.replace("a.txt", "z.txt"),
            _ => text.to_owned(),
        });
        assert_eq!(
            messages(&problems),
            [("Changed on disk since it was listed (use :reload)", false)]
        );
    }
}
