use crate::{Assoc, ChangeSet, Range, Rope, Selection, Transaction};
use once_cell::sync::Lazy;
use regex::Regex;
use std::collections::HashSet;
use std::num::NonZeroUsize;
use std::time::{Duration, SystemTime};

pub mod undo_file;

#[derive(Debug, Clone)]
pub struct State {
    pub doc: Rope,
    pub selection: Selection,
}

/// Stores the history of changes to a buffer.
///
/// Currently the history is represented as a vector of revisions. The vector
/// always has at least one element: the empty root revision. Each revision
/// with the exception of the root has a parent revision, a [Transaction]
/// that can be applied to its parent to transition from the parent to itself,
/// and an inversion of that transaction to transition from the parent to its
/// latest child.
///
/// When using `u` to undo a change, an inverse of the stored transaction will
/// be applied which will transition the buffer to the parent state.
///
/// Each revision with children also has a last child revision. When using `U` to redo a
/// change, the last child transaction will be applied to the current state of the buffer.
/// Redo follows the branch visited last.
///
/// The current revision is the one currently displayed in the buffer.
///
/// Committing a new revision to the history will update the last child of the
/// current revision, and push a new revision to the end of the vector.
///
/// Revisions are committed with a timestamp. :earlier and :later can be used
/// to jump to the closest revision to a moment in time relative to the timestamp
/// of the current revision plus (:later) or minus (:earlier) the duration
/// given to the command. If a single integer is given, the editor will instead
/// jump the given number of revisions in the vector. With a number of file writes
/// like `2f`, they jump to the revision written that many writes before or after.
///
/// Limitations:
///  * Changes in selections currently don't commit history changes. The selection
///    will only be updated to the state after a committed buffer change.
///  * The vector of history revisions is currently unbounded. This might
///    cause the memory consumption to grow significantly large during long
///    editing sessions.
///  * Because delete transactions currently don't store the text that they
///    delete, we also store an inversion of the transaction.
///
/// Using time to navigate the history: <https://github.com/helix-editor/helix/pull/194>
#[derive(Debug)]
pub struct History {
    revisions: Vec<Revision>,
    current: usize,
    /// The revisions written to the file, in the order of the writes.
    saves: Vec<usize>,
}

/// A single point in history. See [History] for more information.
#[derive(Debug, Clone)]
struct Revision {
    parent: usize,
    last_child: Option<NonZeroUsize>,
    transaction: Transaction,
    // We need an inversion for undos because delete transactions don't store
    // the deleted text.
    inversion: Transaction,
    timestamp: SystemTime,
}

impl Default for History {
    fn default() -> Self {
        // Add a dummy root revision with empty transaction
        Self {
            revisions: vec![Revision {
                parent: 0,
                last_child: None,
                transaction: Transaction::from(ChangeSet::new("".into())),
                inversion: Transaction::from(ChangeSet::new("".into())),
                timestamp: SystemTime::now(),
            }],
            current: 0,
            saves: Vec::new(),
        }
    }
}

impl History {
    pub fn commit_revision(&mut self, transaction: &Transaction, original: &State) {
        self.commit_revision_at_timestamp(transaction, original, SystemTime::now());
    }

    pub fn commit_revision_at_timestamp(
        &mut self,
        transaction: &Transaction,
        original: &State,
        timestamp: SystemTime,
    ) {
        let inversion = transaction
            .invert(&original.doc)
            // Store the current cursor position
            .with_selection(original.selection.clone());

        let new_current = self.revisions.len();
        self.revisions[self.current].last_child = NonZeroUsize::new(new_current);
        self.revisions.push(Revision {
            parent: self.current,
            last_child: None,
            transaction: transaction.clone(),
            inversion,
            timestamp,
        });
        self.current = new_current;
    }

    #[inline]
    pub fn current_revision(&self) -> usize {
        self.current
    }

    /// The number of revisions, the root included.
    #[allow(clippy::len_without_is_empty)] // a history always has its root
    #[inline]
    pub fn len(&self) -> usize {
        self.revisions.len()
    }

    /// The revision `revision` was made from; the root's is itself.
    pub fn parent(&self, revision: usize) -> usize {
        self.revisions[revision].parent
    }

