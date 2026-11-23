//! A graph of documents that a language server shows (`helix/showGraph`), like notes and the
//! links between them: the documents one or two links from one, drawn in braille on rings around
//! it, or as a mind map of lines.

use std::collections::{HashMap, VecDeque};
use std::path::Path;
use std::sync::Arc;

use helix_core::{
    unicode::width::{UnicodeWidthChar, UnicodeWidthStr},
    Position,
};
use helix_lsp::{lsp, LanguageServerId, ShowGraphParams};
use helix_view::{
    editor::{Action as Open, Config, GraphLabels, GraphView},
    graphics::{Color, Modifier, Rect, Style},
    info::Info,
    input::KeyEvent,
    Theme,
};
use tui::{
    buffer::Buffer as Surface,
    symbols::braille,
    widgets::{Block, Widget as _},
};

use crate::{
    compositor::{Component, Context, Event, EventResult},
    ctrl, key,
    ui::{
        editor::canonicalize_key,
        file_tree::{
            ls_colors::{EntryType, LsColors},
            Palette,
        },
        lsp::hover::Hover,
        panel_keys::{self, bind, Bindings, Lookup},
        Popup,
    },
};

/// The rows per link length that fitting a graph zooms in to at most, and how many more each
/// node allows, so that small graphs stay small.
const FIT_SCALE: (f32, f32) = (3.0, 0.5);
const MIN_SCALE: f32 = 0.2;
const MAX_SCALE: f32 = 40.0;
const ZOOM_STEP: f32 = 1.5;
/// How much wider or narrower than its layout a graph may be drawn to fill the popup.
const STRETCH: (f32, f32) = (0.25, 2.0);
/// The widest a name is drawn; the status line shows the whole name of the one under the cursor.
const NAME_WIDTH: usize = 32;

pub struct Graph {
    params: ShowGraphParams,
    /// The nodes each node links with, either way.
    links: Vec<Vec<usize>>,
    /// The language server, which runs the nodes' commands.
    server: LanguageServerId,
    labels: GraphLabels,
    view: GraphView,
    /// The `LS_COLORS` rules the file tree colors names by.
    colors: Option<Arc<LsColors>>,
    centre: usize,
    /// How many links from the centre nodes are shown.
    depth: usize,
    /// The shown nodes, the centre first, and their positions in link lengths.
    shown: Vec<(usize, (f32, f32))>,
    /// The links between shown nodes, as indices into `shown`.
    edges: Vec<(usize, usize)>,
    /// How many links each shown node is from the centre, and the node one link nearer that it
    /// hangs off, if any.
    hops: Vec<usize>,
    parents: Vec<Option<usize>>,
    /// The lines of the mind map, to each node from the one it hangs off.
    routes: Vec<(usize, Vec<(i32, i32)>)>,
    /// Columns and rows per link length.
    scale: (f32, f32),
    /// Where the shown nodes are drawn.
    placed: Vec<Place>,
    /// The cursor's cell and the area's top left cell on the graph's canvas.
    cursor: (i32, i32),
    scroll: (i32, i32),
    /// The screen, whether the popup has a border, and the area inside it that the graph was
    /// last drawn in.
    screen: Rect,
    bordered: bool,
    inner: Rect,
    /// Whether to fit the graph into its area when it's drawn next, and to scroll the cursor to
    /// the middle of the area then.
    fit: bool,
    recentre: bool,
    pending: Vec<KeyEvent>,
    info: Option<Info>,
}

/// Where a shown node is drawn: its cell, and whether its name is on its left.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Place {
    x: i32,
    y: i32,
    left: bool,
}

impl Place {
    /// The columns the node, a space and a name `width` wide take.
    fn span(self, width: i32) -> (i32, i32) {
        if self.left {
            (self.x - 1 - width, self.x + 1)
        } else {
            (self.x, self.x + 2 + width)
        }
    }
}

impl Graph {
    pub const ID: &'static str = "graph";

    /// The graph of `params`, centred on its focus, which must be one of its nodes.
    pub fn new(params: ShowGraphParams, server: LanguageServerId, config: &Config) -> Self {
        let n = params.nodes.len();
        let mut links = vec![Vec::new(); n];
        for &(a, b) in &params.edges {
            if a != b && a < n && b < n {
                links[a].push(b);
                links[b].push(a);
            }
        }
        for links in &mut links {
            links.sort_unstable();
            links.dedup();
        }
        let mut graph = Self {
            centre: params.focus,
            params,
            links,
            server,
            labels: config.graph.labels,
            view: config.graph.view,
            colors: Palette::default().get(&config.file_tree.ls_colors),
            depth: 2,
            shown: Vec::new(),
            edges: Vec::new(),
            hops: Vec::new(),
            parents: Vec::new(),
            routes: Vec::new(),
            scale: (2.0 * FIT_SCALE.0, FIT_SCALE.0),
            placed: Vec::new(),
            cursor: (0, 0),
            scroll: (0, 0),
            screen: Rect::default(),
            bordered: true,
            inner: Rect::default(),
            fit: true,
            recentre: true,
            pending: Vec::new(),
            info: None,
        };
        graph.relayout();
        graph
    }

    /// Lays out the nodes up to `depth` links from the centre.
    fn relayout(&mut self) {
        let shown = neighbourhood(&self.links, self.centre, self.depth);
        let index: HashMap<usize, usize> = shown
            .iter()
            .enumerate()
            .map(|(i, &(n, _))| (n, i))
            .collect();
        self.edges = shown
            .iter()
            .enumerate()
            .flat_map(|(i, &(n, _))| {
                let index = &index;
                self.links[n]
                    .iter()
                    .filter_map(move |m| index.get(m).copied())
                    .filter(move |&j| j > i)
                    .map(move |j| (i, j))
            })
            .collect();
        let hops: Vec<usize> = shown.iter().map(|&(_, hops)| hops).collect();
        self.parents = vec![None; hops.len()];
        for &(a, b) in &self.edges {
            for (child, up) in [(a, b), (b, a)] {
                if hops[up] + 1 == hops[child] && self.parents[child].is_none() {
                    self.parents[child] = Some(up);
                }
            }
        }
        let positions = layout(&hops, &self.parents, &self.edges);
        self.hops = hops;
        self.shown = shown.into_iter().map(|(n, _)| n).zip(positions).collect();
        self.arrange(None);
    }

