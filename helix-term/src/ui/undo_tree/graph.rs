//! The undo history as a graph, the newest revision on top. Each revision is a row with a node in
//! a lane, and the lanes of revisions sharing a parent join above it: `├─┘`.

/// The glyph of a node in a graph; drawing it can choose another.
pub const NODE: char = '○';

/// A row of the graph.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Row {
    /// The lanes, two columns each: a glyph and what joins it to the next lane.
    pub graph: String,
    /// The revision of a row with a node, and its lane; none on a row joining lanes.
    pub node: Option<(usize, usize)>,
}

/// The graph of a history.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Graph {
    pub rows: Vec<Row>,
    /// The columns of the widest row's graph.
    pub width: usize,
    /// The row of each revision.
    row_of: Vec<usize>,
}

impl Graph {
    /// The graph of a history whose revisions have the parents `parents`; the root's is itself.
    pub fn new(parents: &[usize]) -> Self {
        let mut graph = Self {
            rows: Vec::with_capacity(parents.len() + parents.len() / 4),
            width: 0,
            row_of: vec![0; parents.len()],
        };
        // The revision each lane goes down to.
        let mut lanes: Vec<Option<usize>> = Vec::new();
        for revision in (0..parents.len()).rev() {
            let mut waiting = (0..lanes.len()).filter(|&lane| lanes[lane] == Some(revision));
            let lane = match waiting.next() {
                Some(lane) => {
                    let closing: Vec<_> = waiting.collect();
                    if !closing.is_empty() {
                        graph.push(joining(&lanes, lane, &closing), None);
                        for &closed in &closing {
                            lanes[closed] = None;
                        }
                    }
                    lane
                }
                None => lanes.iter().position(Option::is_none).unwrap_or_else(|| {
                    lanes.push(None);
                    lanes.len() - 1
                }),
            };
            let row = lanes
                .iter()
                .enumerate()
                .map(|(index, target)| {
                    if index == lane {
                        NODE
                    } else if target.is_some() {
                        '│'
                    } else {
                        ' '
                    }
                })
                .flat_map(|glyph| [glyph, ' '])
                .collect();
            graph.row_of[revision] = graph.rows.len();
            graph.push(row, Some((revision, lane)));
            lanes[lane] = (revision != 0).then(|| parents[revision]);
            while lanes.last() == Some(&None) {
                lanes.pop();
            }
        }
        graph
    }

    fn push(&mut self, graph: String, node: Option<(usize, usize)>) {
        let graph = graph.trim_end().to_owned();
        self.width = self.width.max(graph.chars().count());
        self.rows.push(Row { graph, node });
    }

    /// The row of `revision`.
    pub fn row_of(&self, revision: usize) -> usize {
        self.row_of[revision]
    }
}

/// The row where the lanes `closing` join `lane` above the node in it.
fn joining(lanes: &[Option<usize>], lane: usize, closing: &[usize]) -> String {
    let last = *closing.last().expect("lanes close");
    let mut row = String::new();
    for (index, target) in lanes.iter().enumerate() {
        let (glyph, join) = if index < lane || index > last {
            (if target.is_some() { '│' } else { ' ' }, ' ')
        } else if index == lane {
            ('├', '─')
        } else if index == last {
            ('┘', ' ')
        } else if closing.contains(&index) {
            ('┴', '─')
        } else if target.is_some() {
            ('┼', '─')
        } else {
            ('─', '─')
        };
        row.push(glyph);
        row.push(join);
    }
    row
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The rows of the graph of `parents`, each with its revision after the graph.
    fn drawn(parents: &[usize]) -> Vec<String> {
        let graph = Graph::new(parents);
        graph
            .rows
            .iter()
            .map(|row| {
                let revision = row
                    .node
                    .map_or(String::new(), |(revision, _)| revision.to_string());
                let pad = " ".repeat(graph.width - row.graph.chars().count());
                let lanes = row.graph.replace(NODE, "o");
                format!("{lanes}{pad} {revision}").trim_end().to_owned()
            })
            .collect()
    }

    #[test]
    fn branches_join_above_their_parent() {
        // 0 ─ 1 ┬ 2 ┬ 3
        //       │   └ 4
        //       └ 5 ─ 6
        assert_eq!(
            drawn(&[0, 0, 1, 2, 2, 1, 5]),
            [
                "o     6",
                "o     5",
                "│ o   4",
                "│ │ o 3",
                "│ ├─┘",
                "│ o   2",
                "├─┘",
                "o     1",
                "o     0",
            ]
        );
    }

    #[test]
    fn a_history_without_branches_is_a_line() {
        assert_eq!(drawn(&[0, 0, 1]), ["o 2", "o 1", "o 0"]);
        assert_eq!(drawn(&[0]), ["o 0"]);
    }

    #[test]
    fn lanes_close_together_and_cross_others() {
        // 0 ┬ 1 ┬ 2
        //   │   └ 4
        //   ├ 3
        //   └ 5
        assert_eq!(
            drawn(&[0, 0, 1, 0, 1, 0]),
            [
                "o       5",
                "│ o     4",
                "│ │ o   3",
                "│ │ │ o 2",
                "│ ├─┼─┘",
                "│ o │   1",
                "├─┴─┘",
                "o       0",
            ]
        );
    }

    #[test]
    fn rows_of_revisions_are_found() {
        let graph = Graph::new(&[0, 0, 1, 2, 2, 1, 5]);
        assert_eq!(graph.width, 5);
        assert_eq!(graph.row_of(6), 0);
        assert_eq!(graph.row_of(2), 5);
        assert_eq!(graph.rows[5].node, Some((2, 1)));
        assert_eq!(graph.row_of(0), 8);
    }
}