    /// The child of `revision` that redo goes to.
    pub fn last_child(&self, revision: usize) -> Option<usize> {
        self.revisions[revision].last_child.map(NonZeroUsize::get)
    }

    /// When `revision` was made.
    pub fn timestamp(&self, revision: usize) -> SystemTime {
        self.revisions[revision].timestamp
    }

    /// The transaction that made `revision` from its parent, and its inversion.
    pub fn changes(&self, revision: usize) -> (&Transaction, &Transaction) {
        let revision = &self.revisions[revision];
        (&revision.transaction, &revision.inversion)
    }

    /// Creates the [`Transaction`]s that go to `revision`, which becomes the current one.
    pub fn jump(&mut self, revision: usize) -> Vec<Transaction> {
        self.jump_to(revision)
    }

    #[inline]
    pub const fn at_root(&self) -> bool {
        self.current == 0
    }

    /// Notes that `revision` was written to the file.
    pub fn record_save(&mut self, revision: usize) {
        if self.saves.last() != Some(&revision) {
            self.saves.push(revision);
        }
    }

    /// The revisions written to the file, in the order of the writes.
    pub fn saves(&self) -> &[usize] {
        &self.saves
    }

    /// Returns the changes since the given revision composed into a transaction.
    /// Returns None if there are no changes between the current and given revisions.
    pub fn changes_since(&self, revision: usize) -> Option<Transaction> {
        let lca = self.lowest_common_ancestor(revision, self.current);
        let up = self.path_up(revision, lca);
        let down = self.path_up(self.current, lca);
        let up_txns = up
            .iter()
            .rev()
            .map(|&n| self.revisions[n].inversion.clone());
        let down_txns = down.iter().map(|&n| self.revisions[n].transaction.clone());

        down_txns.chain(up_txns).reduce(|acc, tx| tx.compose(acc))
    }

    /// The transactions leading from `revision` to the current revision, in the order they apply.
    pub fn transactions_since(&self, revision: usize) -> Vec<Transaction> {
        let lca = self.lowest_common_ancestor(revision, self.current);
        let up = self.path_up(revision, lca);
        let down = self.path_up(self.current, lca);
        up.iter()
            .map(|&n| self.revisions[n].inversion.clone())
            .chain(
                down.iter()
                    .rev()
                    .map(|&n| self.revisions[n].transaction.clone()),
            )
            .collect()
    }

    /// Undo the last edit.
    pub fn undo(&mut self) -> Option<&Transaction> {
        if self.at_root() {
            return None;
        }

        let current = self.current;
        let parent = self.revisions[current].parent;
        // Redo comes back here, also from a branch older than the parent's newest.
        self.revisions[parent].last_child = NonZeroUsize::new(current);
        self.current = parent;
        Some(&self.revisions[current].inversion)
    }

    /// Redo the last edit.
    pub fn redo(&mut self) -> Option<&Transaction> {
        let current_revision = &self.revisions[self.current];
        let last_child = current_revision.last_child?;
        self.current = last_child.get();

        Some(&self.revisions[last_child.get()].transaction)
    }

    // Get the position of last change
    pub fn last_edit_pos(&self) -> Option<usize> {
        if self.current == 0 {
            return None;
        }
        let current_revision = &self.revisions[self.current];
        let primary_selection = current_revision
            .inversion
            .selection()
            .expect("inversion always contains a selection")
            .primary();
        let (_from, to, _fragment) = current_revision
            .transaction
            .changes_iter()
            // find a change that matches the primary selection
            .find(|(from, to, _fragment)| Range::new(*from, *to).overlaps(&primary_selection))
            // or use the first change
            .or_else(|| current_revision.transaction.changes_iter().next())
            .unwrap();
        let pos = current_revision
            .transaction
            .changes()
            .map_pos(to, Assoc::After);
        Some(pos)
    }

    fn lowest_common_ancestor(&self, mut a: usize, mut b: usize) -> usize {
        let mut a_path_set = HashSet::new();
        let mut b_path_set = HashSet::new();
        loop {
            a_path_set.insert(a);
            b_path_set.insert(b);
            if a_path_set.contains(&b) {
                return b;
            }
            if b_path_set.contains(&a) {
                return a;
            }
            a = self.revisions[a].parent; // Relies on the parent of 0 being 0.
            b = self.revisions[b].parent; // Same as above.
        }
    }