    /// The width of the name of the shown node `i`, as drawn.
    fn name_width(&self, i: usize) -> i32 {
        self.params.nodes[self.shown[i].0]
            .label
            .width()
            .min(NAME_WIDTH) as i32
    }

    /// Places the nodes at their scaled positions, each moved the least that keeps it and its
    /// name a cell clear of those placed before it, the centre first and the rest outwards,
    /// and within `rows` rows from the topmost position if given. Names point away from the
    /// centre where they can.
    fn arrange(&mut self, rows: Option<i32>) {
        if self.view == GraphView::Map {
            self.arrange_map();
            return;
        }
        self.routes.clear();
        let ideal: Vec<(i32, i32)> = self
            .shown
            .iter()
            .map(|&(_, (x, y))| {
                (
                    (x * self.scale.0).round() as i32,
                    (y * self.scale.1).round() as i32,
                )
            })
            .collect();
        let widths: Vec<i32> = (0..self.shown.len()).map(|i| self.name_width(i)).collect();
        // The taken cells, on a grid as far around the ideal cells as nodes move.
        let reach = (SHIFT.0 + NAME_WIDTH as i32 + 3, SHIFT.1 + 1);
        let (mut min, mut max) = ((i32::MAX, i32::MAX), (i32::MIN, i32::MIN));
        for &(x, y) in &ideal {
            min = (min.0.min(x - reach.0), min.1.min(y - reach.1));
            max = (max.0.max(x + reach.0), max.1.max(y + reach.1));
        }
        let columns = (max.0 - min.0 + 1) as usize;
        let mut taken = vec![false; columns * (max.1 - min.1 + 1) as usize];
        let row = |y: i32| (y - min.1) as usize * columns;
        let top = ideal.iter().map(|&(_, y)| y).min().unwrap_or(0);
        let within = |y: i32| rows.is_none_or(|rows| (top..top + rows).contains(&y));
        let offsets = shifts();
        self.placed = Vec::with_capacity(ideal.len());
        for (i, &(x, y)) in ideal.iter().enumerate() {
            let outwards_left = x < ideal[0].0;
            let place = offsets
                .iter()
                .filter(|&&(_, dy)| within(y + dy))
                .find_map(|&(dx, dy)| {
                    [outwards_left, !outwards_left]
                        .into_iter()
                        .find_map(|left| {
                            let place = Place {
                                x: x + dx,
                                y: y + dy,
                                left,
                            };
                            let (start, end) = place.span(widths[i]);
                            let row = row(place.y);
                            let clear =
                                (start - 1..end + 1).all(|c| !taken[row + (c - min.0) as usize]);
                            clear.then_some(place)
                        })
                })
                .unwrap_or(Place {
                    x,
                    y,
                    left: outwards_left,
                });
            let (start, end) = place.span(widths[i]);
            let row = row(place.y);
            for c in start..end {
                taken[row + (c - min.0) as usize] = true;
            }
            self.placed.push(place);
        }
    }

    fn cell(&self, i: usize) -> (i32, i32) {
        (self.placed[i].x, self.placed[i].y)
    }

    /// The braille dot of the shown node `i` that its links start at.
    fn dot(&self, i: usize) -> (i32, i32) {
        let (x, y) = self.cell(i);
        (x * 2 + 1, y * 4 + 2)
    }

    /// The shown node under the cursor, on its dot or its name.
    fn hovered(&self) -> Option<usize> {
        let (x, y) = self.cursor;
        (0..self.shown.len()).find(|&i| {
            let (start, end) = self.placed[i].span(self.name_width(i));
            self.placed[i].y == y && (start..end).contains(&x)
        })
    }

    /// The most room the graph gets: nine tenths of the screen, inside the popup's border or
    /// above its status row.
    fn room(&self) -> (u16, u16) {
        let (width, height) = (self.screen.width * 9 / 10, self.screen.height * 9 / 10);
        if self.bordered {
            (width.saturating_sub(2), height.saturating_sub(2))
        } else {
            (width, height.saturating_sub(1))
        }
    }

    /// The cells the nodes and their names cover, with a column of room on either side: the top
    /// left one and the one past the bottom right.
    fn content(&self) -> ((i32, i32), (i32, i32)) {
        let (mut min, mut max) = ((i32::MAX, i32::MAX), (i32::MIN, i32::MIN));
        for (i, place) in self.placed.iter().enumerate() {
            let (start, end) = place.span(self.name_width(i));
            min = (min.0.min(start - 1), min.1.min(place.y));
            max = (max.0.max(end + 1), max.1.max(place.y + 1));
        }
        (min, max)
    }

    /// Scales the graph to fill the room it gets, the cursor on the shown node `on`.
    fn fit_view(&mut self, on: usize) {
        if self.view == GraphView::Map {
            self.arrange_map();
            self.cursor = self.cell(on);
            self.recentre = true;
            self.settle();
            return;
        }
        let (width, height) = self.room();
        let (width, height) = (i32::from(width), i32::from(height));
        let (top, bottom) = self
            .shown
            .iter()
            .fold((f32::MAX, f32::MIN), |(top, bottom), &(_, (_, y))| {
                (top.min(y), bottom.max(y))
            });
        let most = FIT_SCALE.0 + FIT_SCALE.1 * self.shown.len() as f32;
        let down = ((height as f32 - 1.0) / (bottom - top).max(0.01)).min(most);
        // As wide as the names leave room for, with cells about twice as high as wide.
        let size = |graph: &mut Self, across: f32, down: f32| {
            graph.scale = (
                across.max(MIN_SCALE),
                down.min(across / (2.0 * STRETCH.0)).max(MIN_SCALE),
            );
            graph.arrange(Some(height));
            let (min, max) = graph.content();
            (max.0 - min.0, max.1 - min.1)
        };
        let (mut fits, mut over) = (MIN_SCALE, 2.0 * STRETCH.1 * down);
        if size(self, over, down).0 <= width {
            fits = over;
        } else {
            for _ in 0..10 {
                let across = (fits + over) / 2.0;
                if size(self, across, down).0 <= width {
                    fits = across;
                } else {
                    over = across;
                }
            }
        }
        size(self, fits, down);
        self.cursor = self.cell(on);
        self.recentre = true;
        self.settle();
    }

