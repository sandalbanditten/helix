//! Working out what the edits of a dired buffer ask for, and whether it can be done.
//!
//! Which entry an edited line belongs to follows from the edits themselves, the changes since
//! the buffer was listed: a line is the entry whose text it kept, wherever it was moved to. An
//! entry whose text is all gone was deleted, unless new lines took its place between the same
//! neighbors, as changing whole lines (`xc`) does: those then edit the entries they replaced, in
//! order. Pasted lines copy the entries they were yanked from, or move them when cut.

use std::{
    borrow::Cow,
    collections::{HashMap, HashSet},
    fs,
    ops::Range,
    path::{Path, PathBuf},
    time::SystemTime,
};

use helix_core::{ChangeSet, Operation, Rope, RopeSlice};
use helix_view::dired::{Entry, GitStatus, Kind, Listing, Size, Source};

use super::{
    format::{self, Clock},
    git,
    paste::{Origin, Paste},
};

/// What a write does to the files, in the order it does it.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Plan {
    pub moves: Vec<Move>,
    /// Copies of entries: `from` copied to `to`, after the moves.
    pub copies: Vec<Move>,
    /// Paths whose entries go, with everything in them.
    pub deletions: Vec<PathBuf>,
    pub changes: Vec<Change>,
    /// The working tree of the repository the git edits are for.
    pub repo: Option<PathBuf>,
    pub git: Vec<(git::Action, PathBuf)>,
    pub ignores: Vec<git::IgnoreEdit>,
}

impl Plan {
    pub fn len(&self) -> usize {
        self.moves.len()
            + self.copies.len()
            + self.deletions.len()
            + self.changes.len()
            + self.git.len()
            + self.ignores.len()
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
    /// Whether the entry is the one a pasted line makes at `path` rather than one listed there.
    pub pasted: bool,
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
/// forced ones under `:w!`. Only a `trusted` workspace runs git.
pub fn plan(
    listing: &Listing,
    text: RopeSlice,
    changes: &ChangeSet,
    pasted: &[Paste],
    clock: &Clock,
    trusted: bool,
) -> (Plan, Vec<Problem>) {
    let mut planner = Planner {
        listing,
        text,
        clock,
        trusted,
        root: listing.source.root(),
        tree: matches!(listing.source, Source::Tree { .. }),
        plan: Plan {
            repo: listing.repo.clone(),
            ..Plan::default()
        },
        problems: Vec::new(),
        move_ranges: Vec::new(),
        copy_ranges: Vec::new(),
        deleting: Vec::new(),
        touched: Vec::new(),
    };
    let pasted_lines: HashSet<usize> = pasted.iter().map(|paste| paste.line).collect();
    let lines = lines(&listing.text, text, changes, &pasted_lines);
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
            None => planner.deleted(index, lines.places[index]),
        }
    }
    let mut owners: Vec<(usize, usize)> = lines
        .entries
        .iter()
        .enumerate()
        .filter_map(|(entry, line)| Some(((*line)?, entry)))
        .collect();
    owners.sort_unstable();
    for paste in pasted {
        let Some(origin) = planner.origin(paste) else {
            let message = "Reads like more than one listed entry";
            planner.problem_at_line(paste.line, message, false);
            continue;
        };
        let dir = planner.destination(paste.line, &owners);
        planner.plan_line(Subject::Pasted { origin, dir }, paste.line);
    }
    planner.validate();
    (planner.plan, planner.problems)
}