    /// List of nodes on the way from `n` to 'a`. Doesn't include `a`.
    /// Includes `n` unless `a == n`. `a` must be an ancestor of `n`.
    fn path_up(&self, mut n: usize, a: usize) -> Vec<usize> {
        let mut path = Vec::new();
        while n != a {
            path.push(n);
            n = self.revisions[n].parent;
        }
        path
    }

    /// Create a [`Transaction`] that will jump to a specific revision in the history.
    fn jump_to(&mut self, to: usize) -> Vec<Transaction> {
        let lca = self.lowest_common_ancestor(self.current, to);
        let up = self.path_up(self.current, lca);
        let down = self.path_up(to, lca);
        self.current = to;
        // Like Vim, redo then follows the way taken: back up the branch left, down the one entered.
        for &n in up.iter().chain(&down) {
            let parent = self.revisions[n].parent;
            self.revisions[parent].last_child = NonZeroUsize::new(n);
        }
        let up_txns = up.iter().map(|&n| self.revisions[n].inversion.clone());
        let down_txns = down
            .iter()
            .rev()
            .map(|&n| self.revisions[n].transaction.clone());
        up_txns.chain(down_txns).collect()
    }

    /// Creates a [`Transaction`] that will undo `delta` revisions.
    fn jump_backward(&mut self, delta: usize) -> Vec<Transaction> {
        self.jump_to(self.current.saturating_sub(delta))
    }

    /// Creates a [`Transaction`] that will redo `delta` revisions.
    fn jump_forward(&mut self, delta: usize) -> Vec<Transaction> {
        self.jump_to(
            self.current
                .saturating_add(delta)
                .min(self.revisions.len() - 1),
        )
    }

    /// Helper for a binary search case below.
    fn revision_closer_to_instant(&self, i: usize, instant: SystemTime) -> usize {
        let dur_im1 = instant
            .duration_since(self.revisions[i - 1].timestamp)
            .unwrap_or_default();
        let dur_i = self.revisions[i]
            .timestamp
            .duration_since(instant)
            .unwrap_or_default();
        use std::cmp::Ordering::*;
        match dur_im1.cmp(&dur_i) {
            Less => i - 1,
            Equal | Greater => i,
        }
    }

    /// Creates a [`Transaction`] that will match a revision created at around
    /// `instant`.
    fn jump_instant(&mut self, instant: SystemTime) -> Vec<Transaction> {
        let search_result = self
            .revisions
            .binary_search_by(|rev| rev.timestamp.cmp(&instant));
        let revision = match search_result {
            Ok(revision) => revision,
            Err(insert_point) => match insert_point {
                0 => 0,
                n if n == self.revisions.len() => n - 1,
                i => self.revision_closer_to_instant(i, instant),
            },
        };
        self.jump_to(revision)
    }

    /// Creates a [`Transaction`] that will match a revision created `duration` ago
    /// from the timestamp of current revision.
    fn jump_duration_backward(&mut self, duration: Duration) -> Vec<Transaction> {
        match self.revisions[self.current].timestamp.checked_sub(duration) {
            Some(instant) => self.jump_instant(instant),
            None => self.jump_to(0),
        }
    }

    /// Creates a [`Transaction`] that will match a revision created `duration` in
    /// the future from the timestamp of the current revision.
    fn jump_duration_forward(&mut self, duration: Duration) -> Vec<Transaction> {
        match self.revisions[self.current].timestamp.checked_add(duration) {
            Some(instant) => self.jump_instant(instant),
            None => self.jump_to(self.revisions.len() - 1),
        }
    }

    /// The number of the write the current revision is at or after, counting from 1, and whether
    /// the current revision is that write's.
    fn current_write(&self) -> (usize, bool) {
        let mut ancestors: HashSet<usize> = self.path_up(self.current, 0).into_iter().collect();
        ancestors.insert(0);
        let number = self
            .saves
            .iter()
            .rposition(|revision| ancestors.contains(revision))
            .map_or(0, |index| index + 1);
        let written = number > 0 && self.saves[number - 1] == self.current;
        (number, written)
    }