    /// Keeps the cursor on the graph and in view, and the graph in the middle where it fits.
    fn settle(&mut self) {
        let (min, max) = self.content();
        self.cursor = (
            self.cursor.0.clamp(min.0, max.0 - 1),
            self.cursor.1.clamp(min.1, max.1 - 1),
        );
        let recentre = self.recentre;
        let axis = |scroll: i32, cursor: i32, min: i32, max: i32, size: u16| {
            let size = i32::from(size).max(1);
            let scroll = if recentre { cursor - size / 2 } else { scroll };
            if max - min <= size {
                min - (size - (max - min)) / 2
            } else {
                scroll
                    .clamp(cursor - size + 1, cursor)
                    .clamp(min, max - size)
            }
        };
        self.scroll = (
            axis(self.scroll.0, self.cursor.0, min.0, max.0, self.inner.width),
            axis(
                self.scroll.1,
                self.cursor.1,
                min.1,
                max.1,
                self.inner.height,
            ),
        );
    }

    fn step(&mut self, (dx, dy): (i32, i32)) {
        self.cursor = (self.cursor.0 + dx, self.cursor.1 + dy);
        self.settle();
    }

    /// Moves the cursor to the nearest node in the direction `(dx, dy)`, at most 45° off it.
    fn jump(&mut self, (dx, dy): (i32, i32)) {
        let nearest = (0..self.shown.len())
            .filter_map(|i| {
                let (x, y) = self.cell(i);
                // Rows are about twice as high as columns are wide.
                let (x, y) = (x - self.cursor.0, (y - self.cursor.1) * 2);
                let along = x * dx + y * dy;
                let across = (x * dy - y * dx).abs();
                (along > 0 && across <= along).then_some((along * along + across * across, i))
            })
            .min();
        if let Some((_, i)) = nearest {
            self.cursor = self.cell(i);
            self.settle();
        }
    }

    /// Zooms by `factor`, keeping the cursor on the node it's on. Mind maps don't zoom.
    fn zoom_by(&mut self, factor: f32) {
        if self.view == GraphView::Map {
            return;
        }
        let hovered = self.hovered();
        let ratio = (self.scale.1 * factor).clamp(MIN_SCALE, MAX_SCALE) / self.scale.1;
        self.scale = (self.scale.0 * ratio, self.scale.1 * ratio);
        self.arrange(None);
        self.cursor = match hovered {
            Some(i) => self.cell(i),
            None => (
                (self.cursor.0 as f32 * ratio).round() as i32,
                (self.cursor.1 as f32 * ratio).round() as i32,
            ),
        };
        self.settle();
    }

    fn run(&mut self, action: Action, cx: &mut Context) -> EventResult {
        match action {
            Action::Left => self.step((-1, 0)),
            Action::Down => self.step((0, 1)),
            Action::Up => self.step((0, -1)),
            Action::Right => self.step((1, 0)),
            Action::JumpLeft => self.jump((-1, 0)),
            Action::JumpDown => self.jump((0, 1)),
            Action::JumpUp => self.jump((0, -1)),
            Action::JumpRight => self.jump((1, 0)),
            Action::ZoomIn => self.zoom_by(ZOOM_STEP),
            Action::ZoomOut => self.zoom_by(1.0 / ZOOM_STEP),
            Action::Fit => self.fit_view(self.hovered().unwrap_or(0)),
            Action::Depth(depth) => {
                let hovered = self.hovered().map(|i| self.shown[i].0);
                self.depth = depth;
                self.relayout();
                let on = hovered.and_then(|n| self.shown.iter().position(|&(m, _)| m == n));
                self.fit_view(on.unwrap_or(0));
            }
            Action::Centre => {
                if let Some(i) = self.hovered() {
                    self.centre = self.shown[i].0;
                    self.relayout();
                    self.fit_view(0);
                }
            }
            Action::View => {
                self.view = match self.view {
                    GraphView::Graph => GraphView::Map,
                    GraphView::Map => GraphView::Graph,
                };
                self.fit_view(self.hovered().unwrap_or(0));
            }
            Action::Preview => return self.preview(cx),
            Action::Open => {
                if let Some(i) = self.hovered() {
                    let node = &self.params.nodes[self.shown[i].0];
                    if let Some(command) = node.command.clone() {
                        cx.editor.execute_lsp_command(command, self.server);
                        return close();
                    }
                    let opened = helix_core::Uri::try_from(node.uri.clone())
                        .map_err(|err| err.to_string())
                        .and_then(|uri| {
                            let path = uri.as_path().ok_or("not a file")?.to_path_buf();
                            cx.editor
                                .open(&path, Open::Replace)
                                .map_err(|err| err.to_string())
                        });
                    if let Err(err) = opened {
                        cx.editor.set_error(format!("Failed to open: {err}"));
                    }
                    return close();
                }
            }
            Action::Help => self.info = Some(BINDINGS.info(&[], "Graph")),
            Action::Close => return close(),
        }
        EventResult::Consumed(None)
    }

    /// Shows the preview of the node under the cursor.
    fn preview(&self, cx: &mut Context) -> EventResult {
        let Some(i) = self.hovered() else {
            return EventResult::Consumed(None);
        };
        let Some(preview) = self.params.nodes[self.shown[i].0].preview.clone() else {
            cx.editor.set_status("No preview");
            return EventResult::Consumed(None);
        };
        let hover = lsp::Hover {
            contents: lsp::HoverContents::Markup(preview),
            range: None,
        };
        let server = cx
            .editor
            .language_server_by_id(self.server)
            .map_or_else(String::new, |server| server.name().to_owned());
        let contents = Hover::new(vec![(server, hover)], cx.editor.syn_loader.clone());
        let at = Position::new(
            (self.inner.y as i32 + self.cursor.1 - self.scroll.1) as usize,
            (self.inner.x as i32 + self.cursor.0 - self.scroll.0) as usize,
        );
        let popup = Popup::new(Hover::ID, contents)
            .auto_close(true)
            .position(Some(at));
        EventResult::Consumed(Some(Box::new(move |compositor, _| {
            compositor.replace_or_push(Hover::ID, popup)
        })))
    }

