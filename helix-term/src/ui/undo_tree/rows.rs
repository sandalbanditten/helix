//! What the undo tree shows of a history: its graph and what each revision changed, made anew
//! when the history changes.

use std::time::{Duration, SystemTime, UNIX_EPOCH};

use helix_core::{history::History, Operation, Transaction};
use helix_view::DocumentId;

use super::graph::Graph;

/// The most characters of a change shown.
const SNIPPET_LEN: usize = 80;

/// What a revision changed, in short.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Snippet {
    /// The root, before any change.
    Original,
    /// The first line of text inserted.
    Inserted(String),
    /// The first line of text deleted, by a revision that inserted none.
    Deleted(String),
}

impl Snippet {
    fn of(history: &History, revision: usize) -> Self {
        if revision == 0 {
            return Self::Original;
        }
        let (transaction, inversion) = history.changes(revision);
        if let Some(line) = first_line(transaction) {
            Self::Inserted(line)
        } else if let Some(line) = first_line(inversion) {
            Self::Deleted(line)
        } else {
            Self::Inserted(String::new())
        }
    }

    /// The text shown.
    pub fn text(&self) -> std::borrow::Cow<'_, str> {
        match self {
            Self::Original => "original".into(),
            Self::Inserted(line) => line.into(),
            Self::Deleted(line) => format!("-{line}").into(),
        }
    }
}

/// The first line inserted by `transaction` with more than whitespace, trimmed.
fn first_line(transaction: &Transaction) -> Option<String> {
    transaction
        .changes()
        .changes()
        .iter()
        .filter_map(|operation| match operation {
            Operation::Insert(text) => Some(text),
            _ => None,
        })
        .flat_map(|text| text.lines())
        .map(str::trim)
        .find(|line| !line.is_empty())
        .map(|line| line.chars().take(SNIPPET_LEN).collect())
}