    /// Creates the [`Transaction`]s that go back `writes` file writes.
    fn jump_writes_backward(&mut self, writes: usize) -> Vec<Transaction> {
        let (current, written) = self.current_write();
        let target = if written { current } else { current + 1 };
        match target.checked_sub(writes).filter(|&target| target > 0) {
            Some(target) => self.jump_to(self.saves[target - 1]),
            None => self.jump_to(0),
        }
    }

    /// Creates the [`Transaction`]s that go forward `writes` file writes.
    fn jump_writes_forward(&mut self, writes: usize) -> Vec<Transaction> {
        let target = self.current_write().0.saturating_add(writes);
        // Writes count from 1.
        match target
            .checked_sub(1)
            .and_then(|index| self.saves.get(index))
        {
            Some(&revision) => self.jump_to(revision),
            None => self.jump_to(self.revisions.len() - 1),
        }
    }

    /// Creates an undo [`Transaction`].
    pub fn earlier(&mut self, uk: UndoKind) -> Vec<Transaction> {
        use UndoKind::*;
        match uk {
            Steps(n) => self.jump_backward(n),
            TimePeriod(d) => self.jump_duration_backward(d),
            FileWrites(n) => self.jump_writes_backward(n),
        }
    }

    /// Creates a redo [`Transaction`].
    pub fn later(&mut self, uk: UndoKind) -> Vec<Transaction> {
        use UndoKind::*;
        match uk {
            Steps(n) => self.jump_forward(n),
            TimePeriod(d) => self.jump_duration_forward(d),
            FileWrites(n) => self.jump_writes_forward(n),
        }
    }
}

/// Whether to undo by a number of edits, a duration of time or a number of file writes.
#[derive(Debug, PartialEq, Eq, Clone, Copy)]
pub enum UndoKind {
    Steps(usize),
    TimePeriod(std::time::Duration),
    FileWrites(usize),
}

/// A subset of systemd.time time span syntax units.
const TIME_UNITS: &[(&[&str], &str, u64)] = &[
    (&["seconds", "second", "sec", "s"], "seconds", 1),
    (&["minutes", "minute", "min", "m"], "minutes", 60),
    (&["hours", "hour", "hr", "h"], "hours", 60 * 60),
    (&["days", "day", "d"], "days", 24 * 60 * 60),
];

/// Checks if the duration input can be turned into a valid duration. It must be a
/// positive integer and denote the [unit of time.](`TIME_UNITS`)
/// Examples of valid durations:
///  * `5 sec`
///  * `5 min`
///  * `5 hr`
///  * `5 days`
static DURATION_VALIDATION_REGEX: Lazy<Regex> =
    Lazy::new(|| Regex::new(r"^(?:\d+\s*[a-z]+\s*)+$").unwrap());

/// A number of file writes, like `2f`.
static FILE_WRITES_REGEX: Lazy<Regex> = Lazy::new(|| Regex::new(r"^(\d+)\s*f$").unwrap());

/// Captures both the number and unit as separate capture groups.
static NUMBER_UNIT_REGEX: Lazy<Regex> = Lazy::new(|| Regex::new(r"(\d+)\s*([a-z]+)").unwrap());

/// Parse a string (e.g. "5 sec") and try to convert it into a [`Duration`].
fn parse_human_duration(s: &str) -> Result<Duration, String> {
    if !DURATION_VALIDATION_REGEX.is_match(s) {
        return Err("duration should be composed \
        of positive integers followed by time units"
            .to_string());
    }

    let mut specified = [false; TIME_UNITS.len()];
    let mut seconds = 0u64;
    for cap in NUMBER_UNIT_REGEX.captures_iter(s) {
        let (n, unit_str) = (&cap[1], &cap[2]);

        let n: u64 = n.parse().map_err(|_| format!("integer too large: {}", n))?;

        let time_unit = TIME_UNITS
            .iter()
            .enumerate()
            .find(|(_, (forms, _, _))| forms.iter().any(|f| f == &unit_str));

        if let Some((i, (_, unit, mul))) = time_unit {
            if specified[i] {
                return Err(format!("{} specified more than once", unit));
            }
            specified[i] = true;

            let new_seconds = n.checked_mul(*mul).and_then(|s| seconds.checked_add(s));
            match new_seconds {
                Some(ns) => seconds = ns,
                None => return Err("duration too large".to_string()),
            }
        } else {
            return Err(format!("incorrect time unit: {}", unit_str));
        }
    }

    Ok(Duration::from_secs(seconds))
}