    fn draw(&self, surface: &mut Surface, theme: &Theme) {
        let inner = self.inner;
        let (width, height) = (inner.width as usize, inner.height as usize);
        let at = |(x, y): (i32, i32)| {
            let (x, y) = (x - self.scroll.0, y - self.scroll.1);
            let visible = (0..width as i32).contains(&x) && (0..height as i32).contains(&y);
            visible.then_some((x as usize, y as usize))
        };
        // Names take the file tree's colors, and the text's color where the theme has none.
        let popup = theme.get("ui.popup");
        let text = Style::default()
            .fg(Color::Reset)
            .patch(theme.get("ui.text"));
        let faint = text.patch(theme.get("ui.text.inactive"));
        let (link, lit) = (theme.get("ui.linenr"), text);
        let hovered = self.hovered();
        let map = self.view == GraphView::Map;

        // The links, lit when they are the hovered node's.
        let mut dots = vec![(0u16, false); width * height];
        let mut lines = vec![(0u8, false); width * height];
        for (node, route) in &self.routes {
            let is_lit = hovered.is_some_and(|i| i == *node || Some(i) == self.parents[*node]);
            for pair in route.windows(2) {
                let ((x0, y0), (x1, y1)) = (pair[0], pair[1]);
                let (dx, dy) = ((x1 - x0).signum(), (y1 - y0).signum());
                let (mut x, mut y) = (x0, y0);
                // Up, down, left and right.
                let toward = |dx: i32, dy: i32| match (dx, dy) {
                    (0, -1) => 1u8,
                    (0, 1) => 2,
                    (-1, 0) => 4,
                    _ => 8,
                };
                while (x, y) != (x1, y1) {
                    if let Some((cx, cy)) = at((x, y)) {
                        lines[cy * width + cx].0 |= toward(dx, dy);
                        lines[cy * width + cx].1 |= is_lit;
                    }
                    (x, y) = (x + dx, y + dy);
                    if let Some((cx, cy)) = at((x, y)) {
                        lines[cy * width + cx].0 |= toward(-dx, -dy);
                        lines[cy * width + cx].1 |= is_lit;
                    }
                }
            }
        }
        for (i, &(bits, is_lit)) in lines.iter().enumerate() {
            if bits != 0 {
                surface[(inner.x + (i % width) as u16, inner.y + (i / width) as u16)]
                    .set_char(box_glyph(bits))
                    .set_style(if is_lit { lit } else { link });
            }
        }
        for &(a, b) in self.edges.iter().filter(|_| !map) {
            let is_lit = hovered.is_some_and(|i| i == a || i == b);
            let (from, to) = (self.dot(a), self.dot(b));
            let steps = (to.0 - from.0).abs().max((to.1 - from.1).abs()).max(1);
            for step in 0..=steps {
                let t = step as f32 / steps as f32;
                let x = from.0 + ((to.0 - from.0) as f32 * t).round() as i32;
                let y = from.1 + ((to.1 - from.1) as f32 * t).round() as i32;
                if let Some((cx, cy)) = at((x.div_euclid(2), y.div_euclid(4))) {
                    let cell = &mut dots[cy * width + cx];
                    cell.0 |= braille::DOTS[y.rem_euclid(4) as usize][x.rem_euclid(2) as usize];
                    cell.1 |= is_lit;
                }
            }
        }
        for (i, &(dots, is_lit)) in dots.iter().enumerate() {
            if dots != 0 {
                let symbol = char::from_u32(u32::from(braille::BLANK | dots))
                    .expect("braille patterns are chars");
                surface[(inner.x + (i % width) as u16, inner.y + (i / width) as u16)]
                    .set_char(symbol)
                    .set_style(if is_lit { lit } else { link });
            }
        }

        // The nodes and their names, those of missing documents faint.
        let linked: Vec<usize> = self
            .edges
            .iter()
            .filter_map(|&(a, b)| match hovered {
                Some(i) if i == a => Some(b),
                Some(i) if i == b => Some(a),
                _ => None,
            })
            .collect();
        for (i, place) in self.placed.iter().enumerate() {
            let node = &self.params.nodes[self.shown[i].0];
            let style = if node.missing {
                faint
            } else {
                let file = Path::new(node.uri.path()).file_name().unwrap_or_default();
                let colors = self.colors.as_ref().and_then(|colors| {
                    colors.style(&file.to_string_lossy(), EntryType::File, None)
                });
                colors.map_or(text, |colors| text.patch(colors))
            };
            let style = if Some(i) == hovered {
                style.add_modifier(Modifier::BOLD)
            } else if map
                && linked.contains(&i)
                && !hovered
                    .is_some_and(|h| self.parents[i] == Some(h) || self.parents[h] == Some(i))
            {
                // A link the mind map has no line for.
                style.underline_style(helix_view::graphics::UnderlineStyle::Line)
            } else {
                style
            };
            let symbol = if i == 0 {
                "◉"
            } else if node.missing {
                "○"
            } else {
                "●"
            };
            let named = match self.labels {
                GraphLabels::All => true,
                GraphLabels::Linked => i == 0 || Some(i) == hovered || linked.contains(&i),
            };
            let name = if named {
                name(&node.label)
            } else {
                String::new()
            };
            let text = if place.left {
                format!("{name} {symbol}")
            } else {
                format!("{symbol} {name}")
            };
            let (start, _) = place.span(name.width() as i32);
            let (mut x, y) = (start - self.scroll.0, place.y - self.scroll.1);
            if !(0..height as i32).contains(&y) {
                continue;
            }
            for grapheme in text.chars() {
                let columns = grapheme.width().unwrap_or(0) as i32;
                if (0..width as i32).contains(&x) && x + columns <= width as i32 {
                    let (x, y) = (inner.x + x as u16, inner.y + y as u16);
                    // Over the links, without their colors and modifiers.
                    surface[(x, y)].reset();
                    let mut buf = [0; 4];
                    let grapheme = grapheme.encode_utf8(&mut buf);
                    surface.set_grapheme(x, y, grapheme, columns as usize, popup.patch(style));
                }
                x += columns;
            }
        }

        if let Some((x, y)) = at(self.cursor) {
            let cell = &mut surface[(inner.x + x as u16, inner.y + y as u16)];
            cell.set_style(theme.get("ui.cursor.primary"));
        }
    }
}

/// `label` cut to [`NAME_WIDTH`] columns.
fn name(label: &str) -> String {
    if label.width() <= NAME_WIDTH {
        return label.to_owned();
    }
    let mut name = String::new();
    let mut used = 1;
    for c in label.chars() {
        used += c.width().unwrap_or(0);
        if used > NAME_WIDTH {
            break;
        }
        name.push(c);
    }
    name.push('…');
    name
}