/// The text `revision` inserted and deleted, for searching.
pub fn changed_text(history: &History, revision: usize) -> String {
    let (transaction, inversion) = history.changes(revision);
    [transaction, inversion]
        .into_iter()
        .flat_map(|transaction| transaction.changes().changes())
        .filter_map(|operation| match operation {
            Operation::Insert(text) => Some(text.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// The rows of the undo tree for the history of a document.
pub struct Rows {
    pub doc: DocumentId,
    pub graph: Graph,
    pub snippets: Vec<Snippet>,
    pub parents: Vec<usize>,
    /// The children of each revision, oldest first.
    pub children: Vec<Vec<usize>>,
    pub timestamps: Vec<SystemTime>,
    /// The revisions written, in the order of the writes.
    pub saves: Vec<usize>,
}

impl Rows {
    pub fn new(doc: DocumentId, history: &History) -> Self {
        let len = history.len();
        let parents: Vec<_> = (0..len).map(|revision| history.parent(revision)).collect();
        let mut children = vec![Vec::new(); len];
        for (revision, &parent) in parents.iter().enumerate().skip(1) {
            children[parent].push(revision);
        }
        Self {
            doc,
            graph: Graph::new(&parents),
            snippets: (0..len)
                .map(|revision| Snippet::of(history, revision))
                .collect(),
            parents,
            children,
            timestamps: (0..len)
                .map(|revision| history.timestamp(revision))
                .collect(),
            saves: history.saves().to_vec(),
        }
    }

    /// Whether these are the rows of `history`, the history of `doc`.
    pub fn are_of(&self, doc: DocumentId, history: &History) -> bool {
        let len = history.len();
        self.doc == doc
            && self.parents.len() == len
            && self.timestamps[len - 1] == history.timestamp(len - 1)
            && self.saves == history.saves()
    }

    pub fn len(&self) -> usize {
        self.graph.rows.len()
    }

    /// The revision of row `row`, if it has one.
    pub fn revision(&self, row: usize) -> Option<usize> {
        self.graph.rows.get(row)?.node.map(|(revision, _)| revision)
    }

    /// Whether `revision` was written, and was the last revision written.
    pub fn written(&self, revision: usize) -> (bool, bool) {
        (
            self.saves.contains(&revision),
            self.saves.last() == Some(&revision),
        )
    }
}

/// How long ago `time` was, in short: `now`, `40s`, `3m`, `2h`, `5d`, then the date as `09-30`, or
/// the year for an earlier year. Dates are UTC.
pub fn age(time: SystemTime, now: SystemTime) -> String {
    let seconds = now.duration_since(time).unwrap_or_default().as_secs();
    const MINUTE: u64 = 60;
    const HOUR: u64 = 60 * MINUTE;
    const DAY: u64 = 24 * HOUR;
    match seconds {
        0..5 => "now".to_owned(),
        5..MINUTE => format!("{seconds}s"),
        MINUTE..HOUR => format!("{}m", seconds / MINUTE),
        HOUR..DAY => format!("{}h", seconds / HOUR),
        _ if seconds < 7 * DAY => format!("{}d", seconds / DAY),
        _ => {
            let (year, month, day) = date(time);
            if year == date(now).0 {
                format!("{month:02}-{day:02}")
            } else {
                year.to_string()
            }
        }
    }
}

/// The UTC date of `time`: year, month and day.
fn date(time: SystemTime) -> (i64, u32, u32) {
    let days = time
        .duration_since(UNIX_EPOCH)
        .unwrap_or(Duration::ZERO)
        .as_secs()
        / (24 * 60 * 60);
    civil_from_days(days as i64)
}

/// The proleptic Gregorian date of the day `days` after 1970-01-01, after Howard Hinnant's
/// `civil_from_days`.
fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let day_of_era = z.rem_euclid(146_097);
    let year_of_era =
        (day_of_era - day_of_era / 1460 + day_of_era / 36524 - day_of_era / 146_096) / 365;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let month_index = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * month_index + 2) / 5 + 1;
    let month = if month_index < 10 {
        month_index + 3
    } else {
        month_index - 9
    };
    let year = year_of_era + era * 400 + i64::from(month <= 2);
    (year, month as u32, day as u32)
}

#[cfg(test)]
mod tests {
    use helix_core::{history::State, Rope, Selection};

    use super::*;

    #[test]
    fn ages_are_short() {
        let now = UNIX_EPOCH + Duration::from_secs(1_790_000_000); // 2026-09-21
        let ago = |seconds| age(now - Duration::from_secs(seconds), now);
        assert_eq!(ago(2), "now");
        assert_eq!(ago(40), "40s");
        assert_eq!(ago(3 * 60 + 5), "3m");
        assert_eq!(ago(2 * 3600), "2h");
        assert_eq!(ago(5 * 86400), "5d");
        assert_eq!(ago(20 * 86400), "09-01");
        assert_eq!(ago(400 * 86400), "2025");
        // A clock gone back counts as now.
        assert_eq!(age(now + Duration::from_secs(60), now), "now");
    }

    #[test]
    fn dates_are_civil() {
        assert_eq!(civil_from_days(0), (1970, 1, 1));
        assert_eq!(civil_from_days(11_016), (2000, 2, 29));
        assert_eq!(civil_from_days(20_729), (2026, 10, 3));
    }

    #[test]
    fn snippets_show_what_changed() {
        let mut history = History::default();
        let mut state = State {
            doc: Rope::from("fn main() {}\n"),
            selection: Selection::point(0),
        };
        let mut change = |history: &mut History, change| {
            let transaction = Transaction::change(&state.doc, [change].into_iter());
            history.commit_revision(&transaction, &state);
            transaction.apply(&mut state.doc);
        };
        change(&mut history, (0, 0, Some("\n  // todo\nmore\n".into())));
        change(&mut history, (0, 11, None));
        change(&mut history, (0, 0, Some(" ".into())));
        let snippets: Vec<_> = (0..4)
            .map(|revision| Snippet::of(&history, revision).text().into_owned())
            .collect();
        assert_eq!(snippets, ["original", "// todo", "-// todo", ""]);
        assert_eq!(changed_text(&history, 2), "\n  // todo\n");
    }
}