impl std::str::FromStr for UndoKind {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let s = s.trim();
        if s.is_empty() {
            Ok(Self::Steps(1usize))
        } else if let Ok(n) = s.parse::<usize>() {
            Ok(UndoKind::Steps(n))
        } else if let Some(writes) = FILE_WRITES_REGEX.captures(s) {
            let n = &writes[1];
            n.parse()
                .map(UndoKind::FileWrites)
                .map_err(|_| format!("integer too large: {n}"))
        } else {
            Ok(Self::TimePeriod(parse_human_duration(s)?))
        }
    }
}

#[cfg(test)]
mod test {
    use super::*;
    use crate::Selection;

    #[test]
    fn transactions_since_lead_across_branches() {
        let mut history = History::default();
        let mut state = State {
            doc: Rope::from("a"),
            selection: Selection::point(0),
        };
        let commit = |history: &mut History, state: &mut State, text: &str| {
            let end = state.doc.len_chars();
            let transaction =
                Transaction::change(&state.doc, [(end, end, Some(text.into()))].into_iter());
            history.commit_revision(&transaction, state);
            transaction.apply(&mut state.doc);
        };
        commit(&mut history, &mut state, "b");
        commit(&mut history, &mut state, "c");
        // Undo `c` and branch off with `d`.
        let undo = history.undo().unwrap().clone();
        undo.apply(&mut state.doc);
        commit(&mut history, &mut state, "d");
        assert_eq!(state.doc, "abd");

        // From `abc`, revision 2: back to `ab`, then to `abd`, one step after another.
        let mut doc = Rope::from("abc");
        let transactions = history.transactions_since(2);
        assert_eq!(transactions.len(), 2);
        for transaction in &transactions {
            transaction.apply(&mut doc);
        }
        assert_eq!(doc, "abd");
        assert!(history
            .transactions_since(history.current_revision())
            .is_empty());
    }

    #[test]
    fn test_undo_redo() {
        let mut history = History::default();
        let doc = Rope::from("hello");
        let mut state = State {
            doc,
            selection: Selection::point(0),
        };

        let transaction1 =
            Transaction::change(&state.doc, vec![(5, 5, Some(" world!".into()))].into_iter());

        // Need to commit before applying!
        history.commit_revision(&transaction1, &state);
        transaction1.apply(&mut state.doc);
        assert_eq!("hello world!", state.doc);

        // ---

        let transaction2 =
            Transaction::change(&state.doc, vec![(6, 11, Some("世界".into()))].into_iter());

        // Need to commit before applying!
        history.commit_revision(&transaction2, &state);
        transaction2.apply(&mut state.doc);
        assert_eq!("hello 世界!", state.doc);

        // ---
        fn undo(history: &mut History, state: &mut State) {
            if let Some(transaction) = history.undo() {
                transaction.apply(&mut state.doc);
            }
        }
        fn redo(history: &mut History, state: &mut State) {
            if let Some(transaction) = history.redo() {
                transaction.apply(&mut state.doc);
            }
        }

        undo(&mut history, &mut state);
        assert_eq!("hello world!", state.doc);
        redo(&mut history, &mut state);
        assert_eq!("hello 世界!", state.doc);
        undo(&mut history, &mut state);
        undo(&mut history, &mut state);
        assert_eq!("hello", state.doc);

        // undo at root is a no-op
        undo(&mut history, &mut state);
        assert_eq!("hello", state.doc);
    }

    /// Applies what `history` hands out for a step to `state`.
    fn apply(state: &mut State, transactions: &[Transaction]) {
        for transaction in transactions {
            transaction.apply(&mut state.doc);
        }
    }