/// How far nodes move at most to keep clear of each other, in columns and rows.
const SHIFT: (i32, i32) = (24, 12);

/// The moves a node tries, nearest first, rows counting about twice as much as columns.
fn shifts() -> Vec<(i32, i32)> {
    let mut shifts: Vec<(i32, i32)> = (-SHIFT.1..=SHIFT.1)
        .flat_map(|dy| (-SHIFT.0..=SHIFT.0).map(move |dx| (dx, dy)))
        .collect();
    shifts.sort_by_key(|&(dx, dy)| (dx * dx + 4 * dy * dy, dy.abs(), dx.abs()));
    shifts
}

/// The nodes up to `depth` links from `centre` either way, `centre` first, and how many links
/// away each one is.
fn neighbourhood(links: &[Vec<usize>], centre: usize, depth: usize) -> Vec<(usize, usize)> {
    let mut found = vec![(centre, 0)];
    let mut seen = vec![false; links.len()];
    seen[centre] = true;
    let mut queue = VecDeque::from([(centre, 0)]);
    while let Some((node, hops)) = queue.pop_front() {
        if hops == depth {
            continue;
        }
        for &next in &links[node] {
            if !seen[next] {
                seen[next] = true;
                found.push((next, hops + 1));
                queue.push_back((next, hops + 1));
            }
        }
    }
    found
}

/// Positions of nodes `hops` links from the first one, at the origin: a radial tree. Nodes `n`
/// links away go on a ring `n` link lengths wide, in a slice of the ring next to the node they
/// hang off, their `parents`: each node's slice is split among the nodes that hang off it by how
/// many hang off those, in the order a force layout puts them in.
fn layout(hops: &[usize], parents: &[Option<usize>], edges: &[(usize, usize)]) -> Vec<(f32, f32)> {
    let n = hops.len();
    let forces = forces(hops, edges);
    let angle = |i: usize| forces[i].1.atan2(forces[i].0);
    let mut children = vec![Vec::new(); n];
    for (i, parent) in parents.iter().enumerate() {
        if let Some(parent) = parent {
            children[*parent].push(i);
        }
    }
    for children in &mut children {
        children.sort_by(|&a, &b| angle(a).total_cmp(&angle(b)));
    }
    // Hubs get more room, but not so much that the nodes beside them crowd.
    let mut hanging = vec![0usize; n];
    let mut deepest_first: Vec<usize> = (1..n).collect();
    deepest_first.sort_by_key(|&i| std::cmp::Reverse(hops[i]));
    for &i in &deepest_first {
        if let Some(parent) = parents[i] {
            hanging[parent] += hanging[i] + 1;
        }
    }
    let weight = |i: usize| (hanging[i] as f32).sqrt().max(1.0);
    let mut positions = vec![(0.0f32, 0.0f32); n];
    let start = children[0].first().map_or(0.0, |&i| angle(i));
    let mut slices = vec![(0, start, start + std::f32::consts::TAU)];
    while let Some((node, from, to)) = slices.pop() {
        let total: f32 = children[node].iter().map(|&c| weight(c)).sum();
        let mut at = from;
        for &c in &children[node] {
            let slice = (to - from) * weight(c) / total;
            let middle = at + slice / 2.0;
            let radius = hops[c] as f32;
            positions[c] = (radius * middle.cos(), radius * middle.sin());
            slices.push((c, at, at + slice));
            at += slice;
        }
    }
    positions
}

/// Positions of nodes `hops` links from the first one, in link lengths, so that linked ones
/// are about a link's length apart: a force-directed layout (Fruchterman and Reingold), starting
/// from rings around the first node, which stays at the origin.
fn forces(hops: &[usize], edges: &[(usize, usize)]) -> Vec<(f32, f32)> {
    let n = hops.len();
    let mut positions = vec![(0.0f32, 0.0f32); n];
    for ring in 1..=hops.iter().copied().max().unwrap_or(0) {
        let members: Vec<usize> = (0..n).filter(|&i| hops[i] == ring).collect();
        for (k, &i) in members.iter().enumerate() {
            let angle =
                std::f32::consts::TAU * (k as f32 + 0.5 * ring as f32) / members.len() as f32;
            positions[i] = (ring as f32 * angle.cos(), ring as f32 * angle.sin());
        }
    }
    // shortcut: O(n²) per iteration, fine for the few hundred nodes two links reach.
    let mut temperature = 1.0;
    for _ in 0..200 {
        let mut forces = vec![(0.0f32, 0.0f32); n];
        for i in 0..n {
            for j in i + 1..n {
                let (dx, dy) = (
                    positions[i].0 - positions[j].0,
                    positions[i].1 - positions[j].1,
                );
                let squared = (dx * dx + dy * dy).max(1e-4);
                let push = (dx / squared, dy / squared);
                forces[i] = (forces[i].0 + push.0, forces[i].1 + push.1);
                forces[j] = (forces[j].0 - push.0, forces[j].1 - push.1);
            }
        }
        for &(i, j) in edges {
            let (dx, dy) = (
                positions[i].0 - positions[j].0,
                positions[i].1 - positions[j].1,
            );
            let distance = (dx * dx + dy * dy).sqrt();
            let pull = (dx * distance, dy * distance);
            forces[i] = (forces[i].0 - pull.0, forces[i].1 - pull.1);
            forces[j] = (forces[j].0 + pull.0, forces[j].1 + pull.1);
        }
        for i in 1..n {
            let (fx, fy) = forces[i];
            let length = (fx * fx + fy * fy).sqrt().max(1e-6);
            let step = length.min(temperature) / length;
            positions[i] = (positions[i].0 + fx * step, positions[i].1 + fy * step);
        }
        temperature *= 0.97;
    }
    positions
}

/// The rounded line that joins a cell's neighbours `bits`: up, down, left and right.
fn box_glyph(bits: u8) -> char {
    let set = ['─', '│', '╭', '╮', '╰', '╯', '├', '┤', '┬', '┴', '┼'];
    let (up, down, left, right) = (bits & 1 != 0, bits & 2 != 0, bits & 4 != 0, bits & 8 != 0);
    match (up, down, left, right) {
        (false, false, _, _) => set[0],
        (_, _, false, false) => set[1],
        (false, true, false, true) => set[2],
        (false, true, true, false) => set[3],
        (true, false, false, true) => set[4],
        (true, false, true, false) => set[5],
        (true, true, false, true) => set[6],
        (true, true, true, false) => set[7],
        (false, true, true, true) => set[8],
        (true, false, true, true) => set[9],
        (true, true, true, true) => set[10],
    }
}