/// Where the listed entries went.
#[derive(Debug, Default, PartialEq, Eq)]
struct Lines {
    /// For each entry, the line it is on now, or `None` if its line was deleted.
    entries: Vec<Option<usize>>,
    /// For each entry, the line where it would be, for a deleted one.
    places: Vec<usize>,
    /// Lines of new text that replace no entry.
    added: Vec<usize>,
    /// Lines holding text of an entry that is on another line: `(line, entry)`, when lines were
    /// joined or split.
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

fn lines(listed: &Rope, text: RopeSlice, changes: &ChangeSet, pasted: &HashSet<usize>) -> Lines {
    let kept = kept(changes);
    let count = listed.len_lines() - 1;
    let mut lines = Lines {
        entries: vec![None; count],
        places: vec![0; count],
        ..Lines::default()
    };

    // A line is the entry whose text it kept, the first one if it kept the text of several.
    let mut owners: HashMap<usize, usize> = HashMap::new();
    for entry in 0..count {
        let start = listed.line_to_char(entry);
        let end = listed.line_to_char(entry + 1) - 1;
        let first = kept.partition_point(|kept| kept.old.end <= start);
        let mut entry_lines = Vec::new();
        for kept in kept[first..].iter().take_while(|kept| kept.old.start < end) {
            let from = kept.new + kept.old.start.max(start) - kept.old.start;
            let to = kept.new + kept.old.end.min(end) - kept.old.start;
            if from < to {
                let (first, last) = (text.char_to_line(from), text.char_to_line(to - 1));
                for line in first..=last {
                    if entry_lines.last() != Some(&line) {
                        entry_lines.push(line);
                    }
                }
            }
        }
        for (i, &line) in entry_lines.iter().enumerate() {
            match owners.get(&line) {
                None if i == 0 => {
                    owners.insert(line, entry);
                    lines.entries[entry] = Some(line);
                }
                _ => lines.joined.push((line, entry)),
            }
        }
    }

    // Deleted entries between two kept ones are replaced by as many new lines between them.
    let blank = |line: usize| text.line(line).chars().all(char::is_whitespace);
    let new_lines = |range: Range<usize>| -> Vec<usize> {
        range
            .filter(|line| !owners.contains_key(line) && !pasted.contains(line) && !blank(*line))
            .collect()
    };
    let joined: HashSet<usize> = lines.joined.iter().map(|(_, entry)| *entry).collect();
    let mut previous: Option<usize> = None;
    let mut deleted = Vec::new();
    for entry in 0..=count {
        let line = match lines.entries.get(entry) {
            Some(Some(line)) => *line,
            Some(None) if !joined.contains(&entry) => {
                deleted.push(entry);
                continue;
            }
            Some(None) => continue,
            None => text.len_lines(),
        };
        let start = previous.map_or(0, |previous| previous + 1);
        let between = if start <= line {
            new_lines(start..line)
        } else {
            Vec::new()
        };
        if between.len() == deleted.len() {
            for (&entry, &line) in deleted.iter().zip(&between) {
                lines.entries[entry] = Some(line);
            }
        } else {
            for &entry in &deleted {
                lines.places[entry] = start.min(line);
            }
        }
        deleted.clear();
        previous = Some(line).filter(|_| start <= line).or(previous);
    }
    // New lines that replace nothing, also those out of order with the kept ones.
    let paired: HashSet<usize> = lines.entries.iter().flatten().copied().collect();
    lines.added = new_lines(0..text.len_lines());
    lines.added.retain(|line| !paired.contains(line));
    lines
}

struct Planner<'a> {
    listing: &'a Listing,
    text: RopeSlice<'a>,
    clock: &'a Clock,
    trusted: bool,
    root: &'a Path,
    tree: bool,
    plan: Plan,
    problems: Vec<Problem>,
    /// Where each move and copy of the plan was typed, for problems about it.
    move_ranges: Vec<Range<usize>>,
    copy_ranges: Vec<Range<usize>>,
    /// The entries to delete, and where their names or lines went.
    deleting: Vec<(PathBuf, Range<usize>)>,
    /// The entries something is done to, which must still be what was listed.
    touched: Vec<Touched>,
}

/// An entry something is done to.
struct Touched {
    path: PathBuf,
    /// Its identity as listed.
    id: (u64, u64),
    /// The chars of the line asking for it.
    range: Range<usize>,
    /// Whether the line was pasted.
    pasted: bool,
}