    fn undo(history: &mut History, state: &mut State) {
        let transaction = history.undo().cloned();
        apply(state, transaction.as_slice());
    }

    fn redo(history: &mut History, state: &mut State) {
        let transaction = history.redo().cloned();
        apply(state, transaction.as_slice());
    }

    /// Deletes the char at `pos` as a revision of its own.
    fn delete_char(history: &mut History, state: &mut State, pos: usize) {
        let transaction = Transaction::change(&state.doc, [(pos, pos + 1, None)].into_iter());
        history.commit_revision(&transaction, state);
        transaction.apply(&mut state.doc);
    }

    /// The example of Vim's `:help undo-branches`, then `u` and `CTRL-R` from the branch it
    /// ends in.
    #[test]
    fn redo_follows_the_branch_visited_last() {
        let mut history = History::default();
        let mut state = State {
            doc: Rope::from("one two three"),
            selection: Selection::point(0),
        };
        for _ in 0..3 {
            delete_char(&mut history, &mut state, 0);
        }
        assert_eq!(state.doc, " two three");
        for _ in 0..3 {
            undo(&mut history, &mut state);
        }
        assert_eq!(state.doc, "one two three");
        for _ in 0..3 {
            delete_char(&mut history, &mut state, 4);
        }
        assert_eq!(state.doc, "one  three");

        // `g-` three times goes back to the first branch.
        for expected in ["one o three", "one wo three", " two three"] {
            apply(&mut state, &history.earlier(UndoKind::Steps(1)));
            assert_eq!(state.doc, expected);
        }
        for _ in 0..3 {
            undo(&mut history, &mut state);
        }
        assert_eq!(state.doc, "one two three");
        // Redo takes the branch visited last, not the newest one.
        redo(&mut history, &mut state);
        assert_eq!(state.doc, "ne two three");

        // Jumping into the newest branch makes redo follow it again.
        apply(&mut state, &history.later(UndoKind::Steps(5)));
        assert_eq!(state.doc, "one  three");
        for _ in 0..3 {
            undo(&mut history, &mut state);
        }
        redo(&mut history, &mut state);
        assert_eq!(state.doc, "one wo three");
    }

    /// Undoing and redoing retrace their steps when a revision has several children.
    #[test]
    fn redo_comes_back_where_undo_left() {
        let mut history = History::default();
        let mut state = State {
            doc: Rope::from("abc"),
            selection: Selection::point(0),
        };
        delete_char(&mut history, &mut state, 0); // 1: "bc"
        undo(&mut history, &mut state);
        delete_char(&mut history, &mut state, 2); // 2: "ab"
        undo(&mut history, &mut state);
        redo(&mut history, &mut state);
        assert_eq!(state.doc, "ab");

        // To revision 1 by time, back to the root, and redo goes to revision 1 again.
        apply(&mut state, &history.earlier(UndoKind::Steps(1)));
        assert_eq!(state.doc, "bc");
        undo(&mut history, &mut state);
        assert_eq!(state.doc, "abc");
        redo(&mut history, &mut state);
        assert_eq!(state.doc, "bc");
    }