impl Graph {
    /// Places the nodes as a mind map: the centre in the middle, the nodes a link away in a
    /// column on either side, those two links away in a column beyond, next to the node they
    /// hang off, with lines along buses between the columns. On one side if both don't fit.
    fn arrange_map(&mut self) {
        self.arrange_sides(true);
        let (min, max) = self.content();
        if max.0 - min.0 > i32::from(self.room().0) {
            self.arrange_sides(false);
        }
    }

    /// Places the nodes of a mind map, all on the right but for `two_sided`, the nodes a link
    /// away in the order the rings have them, clockwise from the top.
    fn arrange_sides(&mut self, two_sided: bool) {
        use std::f32::consts::{FRAC_PI_2, TAU};
        let n = self.shown.len();
        let turn = |i: usize| {
            let (x, y) = self.shown[i].1;
            (y.atan2(x) + FRAC_PI_2).rem_euclid(TAU)
        };
        let mut children = vec![Vec::new(); n];
        for (i, parent) in self.parents.iter().enumerate() {
            if let Some(parent) = parent {
                children[*parent].push(i);
            }
        }
        for children in &mut children {
            children.sort_by(|&a, &b| turn(a).total_cmp(&turn(b)));
        }
        // The rows of each node and those hanging off it, one each at least.
        let mut rows = vec![1i32; n];
        let mut deepest_first: Vec<usize> = (0..n).collect();
        deepest_first.sort_by_key(|&i| std::cmp::Reverse(self.hops[i]));
        for &i in &deepest_first {
            if !children[i].is_empty() {
                rows[i] = children[i].iter().map(|&c| rows[c]).sum();
            }
        }
        let total: i32 = children[0].iter().map(|&p| rows[p]).sum();
        // Clockwise from the top: the right side top down, then the left side bottom up.
        let mut right = Vec::new();
        let mut taken = 0;
        let mut rest = children[0].iter().copied().peekable();
        while let Some(&p) = rest.peek() {
            if two_sided && taken > 0 && 2 * (taken + rows[p]) > total + rows[p] {
                break;
            }
            taken += rows[p];
            right.push(p);
            rest.next();
        }
        let left: Vec<usize> = rest.rev().collect();
        let side_rows = |side: &[usize], gap: i32| -> i32 {
            side.iter().map(|&p| rows[p]).sum::<i32>() + gap * (side.len() as i32 - 1).max(0)
        };
        let room = i32::from(self.room().1);
        let gap = i32::from(side_rows(&right, 1).max(side_rows(&left, 1)) <= room);
        let height = side_rows(&right, gap).max(side_rows(&left, gap)).max(1);
        let centre_row = (height - 1) / 2;
        let width = |i: usize| self.name_width(i);
        let mut placed = vec![
            Place {
                x: 0,
                y: centre_row,
                left: false
            };
            n
        ];
        let mut routes = Vec::new();
        for (side, nodes) in [(1, right), (-1, left)] {
            let hanging = |p: usize| -> Vec<usize> {
                if side > 0 {
                    children[p].clone()
                } else {
                    children[p].iter().rev().copied().collect()
                }
            };
            // Columns: where the lines leave the centre, then a bus and the nodes for each
            // number of links away.
            let mut levels = vec![nodes.clone()];
            while let Some(next) = levels
                .last()
                .map(|level| level.iter().flat_map(|&p| hanging(p)).collect::<Vec<_>>())
                .filter(|next| !next.is_empty())
            {
                levels.push(next);
            }
            let start = if side > 0 { 2 + width(0) + 1 } else { -1 };
            let mut buses = vec![start + side * 2];
            let mut columns = vec![buses[0] + side * 2];
            for level in &levels[..levels.len() - 1] {
                let widest = level.iter().map(|&i| width(i)).max().unwrap_or(0);
                let bus = columns[columns.len() - 1] + side * (widest + 5);
                buses.push(bus);
                columns.push(bus + side * 2);
            }
            // Each node in the middle of the rows of those hanging off it.
            let mut top = (height - side_rows(&nodes, gap)) / 2;
            let mut stack = Vec::new();
            for &p in &nodes {
                let row = top + (rows[p] - 1) / 2;
                let route = vec![
                    (start, centre_row),
                    (buses[0], centre_row),
                    (buses[0], row),
                    (columns[0] - side, row),
                ];
                routes.push((p, route));
                stack.push((p, 0, top));
                top += rows[p] + gap;
            }
            while let Some((p, level, top)) = stack.pop() {
                let row = top + (rows[p] - 1) / 2;
                placed[p] = Place {
                    x: columns[level],
                    y: row,
                    left: side < 0,
                };
                let end = columns[level] + side * (width(p) + 3);
                let mut child_top = top;
                for c in hanging(p) {
                    let child_row = child_top + (rows[c] - 1) / 2;
                    let bus = buses[level + 1];
                    let route = vec![
                        (end, row),
                        (bus, row),
                        (bus, child_row),
                        (columns[level + 1] - side, child_row),
                    ];
                    routes.push((c, route));
                    stack.push((c, level + 1, child_top));
                    child_top += rows[c];
                }
            }
        }
        self.placed = placed;
        self.routes = routes;
    }
}

fn close() -> EventResult {
    EventResult::Consumed(Some(Box::new(|compositor, _| {
        compositor.remove(Graph::ID);
    })))
}

impl Component for Graph {
    fn handle_event(&mut self, event: &Event, cx: &mut Context) -> EventResult {
        let mut key = match event {
            Event::Key(key) => *key,
            Event::Paste(_) | Event::Resize(..) => return EventResult::Consumed(None),
            _ => return EventResult::Ignored(None),
        };
        canonicalize_key(&mut key);
        self.info = None;
        let mut sequence = std::mem::take(&mut self.pending);
        sequence.push(key);
        match BINDINGS.lookup(&sequence) {
            Lookup::Action(action) => return self.run(action, cx),
            Lookup::Prefix => {
                self.info = Some(BINDINGS.info(&sequence, "Graph"));
                self.pending = sequence;
            }
            Lookup::Unbound => {}
        }
        EventResult::Consumed(None)
    }

