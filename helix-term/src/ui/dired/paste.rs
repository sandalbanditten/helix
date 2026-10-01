//! Lines pasted into a dired buffer, which copy the entries whose lines they were.
//!
//! A pasted line is found from the edits themselves: an insertion of whole lines, replacing
//! nothing, that read like a line some open dired buffer lists, tree guides and column padding
//! aside. Edited afterwards, it is still the same line, so a copy can be renamed or given another
//! mode before it is written.

use std::{borrow::Cow, cell::OnceCell, collections::HashMap, ops::Range, path::PathBuf};

use helix_core::{Assoc, Operation, Rope, Transaction};
use helix_view::dired::{Columns, Entry, Listing, Source};

use super::format;

/// An entry a pasted line was yanked from.
#[derive(Debug, Clone)]
pub struct Origin {
    /// Where the entry is.
    pub path: PathBuf,
    pub entry: Entry,
    /// The line it is listed as, without the line break.
    pub line: String,
    pub columns: Columns,
    /// Whether the line was listed with tree guides.
    pub tree: bool,
    /// The number of the register write that yanked it, if any did.
    pub yank: Option<u64>,
}

/// The entries open dired buffers list, looked up by their lines. The lines are only gathered
/// when a line is pasted, and only without guides and padding when one is not found as it is.
pub struct Origins<'a> {
    listings: Vec<&'a Listing>,
    /// The columns of the listings, each once.
    columns: Vec<Columns>,
    /// `(listing, entry)` by the text of the line.
    exact: OnceCell<HashMap<String, Vec<(usize, usize)>>>,
    /// `(listing, entry)` by the text of the line without guides and padding.
    alike: OnceCell<HashMap<String, Vec<(usize, usize)>>>,
}

impl<'a> Origins<'a> {
    pub fn new(listings: impl Iterator<Item = &'a Listing>) -> Self {
        let listings: Vec<_> = listings.collect();
        let mut columns = Vec::new();
        for listing in &listings {
            if !columns.contains(&listing.columns) {
                columns.push(listing.columns);
            }
        }
        Self {
            listings,
            columns,
            exact: OnceCell::new(),
            alike: OnceCell::new(),
        }
    }

    /// The listed lines by `key`, which gives up on a line with `None`.
    fn index(
        &self,
        key: impl Fn(&str, Columns) -> Option<String>,
    ) -> HashMap<String, Vec<(usize, usize)>> {
        let mut index: HashMap<String, Vec<(usize, usize)>> = HashMap::new();
        for (l, listing) in self.listings.iter().enumerate() {
            for i in 0..listing.entries.len() {
                let line = listed_line(listing, i);
                if let Some(key) = key(&line, listing.columns) {
                    index.entry(key).or_default().push((l, i));
                }
            }
        }
        index
    }

    /// The entries `line` could have been yanked from: those listed exactly so, else those
    /// whose lines only differ in guides or padding.
    fn of(&self, line: &str) -> Vec<Origin> {
        let exact = self
            .exact
            .get_or_init(|| self.index(|line, _| Some(line.to_owned())));
        let mut like: Vec<(usize, usize)> = exact.get(line).cloned().unwrap_or_default();
        if like.is_empty() {
            let alike = self.alike.get_or_init(|| self.index(key));
            for &columns in &self.columns {
                let found = key(line, columns).and_then(|key| alike.get(&key));
                like.extend(
                    found
                        .into_iter()
                        .flatten()
                        .filter(|(l, _)| self.listings[*l].columns == columns),
                );
            }
        }
        let mut origins: Vec<Origin> = like
            .into_iter()
            .map(|(l, i)| {
                let listing = self.listings[l];
                let entry = &listing.entries[i];
                Origin {
                    path: listing.source.root().join(&entry.path),
                    entry: entry.clone(),
                    line: listed_line(listing, i).into_owned(),
                    columns: listing.columns,
                    tree: matches!(listing.source, Source::Tree { .. }),
                    yank: listing
                        .yanked
                        .as_ref()
                        .filter(|yanked| yanked.paths.contains(&entry.path))
                        .map(|yanked| yanked.write),
                }
            })
            .collect();
        // The same entry in another listing is no other entry, and the listings come first that
        // came first, as the open ones do before those of closed buffers.
        origins.sort_by(|a, b| a.path.cmp(&b.path));
        origins.dedup_by(|a, b| a.path == b.path);
        origins
    }
}

/// Line `index` of `listing`, without the line break.
pub fn listed_line(listing: &Listing, index: usize) -> Cow<'_, str> {
    let line: Cow<str> = listing.text.line(index).into();
    match line {
        Cow::Borrowed(line) => Cow::Borrowed(line.trim_end_matches(['\n', '\r'])),
        Cow::Owned(line) => Cow::Owned(line.trim_end_matches(['\n', '\r']).to_owned()),
    }
}