    #[test]
    fn test_earlier_later() {
        let mut history = History::default();
        let doc = Rope::from("a\n");
        let mut state = State {
            doc,
            selection: Selection::point(0),
        };

        fn undo(history: &mut History, state: &mut State) {
            if let Some(transaction) = history.undo() {
                transaction.apply(&mut state.doc);
            }
        }

        fn earlier(history: &mut History, state: &mut State, uk: UndoKind) {
            let txns = history.earlier(uk);
            for txn in txns {
                txn.apply(&mut state.doc);
            }
        }

        fn later(history: &mut History, state: &mut State, uk: UndoKind) {
            let txns = history.later(uk);
            for txn in txns {
                txn.apply(&mut state.doc);
            }
        }

        fn commit_change(
            history: &mut History,
            state: &mut State,
            change: crate::transaction::Change,
            instant: SystemTime,
        ) {
            let txn = Transaction::change(&state.doc, vec![change].into_iter());
            history.commit_revision_at_timestamp(&txn, state, instant);
            txn.apply(&mut state.doc);
        }

        let t0 = SystemTime::now();
        let t = |n| t0.checked_add(Duration::from_secs(n)).unwrap();

        commit_change(&mut history, &mut state, (1, 1, Some(" b".into())), t(0));
        assert_eq!("a b\n", state.doc);

        commit_change(&mut history, &mut state, (3, 3, Some(" c".into())), t(10));
        assert_eq!("a b c\n", state.doc);

        commit_change(&mut history, &mut state, (5, 5, Some(" d".into())), t(20));
        assert_eq!("a b c d\n", state.doc);

        undo(&mut history, &mut state);
        assert_eq!("a b c\n", state.doc);

        commit_change(&mut history, &mut state, (5, 5, Some(" e".into())), t(30));
        assert_eq!("a b c e\n", state.doc);

        undo(&mut history, &mut state);
        undo(&mut history, &mut state);
        assert_eq!("a b\n", state.doc);

        commit_change(&mut history, &mut state, (1, 3, None), t(40));
        assert_eq!("a\n", state.doc);

        commit_change(&mut history, &mut state, (1, 1, Some(" f".into())), t(50));
        assert_eq!("a f\n", state.doc);

        use UndoKind::*;

        earlier(&mut history, &mut state, Steps(3));
        assert_eq!("a b c d\n", state.doc);

        later(&mut history, &mut state, TimePeriod(Duration::new(20, 0)));
        assert_eq!("a\n", state.doc);

        earlier(&mut history, &mut state, TimePeriod(Duration::new(19, 0)));
        assert_eq!("a b c d\n", state.doc);

        earlier(
            &mut history,
            &mut state,
            TimePeriod(Duration::new(10000, 0)),
        );
        assert_eq!("a\n", state.doc);

        later(&mut history, &mut state, Steps(50));
        assert_eq!("a f\n", state.doc);

        earlier(&mut history, &mut state, Steps(4));
        assert_eq!("a b c\n", state.doc);

        later(&mut history, &mut state, TimePeriod(Duration::new(1, 0)));
        assert_eq!("a b c\n", state.doc);

        later(&mut history, &mut state, TimePeriod(Duration::new(5, 0)));
        assert_eq!("a b c d\n", state.doc);

        later(&mut history, &mut state, TimePeriod(Duration::new(6, 0)));
        assert_eq!("a b c e\n", state.doc);

        later(&mut history, &mut state, Steps(1));
        assert_eq!("a\n", state.doc);
    }

    /// `:earlier {N}f` and `:later {N}f`, across writes in two branches.
    #[test]
    fn earlier_and_later_go_by_file_writes() {
        let mut history = History::default();
        let mut state = State {
            doc: Rope::from("abcdef"),
            selection: Selection::point(0),
        };
        let t0 = SystemTime::now();
        let t = |n| t0 + Duration::from_secs(n);
        let delete = |history: &mut History, state: &mut State, at: u64| {
            let transaction = Transaction::change(&state.doc, [(0, 1, None)].into_iter());
            history.commit_revision_at_timestamp(&transaction, state, t(at));
            transaction.apply(&mut state.doc);
        };
        let jump = |history: &mut History, state: &mut State, earlier: bool, writes: usize| {
            let kind = UndoKind::FileWrites(writes);
            let transactions = if earlier {
                history.earlier(kind)
            } else {
                history.later(kind)
            };
            apply(state, &transactions);
            state.doc.to_string()
        };

        delete(&mut history, &mut state, 1); // 1 "bcdef"
        history.record_save(1);
        delete(&mut history, &mut state, 3); // 2 "cdef"
        delete(&mut history, &mut state, 4); // 3 "def"
        history.record_save(3);
        history.record_save(3); // the same write
        undo(&mut history, &mut state);
        undo(&mut history, &mut state);
        delete(&mut history, &mut state, 7); // 4 "cdef" from 1
        history.record_save(4);
        delete(&mut history, &mut state, 9); // 5 "def" from 4, unsaved
        assert_eq!(history.saves().len(), 3);

        // Changes since the last write: one write back is that write.
        assert_eq!(jump(&mut history, &mut state, true, 1), "cdef");
        assert_eq!(history.current_revision(), 4);
        assert_eq!(jump(&mut history, &mut state, true, 1), "def");
        assert_eq!(history.current_revision(), 3);
        assert_eq!(jump(&mut history, &mut state, true, 1), "bcdef");
        // Before the first write is the root.
        assert_eq!(jump(&mut history, &mut state, true, 1), "abcdef");
        assert_eq!(jump(&mut history, &mut state, false, 2), "def");
        assert_eq!(history.current_revision(), 3);
        // After the last write is the newest revision.
        assert_eq!(jump(&mut history, &mut state, false, 2), "def");
        assert_eq!(history.current_revision(), 5);
        assert_eq!(jump(&mut history, &mut state, true, 3), "bcdef");

        // A revision is after the writes of its ancestors, not of another branch.
        apply(&mut state, &history.jump_to(2));
        assert_eq!(state.doc, "cdef");
        assert_eq!(jump(&mut history, &mut state, true, 1), "bcdef");
    }