    fn render(&mut self, area: Rect, surface: &mut Surface, cx: &mut Context) {
        self.paint(area, surface, &cx.editor.theme, cx.editor.popup_border());
        if let Some(info) = &mut self.info {
            info.render(area, surface, cx);
        }
    }

    fn id(&self) -> Option<&'static str> {
        Some(Self::ID)
    }
}

impl Graph {
    /// Draws the popup in the middle of `area`.
    fn paint(&mut self, area: Rect, surface: &mut Surface, theme: &Theme, bordered: bool) {
        self.screen = area;
        self.bordered = bordered;
        if self.fit {
            self.fit = false;
            self.fit_view(0);
        }

        // A popup in the middle of the screen as big as the graph, or as much of it as fits.
        let title = format!(" {} ", self.params.nodes[self.centre].label);
        let (room, (min, max)) = (self.room(), self.content());
        let size = (
            (max.0 - min.0)
                .max(title.width() as i32 + 2)
                .min(i32::from(room.0)) as u16,
            (max.1 - min.1).min(i32::from(room.1)) as u16,
        );
        let chrome = if self.bordered { (2, 2) } else { (0, 1) };
        let popup = Rect::new(
            area.x + area.width.saturating_sub(size.0 + chrome.0) / 2,
            area.y + area.height.saturating_sub(size.1 + chrome.1) / 2,
            size.0 + chrome.0,
            size.1 + chrome.1,
        )
        .intersection(area);
        let text = theme.get("ui.text");
        surface.clear_with(popup, theme.get("ui.popup"));
        self.inner = if self.bordered {
            let block = Block::bordered().title(title);
            let inner = block.inner(popup);
            block.render(popup, surface);
            inner
        } else {
            popup.clip_bottom(1)
        };
        self.settle();
        self.recentre = false;
        self.draw(surface, theme);

        let hovered = self
            .hovered()
            .map(|i| format!("{} · ", self.params.nodes[self.shown[i].0].label))
            .unwrap_or_default();
        let status = format!(" {hovered}depth {} ", self.depth);
        let indent = u16::from(self.bordered);
        surface.set_stringn(
            popup.x + indent,
            popup.bottom().saturating_sub(1),
            &status,
            popup.width.saturating_sub(2 * indent) as usize,
            text,
        );
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Action {
    Left,
    Down,
    Up,
    Right,
    JumpLeft,
    JumpDown,
    JumpUp,
    JumpRight,
    ZoomIn,
    ZoomOut,
    Fit,
    Depth(usize),
    View,
    Centre,
    Preview,
    Open,
    Help,
    Close,
}

impl panel_keys::Action for Action {
    fn doc(self) -> &'static str {
        match self {
            Self::Left => "Move left",
            Self::Down => "Move down",
            Self::Up => "Move up",
            Self::Right => "Move right",
            Self::JumpLeft => "Go to the nearest document on the left",
            Self::JumpDown => "Go to the nearest document below",
            Self::JumpUp => "Go to the nearest document above",
            Self::JumpRight => "Go to the nearest document on the right",
            Self::ZoomIn => "Zoom in",
            Self::ZoomOut => "Zoom out",
            Self::Fit => "Fit the whole graph",
            Self::Depth(1) => "Show the documents a link away",
            Self::Depth(2) => "Show the documents up to two links away",
            Self::Depth(_) => "Show the documents up to three links away",
            Self::View => "Toggle the mind map",
            Self::Centre => "Centre the graph on the document",
            Self::Preview => "Preview the document",
            Self::Open => "Open the document",
            Self::Help => "Show these keys",
            Self::Close => "Close the graph",
        }
    }
}

const BINDINGS: Bindings<Action> = Bindings(&[
    bind(&[&[key!('h')], &[key!(Left)]], Action::Left),
    bind(&[&[key!('j')], &[key!(Down)]], Action::Down),
    bind(&[&[key!('k')], &[key!(Up)]], Action::Up),
    bind(&[&[key!('l')], &[key!(Right)]], Action::Right),
    bind(&[&[key!('H')]], Action::JumpLeft),
    bind(&[&[key!('J')]], Action::JumpDown),
    bind(&[&[key!('K')]], Action::JumpUp),
    bind(&[&[key!('L')]], Action::JumpRight),
    bind(&[&[key!('+')]], Action::ZoomIn),
    bind(&[&[key!('-')]], Action::ZoomOut),
    bind(&[&[key!('0')]], Action::Fit),
    bind(&[&[key!('1')]], Action::Depth(1)),
    bind(&[&[key!('2')]], Action::Depth(2)),
    bind(&[&[key!('3')]], Action::Depth(3)),
    bind(&[&[key!('m')]], Action::View),
    bind(&[&[key!(Enter)]], Action::Centre),
    bind(&[&[key!(' '), key!('k')]], Action::Preview),
    bind(&[&[key!(' '), key!(Enter)]], Action::Open),
    bind(&[&[key!('?')]], Action::Help),
    bind(&[&[key!(Esc)], &[ctrl!('c')]], Action::Close),
]);

#[cfg(test)]
mod tests {
    use super::*;

    fn graph(edges: &[(usize, usize)], n: usize) -> Graph {
        let nodes = (0..n)
            .map(|i| helix_lsp::GraphNode {
                label: format!("Note {i}"),
                uri: lsp::Url::parse(&format!("file:///vault/Note{i}.typ")).unwrap(),
                preview: None,
                missing: false,
                command: None,
            })
            .collect();
        let params = ShowGraphParams {
            nodes,
            edges: edges.to_vec(),
            focus: 0,
        };
        Graph::new(params, LanguageServerId::default(), &Config::default())
    }

    #[test]
    fn every_sequence_is_bound_once() {
        assert!(BINDINGS.are_unique());
    }