/// `line` of a listing with `columns` without its tree guides, its fields one space apart.
fn key(line: &str, columns: Columns) -> Option<String> {
    let parsed = format::parse(line, columns, true).ok()?;
    let fields = line[..parsed.guides.start]
        .split_whitespace()
        .chain(line[parsed.guides.end..].split_whitespace());
    Some(fields.collect::<Vec<_>>().join(" "))
}

/// A pasted line: where it is now and the entries it could have been yanked from, more than one
/// only when their lines read alike.
#[derive(Debug)]
pub struct Paste {
    pub line: usize,
    pub origins: Vec<Origin>,
}

/// The lines of the text that `transactions` made of `listed` which were pasted: inserted whole
/// as a line of `origins`. In order.
pub fn pasted(listed: &Rope, transactions: &[Transaction], origins: &Origins) -> Vec<Paste> {
    let inserts_lines = transactions.iter().any(|transaction| {
        let operations = transaction.changes().changes();
        (operations.iter())
            .any(|operation| matches!(operation, Operation::Insert(text) if text.contains('\n')))
    });
    if !inserts_lines {
        return Vec::new();
    }
    let mut text = listed.clone();
    // The chars of each pasted line, as of the text so far.
    let mut found: Vec<(Range<usize>, Vec<Origin>)> = Vec::new();
    for transaction in transactions {
        let changes = transaction.changes();
        if !found.is_empty() {
            // Each line's start and end, in order as `update_positions` is fastest that way.
            let mut ends: Vec<(usize, Assoc, usize, bool)> = found
                .iter()
                .enumerate()
                .flat_map(|(i, (range, _))| {
                    [
                        (range.start, Assoc::After, i, false),
                        (range.end, Assoc::Before, i, true),
                    ]
                })
                .collect();
            ends.sort_by_key(|(pos, ..)| *pos);
            let mut mapped: Vec<usize> = ends.iter().map(|(pos, ..)| *pos).collect();
            changes.update_positions(
                mapped
                    .iter_mut()
                    .zip(&ends)
                    .map(|(pos, (_, assoc, ..))| (pos, *assoc)),
            );
            for (pos, &(_, _, i, end)) in mapped.into_iter().zip(&ends) {
                if end {
                    found[i].0.end = pos;
                } else {
                    found[i].0.start = pos;
                }
            }
            // A line deleted again was no paste after all.
            found.retain(|(range, _)| range.start < range.end);
        }
        transaction.apply(&mut text);

        let mut new_pos = 0;
        let operations = changes.changes();
        for (i, operation) in operations.iter().enumerate() {
            // Text put in place of other text, as replacing a selection or piping it through a
            // command does, edits the lines it replaced.
            let replaces = |i: Option<usize>| {
                i.and_then(|i| operations.get(i))
                    .is_some_and(|operation| matches!(operation, Operation::Delete(_)))
            };
            match operation {
                Operation::Retain(len) => new_pos += len,
                Operation::Delete(_) => {}
                Operation::Insert(inserted)
                    if replaces(i.checked_sub(1)) || replaces(Some(i + 1)) =>
                {
                    new_pos += inserted.chars().count();
                }
                Operation::Insert(inserted) => {
                    let mut start = new_pos;
                    let mut line_start = start == 0 || text.char(start - 1) == '\n';
                    for piece in inserted.split_inclusive('\n') {
                        let len = piece.chars().count();
                        if let Some(line) = piece.strip_suffix('\n').filter(|_| line_start) {
                            let line = line.strip_suffix('\r').unwrap_or(line);
                            let like = origins.of(line);
                            if !like.is_empty() {
                                found.push((start..start + line.chars().count(), like));
                            }
                        }
                        line_start = piece.ends_with('\n');
                        start += len;
                    }
                    new_pos += inserted.chars().count();
                }
            }
        }
    }

    let mut pasted: Vec<Paste> = found
        .into_iter()
        .map(|(range, origins)| Paste {
            line: text.char_to_line(range.start),
            origins,
        })
        .collect();
    pasted.sort_by_key(|paste| paste.line);
    pasted.dedup_by_key(|paste| paste.line);
    pasted
}

#[cfg(test)]
mod tests {
    use std::time::SystemTime;

    use helix_view::dired::{Kind, Size};

    use super::*;

    const FLAT: Columns = Columns {
        unix: false,
        git: false,
        icons: false,
    };

    /// A listing of `/dir` with lines `(size, guides, name)`, a tree one with `tree`.
    fn listing(lines: &[(&str, &str, &str)], tree: bool) -> Listing {
        let entry = |guides: &str, name: &str| Entry {
            path: name.into(),
            kind: Kind::File,
            mode: 0o644,
            uid: 0,
            gid: 0,
            user: "root".into(),
            group: "root".into(),
            size: Size::Bytes(0),
            modified: SystemTime::UNIX_EPOCH,
            id: (0, 0),
            link: None,
            git: None,
            guides: guides.into(),
            icon: None,
        };
        let text: String = lines
            .iter()
            .map(|(size, guides, name)| format!("{size} 1 Jan  1970 {guides}{name}\n"))
            .collect();
        Listing {
            source: if tree {
                Source::Tree {
                    root: "/dir".into(),
                    expanded: Default::default(),
                }
            } else {
                Source::Directory("/dir".into())
            },
            columns: FLAT,
            repo: None,
            entries: lines
                .iter()
                .map(|(_, guides, name)| entry(guides, name))
                .collect(),
            text: Rope::from(text),
            yanked: None,
        }
    }