    #[test]
    fn test_parse_undo_kind() {
        use UndoKind::*;

        // Default is one step.
        assert_eq!("".parse(), Ok(Steps(1)));

        // A number with `f` counts file writes.
        assert_eq!("2f".parse(), Ok(FileWrites(2)));
        assert_eq!(" 1 f".parse(), Ok(FileWrites(1)));

        // An integer means the number of steps.
        assert_eq!("1".parse(), Ok(Steps(1)));
        assert_eq!("  16 ".parse(), Ok(Steps(16)));

        // Duration has a strict format.
        let validation_err = Err("duration should be composed \
         of positive integers followed by time units"
            .to_string());
        assert_eq!("  16 33".parse::<UndoKind>(), validation_err);
        assert_eq!("  seconds 22  ".parse::<UndoKind>(), validation_err);
        assert_eq!("  -4 m".parse::<UndoKind>(), validation_err);
        assert_eq!("5s 3".parse::<UndoKind>(), validation_err);

        // Units are u64.
        assert_eq!(
            "18446744073709551616minutes".parse::<UndoKind>(),
            Err("integer too large: 18446744073709551616".to_string())
        );

        // Units are validated.
        assert_eq!(
            "1 millennium".parse::<UndoKind>(),
            Err("incorrect time unit: millennium".to_string())
        );

        // Units can't be specified twice.
        assert_eq!(
            "2 seconds 6s".parse::<UndoKind>(),
            Err("seconds specified more than once".to_string())
        );

        // Various formats are correctly handled.
        assert_eq!(
            "4s".parse::<UndoKind>(),
            Ok(TimePeriod(Duration::from_secs(4)))
        );
        assert_eq!(
            "2m".parse::<UndoKind>(),
            Ok(TimePeriod(Duration::from_secs(120)))
        );
        assert_eq!(
            "5h".parse::<UndoKind>(),
            Ok(TimePeriod(Duration::from_secs(5 * 60 * 60)))
        );
        assert_eq!(
            "3d".parse::<UndoKind>(),
            Ok(TimePeriod(Duration::from_secs(3 * 24 * 60 * 60)))
        );
        assert_eq!(
            "1m30s".parse::<UndoKind>(),
            Ok(TimePeriod(Duration::from_secs(90)))
        );
        assert_eq!(
            "1m 20 seconds".parse::<UndoKind>(),
            Ok(TimePeriod(Duration::from_secs(80)))
        );
        assert_eq!(
            "  2 minute 1day".parse::<UndoKind>(),
            Ok(TimePeriod(Duration::from_secs(24 * 60 * 60 + 2 * 60)))
        );
        assert_eq!(
            "3 d 2hour 5 minutes 30sec".parse::<UndoKind>(),
            Ok(TimePeriod(Duration::from_secs(
                3 * 24 * 60 * 60 + 2 * 60 * 60 + 5 * 60 + 30
            )))
        );

        // Sum overflow is handled.
        assert_eq!(
            "18446744073709551615minutes".parse::<UndoKind>(),
            Err("duration too large".to_string())
        );
        assert_eq!(
            "1 minute 18446744073709551615 seconds".parse::<UndoKind>(),
            Err("duration too large".to_string())
        );
    }
}