    #[test]
    fn neighbourhoods_follow_links_both_ways() {
        // 0 -> 1, 2 -> 0, 1 -> 3, 3 -> 4
        let graph = graph(&[(0, 1), (2, 0), (1, 3), (3, 4), (1, 1)], 5);
        let nodes = |depth| {
            let mut nodes: Vec<usize> = neighbourhood(&graph.links, 0, depth)
                .into_iter()
                .map(|(n, _)| n)
                .collect();
            nodes.sort_unstable();
            nodes
        };
        assert_eq!(nodes(1), [0, 1, 2]);
        assert_eq!(nodes(2), [0, 1, 2, 3]);
        assert_eq!(graph.shown.len(), 4);
        assert_eq!(graph.shown[0].0, 0);
    }

    #[test]
    fn layouts_keep_linked_nodes_closer() {
        // A path 0 - 1 - 2 - 3.
        let edges = [(0, 1), (1, 2), (2, 3)];
        let positions = forces(&[0, 1, 2, 3], &edges);
        assert_eq!(positions[0], (0.0, 0.0));
        assert_eq!(positions, forces(&[0, 1, 2, 3], &edges));
        let distance = |a: usize, b: usize| {
            let (dx, dy) = (
                positions[a].0 - positions[b].0,
                positions[a].1 - positions[b].1,
            );
            (dx * dx + dy * dy).sqrt()
        };
        assert!(distance(0, 1) > 0.5 && distance(0, 1) < 2.0);
        assert!(distance(0, 3) > distance(0, 1));
        assert!(positions
            .iter()
            .all(|(x, y)| x.is_finite() && y.is_finite()));
    }

    #[test]
    fn capital_motions_go_to_the_nearest_node_that_way() {
        let mut graph = graph(&[(0, 1), (0, 2), (0, 3)], 4);
        graph.inner = Rect::new(0, 0, 80, 20);
        graph.fit = false;
        let place = |x, y| Place { x, y, left: false };
        graph.placed = vec![place(0, 0), place(20, 1), place(10, 0), place(2, 9)];
        graph.cursor = (0, 0);
        graph.jump((1, 0));
        assert_eq!(graph.hovered(), Some(2));
        graph.jump((1, 0));
        assert_eq!(graph.hovered(), Some(1));
        // 3 is more left than down from 1.
        graph.jump((0, 1));
        assert_eq!(graph.hovered(), Some(1));
        graph.jump((-1, 0));
        assert_eq!(graph.hovered(), Some(2));
        graph.jump((0, 1));
        assert_eq!(graph.hovered(), Some(3));
        graph.jump((0, 1));
        assert_eq!(graph.hovered(), Some(3));
        // On its name.
        graph.step((1, 0));
        assert_eq!(graph.hovered(), Some(3));
        // The cursor stays on the graph: a column left of its leftmost node.
        for _ in 0..20 {
            graph.step((-1, 0));
        }
        assert_eq!(graph.cursor, (-1, 9));
    }

    #[test]
    fn nodes_and_names_keep_clear_of_each_other() {
        // A star: everything links with 0, all laid out on top of each other.
        let mut graph = graph(&(1..12).map(|i| (0, i)).collect::<Vec<_>>(), 12);
        for (_, position) in &mut graph.shown {
            *position = (0.0, 0.0);
        }
        graph.arrange(None);
        let spans: Vec<(i32, i32, i32)> = (0..12)
            .map(|i| {
                let (start, end) = graph.placed[i].span(graph.name_width(i));
                (graph.placed[i].y, start, end)
            })
            .collect();
        for (i, a) in spans.iter().enumerate() {
            for b in &spans[i + 1..] {
                // A cell apart at least.
                assert!(a.0 != b.0 || a.2 < b.1 || b.2 < a.1, "{a:?} {b:?}");
            }
        }
        // The centre stays put.
        assert_eq!(graph.cell(0), (0, 0));
    }

    #[test]
    fn names_count_as_their_nodes() {
        let mut graph = graph(&[(0, 1)], 2);
        graph.inner = Rect::new(0, 0, 80, 20);
        graph.placed = vec![
            Place {
                x: 0,
                y: 0,
                left: false,
            },
            Place {
                x: 20,
                y: 3,
                left: true,
            },
        ];
        // "● Note 0" and "Note 1 ●".
        for (cursor, hovered) in [
            ((0, 0), Some(0)),
            ((7, 0), Some(0)),
            ((8, 0), None),
            ((13, 3), Some(1)),
            ((20, 3), Some(1)),
            ((21, 3), None),
        ] {
            graph.cursor = cursor;
            assert_eq!(graph.hovered(), hovered, "{cursor:?}");
        }
    }

    #[test]
    fn mind_maps_hang_nodes_off_their_parents() {
        // 0 links 1 and 2; 1 links 3 and 4; 2 links 5.
        let mut graph = graph(&[(0, 1), (0, 2), (1, 3), (1, 4), (2, 5)], 6);
        graph.screen = Rect::new(0, 0, 200, 50);
        graph.view = GraphView::Map;
        graph.arrange(None);
        let at = |graph: &Graph, node: usize| {
            let i = graph.shown.iter().position(|&(n, _)| n == node).unwrap();
            graph.placed[i]
        };
        // Children are a column beyond their parent, on its side, in rows of their own.
        let centre = at(&graph, 0).x;
        for (parent, child) in [(1, 3), (1, 4), (2, 5)] {
            let (parent, child) = (at(&graph, parent), at(&graph, child));
            assert_eq!(parent.left, child.left);
            assert!((child.x - centre).abs() > (parent.x - centre).abs());
        }
        assert_ne!(at(&graph, 3).y, at(&graph, 4).y);
        // A line for each link from a node to the one it hangs off.
        assert_eq!(graph.routes.len(), 5);
        // Too narrow for both sides: all on the right.
        graph.screen = Rect::new(0, 0, 30, 50);
        graph.arrange(None);
        assert!((1..6).all(|node| !at(&graph, node).left));
    }

    #[test]
    fn lines_join_their_neighbours() {
        assert_eq!(box_glyph(4 | 8), '─');
        assert_eq!(box_glyph(1 | 2), '│');
        assert_eq!(box_glyph(2 | 8), '╭');
        assert_eq!(box_glyph(1 | 2 | 8), '├');
        assert_eq!(box_glyph(1 | 2 | 4 | 8), '┼');
    }

    #[test]
    fn long_names_are_cut() {
        assert_eq!(name("Functor"), "Functor");
        let long = "IMO 10 - More Euclidian, More Vector, More Spaces";
        assert_eq!(name(long).width(), NAME_WIDTH);
        assert!(name(long).ends_with('…'));
    }
}