/// What a line of the buffer stands for.
enum Subject<'a> {
    /// The listed entry of that index, edited in place.
    Listed(usize),
    /// A pasted line copying the entry it was yanked from into the directory `dir`.
    Pasted { origin: &'a Origin, dir: PathBuf },
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
        self.plan_line(Subject::Listed(index), line);
    }

    /// The entry `paste` copies: the one it could be, or among entries whose lines read alike,
    /// the one this write deletes, so that cutting and pasting a line moves its entry, the one
    /// yanked last, or any of them when they are files holding the same.
    fn origin<'a>(&self, paste: &'a Paste) -> Option<&'a Origin> {
        let origins = &paste.origins[..];
        if let [origin] = origins {
            return Some(origin);
        }
        let one = |mut origins: Vec<&'a Origin>| (origins.len() == 1).then(|| origins.remove(0));
        let cut = origins
            .iter()
            .filter(|origin| self.deleting.iter().any(|(path, _)| *path == origin.path));
        if let Some(origin) = one(cut.collect()) {
            return Some(origin);
        }
        let last = origins.iter().filter_map(|origin| origin.yank).max();
        let yanked = origins
            .iter()
            .filter(|origin| last.is_some() && origin.yank == last);
        if let Some(origin) = one(yanked.collect()) {
            return Some(origin);
        }
        let first = origins.first()?;
        same_files(origins.iter().map(|origin| (&origin.path, &origin.entry))).then_some(first)
    }

    /// The directory a line pasted at `line` copies into: that of the entry on the line above,
    /// or the entry itself when it is a directory listed with its contents. `owners` are the
    /// lines of entries with their entries, in order.
    fn destination(&self, line: usize, owners: &[(usize, usize)]) -> PathBuf {
        let above = owners.partition_point(|&(owned, _)| owned < line);
        let Some(&(_, index)) = above.checked_sub(1).and_then(|above| owners.get(above)) else {
            return self.root.to_path_buf();
        };
        let entry = &self.listing.entries[index];
        let expanded = match &self.listing.source {
            Source::Tree { expanded, .. } => expanded.contains(&entry.path),
            Source::Directory(_) => false,
        };
        let path = self.root.join(&entry.path);
        if entry.path.as_os_str().is_empty() || (entry.kind == Kind::Directory && expanded) {
            path
        } else {
            path.parent().unwrap_or(self.root).to_path_buf()
        }
    }

    /// Plans what `line` asks for, compared column by column with the line its subject was
    /// listed as.
    fn plan_line(&mut self, subject: Subject, line: usize) {
        let listing = self.listing;
        let (entry, listed_line, columns, tree): (&Entry, Cow<str>, _, _) = match &subject {
            Subject::Listed(index) => (
                &listing.entries[*index],
                listing.text.line(*index).into(),
                listing.columns,
                self.tree,
            ),
            Subject::Pasted { origin, .. } => (
                &origin.entry,
                origin.line.as_str().into(),
                origin.columns,
                origin.tree,
            ),
        };
        let line_text: Cow<str> = self.text.line(line).into();
        let pasted = matches!(subject, Subject::Pasted { .. });
        if !pasted && listed_line.trim_end() == line_text.trim_end() {
            return;
        }
        let start = self.text.line_to_char(line);
        let chars = |byte: Range<usize>| {
            start + line_text[..byte.start].chars().count()
                ..start + line_text[..byte.end].chars().count()
        };
        let Ok(listed) = format::parse(&listed_line, columns, tree) else {
            return;
        };
        // Guides may have been given to a line pasted from a flat listing.
        let new = match format::parse(&line_text, columns, tree || self.tree) {
            Ok(new) => new,
            Err(message) => return self.problem_at_line(line, message, false),
        };
        let listed_field = |range: &Range<usize>| &listed_line[range.clone()];
        let field = |range: &Range<usize>| &line_text[range.clone()];
        let entry_path = match &subject {
            Subject::Listed(_) => self.root.join(&entry.path),
            Subject::Pasted { origin, .. } => origin.path.clone(),
        };
        let planned = self.plan.len();

        // The path the columns are changed of: the entry's, or that of the copy.
        let name = format::unquote(field(&new.name));
        let name_range = chars(new.name.clone());
        let path = match &subject {
            Subject::Listed(_) => entry_path.clone(),
            Subject::Pasted { dir, .. } => {
                if name.trim().is_empty() || name.contains('\0') {
                    return self.problem(name_range, "A copy needs a name", false);
                }
                let to = within(dir, &name);
                self.plan.copies.push(Move {
                    from: entry_path.clone(),
                    to: to.clone(),
                });
                self.copy_ranges.push(name_range.clone());
                to
            }
        };

        let mut metadata = Vec::new();
        if listed_field(&listed.size) != field(&new.size) {
            self.problem(chars(new.size.clone()), "The size cannot be edited", false);
        }
        if columns.unix {
            let octal = listed_field(&listed.octal) != field(&new.octal);
            let permissions = listed_field(&listed.permissions) != field(&new.permissions);
            if octal || permissions {
                let range = chars(new.octal.start..new.permissions.end);
                metadata.extend(self.mode(
                    entry,
                    field(&new.octal),
                    field(&new.permissions),
                    [octal, permissions],
                    range,
                ));
            }
            let user = (listed_field(&listed.user) != field(&new.user)).then(|| field(&new.user));
            let group =
                (listed_field(&listed.group) != field(&new.group)).then(|| field(&new.group));
            if user.is_some() || group.is_some() {
                let range = chars(new.user.start..new.group.end);
                metadata.extend(self.owner(user, group, range));
            }
        }
        if listed_field(&listed.date) != field(&new.date) {
            match format::parse_date(field(&new.date), entry.modified, self.clock) {
                Some(time) => metadata.push(Metadata::Modified(time)),
                None => self.problem(chars(new.date.clone()), "Unreadable date", false),
            }
        }
        let listed_git = listed.git.as_ref().map(listed_field);
        let git = new
            .git
            .clone()
            .filter(|range| listed_git != Some(field(range)));
        let git_edited = match (entry.git, git) {
            (Some(_), Some(range)) if pasted => {
                let message = "The git status of a copy cannot be edited";
                self.problem(chars(range), message, false);
                true
            }
            (Some(status), Some(range)) => {
                self.git(entry, &path, status, field(&range), chars(range));
                true
            }
            _ => false,
        };

        if let Subject::Listed(index) = subject {
            if name.trim().is_empty() {
                self.delete(index, name_range.clone());
            } else if field(&new.name) != listed_field(&listed.name) {
                if git_edited {
                    let message = "Rename and edit the git status in separate writes";
                    self.problem(name_range.clone(), message, false);
                } else {
                    self.rename(entry, &path, &name, name_range.clone());
                }
            }
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
                    metadata.push(Metadata::Link(target));
                }
                None => self.problem(name_range, "A link needs a target", false),
            }
        }
        let changes = metadata.into_iter().map(|metadata| Change {
            path: path.clone(),
            metadata,
            pasted,
        });
        self.plan.changes.extend(changes);
        if self.plan.len() > planned {
            let range = self.line_range(line);
            self.touched.push(Touched {
                path: entry_path,
                id: entry.id,
                range,
                pasted,
            });
        }
    }

    /// The mode of `entry` the edited `octal` or symbolic `permissions` stand for, of which
    /// `changed` tells which were edited.
    fn mode(
        &mut self,
        entry: &Entry,
        octal: &str,
        permissions: &str,
        changed: [bool; 2],
        range: Range<usize>,
    ) -> Option<Metadata> {
        let octal = changed[0].then(|| format::parse_octal(octal));
        let permissions = changed[1].then(|| format::parse_permissions(permissions));
        let problem = match (octal, permissions) {
            (Some(None), _) => "Unreadable octal permissions",
            (_, Some(None)) => "Unreadable permissions",
            (_, Some(Some((kind, _)))) if kind != format::kind_letter(entry.kind) => {
                "The kind of an entry cannot be changed"
            }
            (Some(Some(octal)), Some(Some((_, mode)))) if octal != mode => {
                "The octal and the other permissions disagree"
            }
            (None, None) => return None,
            _ if entry.kind == Kind::Link => "Links have no permissions of their own",
            (Some(Some(mode)), _) | (None, Some(Some((_, mode)))) => {
                return (mode != entry.mode).then_some(Metadata::Mode(mode))
            }
        };
        self.problem(range, problem, false);
        None
    }

    /// The owner the edited `user` and `group`, names or ids, stand for.
    fn owner(
        &mut self,
        user: Option<&str>,
        group: Option<&str>,
        range: Range<usize>,
    ) -> Option<Metadata> {
        let uid = user.map(|user| user.parse().ok().or_else(|| user_id(user)));
        let gid = group.map(|group| group.parse().ok().or_else(|| group_id(group)));
        match (uid, gid) {
            (Some(None), _) => {
                let user = user.unwrap_or_default();
                self.problem(range, format!("No user is called `{user}`"), false);
                None
            }
            (_, Some(None)) => {
                let group = group.unwrap_or_default();
                self.problem(range, format!("No group is called `{group}`"), false);
                None
            }
            (uid, gid) => Some(Metadata::Owner {
                uid: uid.flatten(),
                gid: gid.flatten(),
            }),
        }
    }

    /// Plans the git edit turning the `status` of the entry at `path` into `text`.
    fn git(
        &mut self,
        entry: &Entry,
        path: &Path,
        status: GitStatus,
        text: &str,
        range: Range<usize>,
    ) {
        let new: Vec<char> = text.chars().collect();
        let [index, worktree] = new[..] else {
            return self.problem(range, "Unreadable git status", false);
        };
        let Some(repo) = self.listing.repo.clone() else {
            return;
        };
        let is_dir = entry.kind == Kind::Directory;
        match git::edit(status, (index, worktree)) {
            Err(message) => self.problem(range, message, false),
            Ok(None) => {}
            Ok(Some(git::Edit::Git(_))) if !self.trusted => {
                let message = "Git edits need a trusted workspace (use :workspace-trust)";
                self.problem(range, message, false);
            }
            Ok(Some(git::Edit::Git(action))) => {
                if action.discards() {
                    let name = format::quote(&format::name(entry));
                    let message = format!("Discards the changes of {name} (use :w! to apply)");
                    self.problem(range, message, true);
                }
                self.plan.git.push((action, path.to_path_buf()));
            }
            Ok(Some(git::Edit::Ignore)) => self.plan.ignores.push(git::ignore(&repo, path, is_dir)),
            Ok(Some(git::Edit::Unignore)) => match git::unignore(&repo, path, is_dir) {
                Ok(edit) => self.plan.ignores.push(edit),
                Err(message) => self.problem(range, message, false),
            },
        }
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
        let path = self.root.join(&entry.path);
        self.deleting.push((path.clone(), range.clone()));
        self.touched.push(Touched {
            path,
            id: entry.id,
            range,
            pasted: false,
        });
    }

    /// Checks the plan as a whole: what the moves run into, and what is still as listed.
    fn validate(&mut self) {
        // An entry deleted and pasted once is moved where it was pasted.
        let mut pastes: HashMap<&Path, usize> = HashMap::new();
        for copy in &self.plan.copies {
            *pastes.entry(copy.from.as_path()).or_default() += 1;
        }
        let moved: HashSet<PathBuf> = self
            .deleting
            .iter()
            .filter(|(path, _)| pastes.get(path.as_path()) == Some(&1))
            .map(|(path, _)| path.clone())
            .collect();
        if !moved.is_empty() {
            let copies = std::mem::take(&mut self.plan.copies);
            let ranges = std::mem::take(&mut self.copy_ranges);
            for (copy, range) in copies.into_iter().zip(ranges) {
                if copy.from == copy.to {
                    // Pasted back where it was cut.
                } else if moved.contains(&copy.from) {
                    self.plan.moves.push(copy);
                    self.move_ranges.push(range);
                } else {
                    self.plan.copies.push(copy);
                    self.copy_ranges.push(range);
                }
            }
        }
        for (path, range) in std::mem::take(&mut self.deleting) {
            if moved.contains(&path) {
                continue;
            }
            let name = format::quote(
                &path
                    .strip_prefix(self.root)
                    .unwrap_or(&path)
                    .to_string_lossy(),
            );
            self.problem(range, format!("Deletes {name} (use :w! to apply)"), true);
            self.plan.deletions.push(path);
        }

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

        let (moves, copies) = (&self.plan.moves, &self.plan.copies);
        let sources: HashSet<&Path> = moves.iter().map(|step| step.from.as_path()).collect();
        let targets: HashSet<&Path> = moves
            .iter()
            .chain(copies)
            .map(|step| step.to.as_path())
            .collect();
        // Each target, and whether a copy goes there.
        let mut seen: HashMap<&Path, bool> = HashMap::with_capacity(moves.len() + copies.len());
        // Many moves stay in one directory, which is looked up once.
        let mut directories: HashMap<&Path, bool> = HashMap::new();
        let mut problems = Vec::new();
        let steps = moves
            .iter()
            .zip(&self.move_ranges)
            .map(|(step, range)| (step, range, false))
            .chain(
                copies
                    .iter()
                    .zip(&self.copy_ranges)
                    .map(|(step, range)| (step, range, true)),
            );
        for (Move { from, to }, range, copy) in steps {
            let mut problem = |message: String, forced| {
                problems.push(Problem {
                    range: range.clone(),
                    message,
                    forced,
                })
            };
            let missing_parent = to.parent().filter(|parent| {
                !*directories.entry(parent).or_insert_with(|| parent.is_dir())
                    && !targets.contains(parent)
                    && !sources.contains(parent)
            });
            if let Some(&other_copy) = seen.get(to.as_path()) {
                let verb = if other_copy { "is copied" } else { "moves" };
                problem(format!("Another entry {verb} to {} too", shown(to)), false);
                continue;
            }
            seen.insert(to, copy);
            if to != from && to.starts_with(from) {
                let verb = if copy { "be copied" } else { "move" };
                problem(format!("A directory cannot {verb} into itself"), false);
            } else if deleted.iter().any(|dir| to.starts_with(dir)) {
                problem(format!("{} is deleted by the same write", shown(to)), false);
            } else if fs::symlink_metadata(to).is_ok() && !sources.contains(to.as_path()) {
                problem(format!("{} already exists", shown(to)), false);
            } else if let Some(parent) = missing_parent {
                let parent = shown(parent);
                problem(
                    format!("Creates the directory {parent} (use :w! to apply)"),
                    true,
                );
            }
        }
        self.problems.extend(problems);

        for touched in std::mem::take(&mut self.touched) {
            let Touched {
                path,
                id,
                range,
                pasted,
            } = touched;
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
            if !still_listed(&path, id) {
                let message = if pasted {
                    "Changed on disk since it was yanked"
                } else {
                    "Changed on disk since it was listed (use :reload)"
                };
                self.problem(range, message, false);
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
    within(path.parent().unwrap_or(path), name)
}

/// The path `name` stands for in `dir`.
fn within(dir: &Path, name: &str) -> PathBuf {
    let name = helix_stdx::path::expand_tilde(Path::new(name));
    helix_stdx::path::normalize(dir.join(name))
}

/// Whether `files`, as listed at their paths, are regular files of up to a MiB holding the same.
fn same_files<'a>(mut files: impl Iterator<Item = (&'a PathBuf, &'a Entry)>) -> bool {
    const MAX: u64 = 1 << 20;
    let Some((path, entry)) = files.next() else {
        return false;
    };
    let file = |entry: &Entry| match entry.size {
        Size::Bytes(size) if entry.kind == Kind::File && size <= MAX => Some(size),
        _ => None,
    };
    let (Some(size), Ok(contents)) = (file(entry), fs::read(path)) else {
        return false;
    };
    files.all(|(path, entry)| {
        file(entry) == Some(size) && fs::read(path).is_ok_and(|other| other == contents)
    })
}

/// Whether `path` still holds the file of the identity `id` that was listed there.
fn still_listed(path: &Path, id: (u64, u64)) -> bool {
    let Ok(metadata) = fs::symlink_metadata(path) else {
        return false;
    };
    identity(&metadata) == id
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

    use super::super::paste::{self, Origins};
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
        (
            lines(&listed, text.slice(..), &changes, &HashSet::new()),
            text.to_string(),
        )
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
    fn lines_retyped_like_helix_does_edit_their_entries() {
        // `xc` on `b` deletes its line, then opens one above the next: a line break at the end
        // of `a`, so that `a`'s own line break ends the typed text.
        let (lines, text) = lines_after(LISTED, &[(2, 4, ""), (1, 1, "\n"), (2, 2, "B")]);
        assert_eq!(text, "a\nB\nc\nd\n");
        assert_eq!(lines.entries, [Some(0), Some(1), Some(2), Some(3)]);
        assert!(lines.added.is_empty() && lines.joined.is_empty());
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
        assert_eq!(lines.entries, [Some(0), None, Some(1), Some(2)]);
        assert_eq!(lines.joined, [(0, 1)]);
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
            &[],
            &Clock::system(),
            true,
        )
    }

    /// Plans the char edits `(from, to, replacement)` of `listing`, one transaction each, which
    /// may paste lines of `listing` or of `others`.
    fn plan_edits(
        listing: &Listing,
        others: &[&Listing],
        edits: &[(usize, usize, &str)],
    ) -> (Plan, Vec<Problem>) {
        let mut text = listing.text.clone();
        let mut changes = ChangeSet::new(text.slice(..));
        let mut transactions = Vec::new();
        for &(from, to, replacement) in edits {
            let transaction =
                Transaction::change(&text, [(from, to, Some(replacement.into()))].into_iter());
            transaction.apply(&mut text);
            changes = changes.compose(transaction.changes().clone());
            transactions.push(transaction);
        }
        let origins = Origins::new(std::iter::once(listing).chain(others.iter().copied()));
        let pasted = paste::pasted(&listing.text, &transactions, &origins);
        plan(
            listing,
            text.slice(..),
            &changes,
            &pasted,
            &Clock::system(),
            true,
        )
    }

    /// The text of line `line` of `listing`, with its line break, and the chars it starts at.
    fn line(listing: &Listing, line: usize) -> (String, usize) {
        let text = &listing.text;
        (text.line(line).to_string(), text.line_to_char(line))
    }

    /// The chars `text` has before `pattern`.
    fn chars_before(text: &str, pattern: &str) -> usize {
        text[..text.find(pattern).unwrap()].chars().count()
    }

    /// The line of `listing` naming `name`.
    fn line_of(listing: &Listing, name: &str) -> usize {
        (0..listing.entries.len())
            .find(|&line| listing.entries[line].path.file_name() == Some(name.as_ref()))
            .unwrap()
    }

    /// A tree listing of a temporary directory holding `sub/inner.txt` and `a.txt`, with `sub`
    /// expanded.
    fn tree_listing() -> (tempfile::TempDir, Listing) {
        let dir = tempfile::tempdir().unwrap();
        let root = helix_stdx::path::canonicalize(dir.path());
        fs::create_dir(root.join("sub")).unwrap();
        fs::write(root.join("sub/inner.txt"), "inner").unwrap();
        fs::write(root.join("a.txt"), "a").unwrap();
        let options = super::super::listing::Options {
            sort: helix_view::editor::FileTreeSort::DirectoriesFirst,
            icons: false,
            providers: helix_vcs::DiffProviderRegistry::default(),
            trust_git: true,
        };
        let source = Source::Tree {
            root,
            expanded: [PathBuf::from("sub")].into(),
        };
        let mut listing = super::super::listing::read(&source, &options);
        listing.text = Rope::from(format::text(&listing, &Clock::system()));
        (dir, listing)
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
                pasted: false,
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

    #[test]
    fn pasted_lines_copy_their_entries() {
        let (_dir, listing) = listing();
        let root = listing.source.root().to_path_buf();
        // `sub`, `a.txt`, `b.txt`: `b.txt` pasted below itself, renamed and given another mode.
        let (b, start) = line(&listing, 2);
        let end = start + b.chars().count();
        let name = end + b.find("b.txt").unwrap();
        let octal = end + b.find("0644").unwrap();
        let edits = [
            (end, end, b.as_str()),
            (name, name + 5, "c.txt"),
            (octal, octal + 4, "0600"),
        ];
        let (plan, problems) = plan_edits(&listing, &[], &edits);
        assert_eq!(messages(&problems), []);
        let copy = Move {
            from: root.join("b.txt"),
            to: root.join("c.txt"),
        };
        assert_eq!(plan.copies, [copy]);
        let mode = Change {
            path: root.join("c.txt"),
            metadata: Metadata::Mode(0o600),
            pasted: true,
        };
        assert_eq!(plan.changes, [mode]);

        // Pasted as it is, the copy would take the name of its entry.
        let (plan, problems) = plan_edits(&listing, &[], &edits[..1]);
        assert_eq!(plan.copies.len(), 1);
        assert!(problems[0].message.ends_with("b.txt already exists"));
        // Nor where another entry moves.
        let (a, a_start) = line(&listing, 1);
        let a_name = a_start + a.find("a.txt").unwrap();
        let rename = (a_name, a_name + 5, "c.txt");
        let (_, problems) = plan_edits(&listing, &[], &[edits[0], edits[1], rename]);
        assert!(problems[0].message.starts_with("Another entry moves to "));
        // Its size cannot be edited.
        let size = end + b.find(" 1 ").unwrap() + 1;
        let (_, problems) = plan_edits(&listing, &[], &[edits[0], edits[1], (size, size + 1, "2")]);
        assert_eq!(messages(&problems), [("The size cannot be edited", false)]);
    }

    #[test]
    fn pasted_lines_copy_into_the_directory_of_the_line_above() {
        let (_dir, listing) = tree_listing();
        let root = listing.source.root().to_path_buf();
        // `.`, `sub`, `inner.txt`, `a.txt`, the guides of `a.txt` left as they are.
        let (a, _) = line(&listing, line_of(&listing, "a.txt"));
        let copy = |line: usize| {
            let start = listing.text.line_to_char(line);
            let (plan, problems) = plan_edits(&listing, &[], &[(start, start, a.as_str())]);
            assert_eq!(messages(&problems), []);
            plan.copies[0].to.clone()
        };
        // Below the expanded `sub` and below `inner.txt`, into `sub`.
        assert_eq!(copy(line_of(&listing, "sub") + 1), root.join("sub/a.txt"));
        assert_eq!(
            copy(line_of(&listing, "inner.txt") + 1),
            root.join("sub/a.txt")
        );

        // Yanked with its guides edited: below the root line, into the root, by another name.
        let start = listing.text.line_to_char(1);
        let edited = a.replacen("└── ", "│   ├──", 1);
        assert_ne!(edited, a);
        let name = start + chars_before(&edited, "a.txt");
        let edits = [
            (start, start, edited.as_str()),
            (name, name + 5, "../b.txt"),
        ];
        let (plan, problems) = plan_edits(&listing, &[], &edits);
        assert_eq!(messages(&problems), []);
        assert_eq!(plan.copies[0].to, root.parent().unwrap().join("b.txt"));
    }

    #[test]
    fn cut_and_pasted_lines_move_their_entries() {
        let (_dir, listing) = tree_listing();
        let root = listing.source.root().to_path_buf();
        let (a, a_start) = line(&listing, line_of(&listing, "a.txt"));
        let a_end = a_start + a.chars().count();
        let below_sub = listing.text.line_to_char(line_of(&listing, "sub") + 1);
        // Cut, then pasted into `sub`: a move, no `:w!` needed.
        let cut = (a_start, a_end, "");
        let edits = [cut, (below_sub, below_sub, a.as_str())];
        let (plan, problems) = plan_edits(&listing, &[], &edits);
        assert_eq!(messages(&problems), []);
        let to = root.join("sub/a.txt");
        assert_eq!(
            plan.moves,
            [Move {
                from: root.join("a.txt"),
                to
            }]
        );
        assert!(plan.copies.is_empty() && plan.deletions.is_empty());

        // Pasted back where it was, it goes below `inner.txt`, into `sub`: the line above
        // decides, not the guides.
        let (plan, _) = plan_edits(&listing, &[], &[cut, (a_start, a_start, a.as_str())]);
        assert_eq!(plan.moves[0].to, root.join("sub/a.txt"));
        // Pasted back where it was in a flat listing: nothing to do.
        let (_dir, flat) = self::listing();
        let (a, a_start) = line(&flat, line_of(&flat, "a.txt"));
        let cut_flat = (a_start, a_start + a.chars().count(), "");
        let (plan, problems) = plan_edits(&flat, &[], &[cut_flat, (a_start, a_start, a.as_str())]);
        assert_eq!(messages(&problems), []);
        assert!(plan.is_empty(), "{plan:?}");
        let (a, _) = line(&listing, line_of(&listing, "a.txt"));

        // Pasted twice: two copies, and the deletion takes `:w!`.
        let mut twice = edits.to_vec();
        twice.push((below_sub, below_sub, a.as_str()));
        let name = below_sub + chars_before(&a, "a.txt");
        twice.push((name, name + 5, "b.txt"));
        let (plan, problems) = plan_edits(&listing, &[], &twice);
        assert_eq!(plan.copies.len(), 2);
        assert_eq!(plan.deletions, [root.join("a.txt")]);
        assert_eq!(
            messages(&problems),
            [("Deletes a.txt (use :w! to apply)", true)]
        );
    }

    #[test]
    fn lines_pasted_from_other_listings_copy_here() {
        let (_dir, listing) = listing();
        let (_other_dir, other) = tree_listing();
        let (inner, _) = line(&other, line_of(&other, "inner.txt"));
        let end = listing.text.len_chars();
        let (plan, problems) = plan_edits(&listing, &[&other], &[(end, end, inner.as_str())]);
        assert_eq!(messages(&problems), []);
        let copy = Move {
            from: other.source.root().join("sub/inner.txt"),
            to: listing.source.root().join("inner.txt"),
        };
        assert_eq!(plan.copies, [copy]);
    }

    #[test]
    fn lines_reading_alike_are_told_apart_by_a_cut() {
        let dir = tempfile::tempdir().unwrap();
        let root = helix_stdx::path::canonicalize(dir.path());
        let time = SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(1 << 30);
        for sub in ["x", "y"] {
            fs::create_dir(root.join(sub)).unwrap();
            fs::write(root.join(sub).join("a"), "a").unwrap();
            helix_stdx::fs::set_modified(&root.join(sub).join("a"), time).unwrap();
        }
        fs::write(root.join("z"), "z").unwrap();
        let options = super::super::listing::Options {
            sort: helix_view::editor::FileTreeSort::DirectoriesFirst,
            icons: false,
            providers: helix_vcs::DiffProviderRegistry::default(),
            trust_git: true,
        };
        let source = Source::Tree {
            root: root.clone(),
            expanded: [PathBuf::from("x"), PathBuf::from("y")].into(),
        };
        let mut listing = super::super::listing::read(&source, &options);
        listing.text = Rope::from(format::text(&listing, &Clock::system()));
        // `.`, `x`, `a`, `y`, `a`, `z`
        let (a, x_a) = line(&listing, 2);
        assert_eq!(a, listing.text.line(4).to_string());
        let end = listing.text.len_chars();
        let name = end + chars_before(&a, " a\n") + 1;
        let paste = [(end, end, a.as_str()), (name, name + 1, "b")];
        let (plan, problems) = plan_edits(&listing, &[], &paste);
        assert_eq!(messages(&problems), []);
        assert_eq!(
            plan.copies.len(),
            1,
            "files holding the same make the same copy"
        );
        // Unless they hold different things.
        fs::write(root.join("y/a"), "b").unwrap();
        let (plan, problems) = plan_edits(&listing, &[], &paste);
        assert!(plan.copies.is_empty());
        assert_eq!(
            messages(&problems),
            [("Reads like more than one listed entry", false)]
        );
        // Cut one of them, and the paste is that one.
        let cut = (x_a, x_a + a.chars().count(), "");
        let end = end - a.chars().count();
        let (plan, problems) = plan_edits(&listing, &[], &[cut, (end, end, a.as_str())]);
        assert_eq!(messages(&problems), []);
        assert_eq!(
            plan.moves,
            [Move {
                from: root.join("x/a"),
                to: root.join("a")
            }]
        );
    }

    #[test]
    fn lines_reading_alike_are_told_apart_by_the_yank() {
        let dir = tempfile::tempdir().unwrap();
        let root = helix_stdx::path::canonicalize(dir.path());
        let time = SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(1 << 30);
        for (sub, contents) in [("x", "x"), ("y", "y")] {
            fs::create_dir(root.join(sub)).unwrap();
            fs::write(root.join(sub).join("a"), contents).unwrap();
            helix_stdx::fs::set_modified(&root.join(sub).join("a"), time).unwrap();
        }
        let options = super::super::listing::Options {
            sort: helix_view::editor::FileTreeSort::DirectoriesFirst,
            icons: false,
            providers: helix_vcs::DiffProviderRegistry::default(),
            trust_git: true,
        };
        let [mut x, y] = ["x", "y"].map(|sub| {
            let source = Source::Directory(root.join(sub));
            let mut listing = super::super::listing::read(&source, &options);
            listing.text = Rope::from(format::text(&listing, &Clock::system()));
            listing
        });
        assert_eq!(x.text, y.text);
        // `x/a` pasted into `y` as `b`.
        let (a, _) = line(&x, 0);
        let end = y.text.len_chars();
        let paste = [
            (end, end, a.as_str()),
            (end + a.len() - 2, end + a.len() - 1, "b"),
        ];
        let (plan, problems) = plan_edits(&y, &[&x], &paste);
        assert!(plan.copies.is_empty());
        assert_eq!(
            messages(&problems),
            [("Reads like more than one listed entry", false)]
        );
        x.yanked = Some(helix_view::dired::Yanked {
            write: 1,
            paths: [PathBuf::from("a")].into(),
        });
        let (plan, problems) = plan_edits(&y, &[&x], &paste);
        assert_eq!(messages(&problems), []);
        let copy = Move {
            from: root.join("x/a"),
            to: root.join("y/b"),
        };
        assert_eq!(plan.copies, [copy]);
    }
}