    /// The pasted lines after `edits` (`(from, to, text)`, one transaction each) on the first
    /// of `listings`, with the paths they could be of.
    fn pasted_after(
        listings: &[&Listing],
        edits: &[(usize, usize, &str)],
    ) -> Vec<(usize, Vec<PathBuf>)> {
        let origins = Origins::new(listings.iter().copied());
        let mut text = listings[0].text.clone();
        let transactions: Vec<_> = edits
            .iter()
            .map(|&(from, to, inserted)| {
                let transaction =
                    Transaction::change(&text, [(from, to, Some(inserted.into()))].into_iter());
                transaction.apply(&mut text);
                transaction
            })
            .collect();
        pasted(&listings[0].text, &transactions, &origins)
            .into_iter()
            .map(|paste| {
                let paths = paste.origins.iter().map(|origin| origin.path.clone());
                (paste.line, paths.collect())
            })
            .collect()
    }

    #[test]
    fn pasted_lines_are_found_and_followed() {
        let listing = listing(&[("1", "", "a"), ("2", "", "b")], false);
        let a = "1 1 Jan  1970 a\n";
        // `yp` on `a`, then the copy renamed.
        let pasted = pasted_after(&[&listing], &[(16, 16, a), (30, 31, "c")]);
        assert_eq!(pasted, [(1, vec![PathBuf::from("/dir/a")])]);
        // Pasted at the end, then another line inserted before it.
        let pasted = pasted_after(&[&listing], &[(32, 32, a), (0, 0, "new\n")]);
        assert_eq!(pasted, [(3, vec![PathBuf::from("/dir/a")])]);
    }

    #[test]
    fn typed_or_undone_lines_are_not_pasted() {
        let listing = listing(&[("1", "", "a"), ("2", "", "b")], false);
        // Typed in pieces.
        let edits = [(16, 16, "1 1 Jan"), (23, 23, "  1970 a\n")];
        assert_eq!(pasted_after(&[&listing], &edits), []);
        // Not at the start of a line.
        assert_eq!(
            pasted_after(&[&listing], &[(17, 17, "1 1 Jan  1970 a\n")]),
            []
        );
        // Pasted, then deleted again.
        let edits = [(16, 16, "1 1 Jan  1970 a\n"), (16, 32, "")];
        assert_eq!(pasted_after(&[&listing], &edits), []);
        // In place of other text, as `R` or `%|sort` do.
        assert_eq!(
            pasted_after(&[&listing], &[(0, 16, "1 1 Jan  1970 a\n")]),
            []
        );
        let sorted = "2 1 Jan  1970 b\n1 1 Jan  1970 a\n";
        assert_eq!(pasted_after(&[&listing], &[(0, 32, sorted)]), []);
        // Several lines at once.
        let pasted = pasted_after(&[&listing], &[(32, 32, &listing.text.to_string())]);
        assert_eq!(pasted.len(), 2);
    }

    #[test]
    fn guides_and_padding_do_not_matter() {
        let tree = listing(&[("1", "├── ", "a"), ("1", "└── ", "b")], true);
        // Yanked with other guides and padding, pasted from a tree into a flat listing.
        let flat = listing(&[("2", "", "c")], false);
        let edits = [(0, 0, " 1  1 Jan  1970 │   └──   a\n")];
        assert_eq!(
            pasted_after(&[&flat, &tree], &edits),
            [(0, vec![PathBuf::from("/dir/a")])]
        );
    }

    #[test]
    fn lines_reading_alike_could_be_either_entry() {
        // `a` in two directories, listed alike.
        let mut tree = listing(&[("1", "│   └── ", "a"), ("1", "│   └── ", "a")], true);
        tree.entries[0].path = "x/a".into();
        tree.entries[1].path = "y/a".into();
        let pasted = pasted_after(&[&tree], &[(0, 0, "1 1 Jan  1970 │   └── a\n")]);
        let both = vec![PathBuf::from("/dir/x/a"), PathBuf::from("/dir/y/a")];
        assert_eq!(pasted, [(0, both)]);
        // The line as listed tells them apart when only the guides differ.
        tree.text = Rope::from("1 1 Jan  1970 │   └── a\n1 1 Jan  1970     └── a\n");
        let pasted = pasted_after(&[&tree], &[(0, 0, "1 1 Jan  1970     └── a\n")]);
        assert_eq!(pasted, [(0, vec![PathBuf::from("/dir/y/a")])]);
    }
}
