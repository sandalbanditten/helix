//! A graph of documents that a language server shows (`helix/showGraph`), like notes and the
//! links between them: the documents one or two links from one, drawn in braille.

use std::collections::{HashMap, VecDeque};

use helix_core::{
    unicode::width::{UnicodeWidthChar, UnicodeWidthStr},
    Position,
};
use helix_lsp::{lsp, ShowGraphParams};
use helix_view::{
    editor::{Action as Open, GraphLabels},
    graphics::{Modifier, Rect},
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
        lsp::hover::Hover,
        panel_keys::{self, bind, Bindings, Lookup},
        Popup,
    },
};

/// The dots per link length that fitting the graph zooms in to at most.
const FIT_ZOOM: f32 = 32.0;
const MIN_ZOOM: f32 = 1.0;
const MAX_ZOOM: f32 = 128.0;
const ZOOM_STEP: f32 = 1.5;

pub struct Graph {
    params: ShowGraphParams,
    /// The nodes each node links with, either way.
    links: Vec<Vec<usize>>,
    /// The language server, which names previews.
    server: String,
    labels: GraphLabels,
    centre: usize,
    /// How many links from the centre nodes are shown.
    depth: usize,
    /// The shown nodes, the centre first, and their positions in link lengths.
    shown: Vec<(usize, (f32, f32))>,
    /// The links between shown nodes, as indices into `shown`.
    edges: Vec<(usize, usize)>,
    /// Dots per link length, and how much wider than high the graph is drawn: names need the
    /// room more than links.
    zoom: f32,
    stretch: f32,
    /// The cursor's cell and the area's top left cell on the graph's canvas.
    cursor: (i32, i32),
    scroll: (i32, i32),
    /// The area the graph was last drawn in.
    inner: Rect,
    /// Whether to fit the graph into its area when it's drawn next.
    fit: bool,
    pending: Vec<KeyEvent>,
    info: Option<Info>,
}

impl Graph {
    pub const ID: &'static str = "graph";

    /// The graph of `params`, centred on its focus, which must be one of its nodes.
    pub fn new(params: ShowGraphParams, server: String, labels: GraphLabels) -> Self {
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
            labels,
            depth: 2,
            shown: Vec::new(),
            edges: Vec::new(),
            zoom: FIT_ZOOM,
            stretch: 1.0,
            cursor: (0, 0),
            scroll: (0, 0),
            inner: Rect::default(),
            fit: true,
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
        let positions = layout(&hops, &self.edges);
        self.shown = shown.into_iter().map(|(n, _)| n).zip(positions).collect();
    }

    /// The dot of the shown node `i` on the canvas.
    fn dot(&self, i: usize) -> (i32, i32) {
        let (x, y) = self.shown[i].1;
        let zoom = (self.zoom * self.stretch, self.zoom);
        ((x * zoom.0).round() as i32, (y * zoom.1).round() as i32)
    }

    fn cell(&self, i: usize) -> (i32, i32) {
        let (x, y) = self.dot(i);
        (x.div_euclid(2), y.div_euclid(4))
    }

    /// The shown node under the cursor, the one nearest the cell's middle if several are.
    fn hovered(&self) -> Option<usize> {
        let middle = (self.cursor.0 * 2 + 1, self.cursor.1 * 4 + 2);
        (0..self.shown.len())
            .filter(|&i| self.cell(i) == self.cursor)
            .min_by_key(|&i| {
                let (x, y) = self.dot(i);
                (x - middle.0).pow(2) + (y - middle.1).pow(2)
            })
    }

    /// Zooms so that the whole graph fits, the cursor on the shown node `on`.
    fn fit_view(&mut self, on: usize) {
        let (width, height) = (self.inner.width as f32, self.inner.height as f32);
        let (mut min, mut max) = ((f32::MAX, f32::MAX), (f32::MIN, f32::MIN));
        for &(_, (x, y)) in &self.shown {
            min = (min.0.min(x), min.1.min(y));
            max = (max.0.max(x), max.1.max(y));
        }
        let span = ((max.0 - min.0).max(0.01), (max.1 - min.1).max(0.01));
        // A cell of room on each side.
        let fits = ((width - 2.0) * 2.0 / span.0, (height - 2.0) * 4.0 / span.1);
        self.zoom = fits.0.min(fits.1).clamp(MIN_ZOOM, FIT_ZOOM);
        self.stretch = (fits.0 / self.zoom).clamp(1.0, 2.0);
        let middle = (
            ((min.0 + max.0) / 2.0 * self.zoom * self.stretch / 2.0).floor() as i32,
            ((min.1 + max.1) / 2.0 * self.zoom / 4.0).floor() as i32,
        );
        self.scroll = (
            middle.0 - self.inner.width as i32 / 2,
            middle.1 - self.inner.height as i32 / 2,
        );
        self.cursor = self.cell(on);
        self.follow();
    }

    /// Scrolls the cursor into view.
    fn follow(&mut self) {
        let (width, height) = (
            (self.inner.width as i32).max(1),
            (self.inner.height as i32).max(1),
        );
        self.scroll.0 = self
            .scroll
            .0
            .clamp(self.cursor.0 - width + 1, self.cursor.0);
        self.scroll.1 = self
            .scroll
            .1
            .clamp(self.cursor.1 - height + 1, self.cursor.1);
    }

    fn step(&mut self, (dx, dy): (i32, i32)) {
        self.cursor = (self.cursor.0 + dx, self.cursor.1 + dy);
        self.follow();
    }

    /// Moves the cursor to the nearest node in the direction `(dx, dy)`, at most 45° off it.
    fn jump(&mut self, (dx, dy): (i32, i32)) {
        let nearest = (0..self.shown.len())
            .filter_map(|i| {
                let (x, y) = self.cell(i);
                // In dots, which are about as wide as high.
                let (x, y) = ((x - self.cursor.0) * 2, (y - self.cursor.1) * 4);
                let along = x * dx + y * dy;
                let across = (x * dy - y * dx).abs();
                (along > 0 && across <= along).then_some((along * along + across * across, i))
            })
            .min();
        if let Some((_, i)) = nearest {
            self.cursor = self.cell(i);
            self.follow();
        }
    }

    /// Zooms by `factor`, keeping the cursor where it is on the screen and on the node it's on.
    fn zoom_by(&mut self, factor: f32) {
        let hovered = self.hovered();
        let screen = (self.cursor.0 - self.scroll.0, self.cursor.1 - self.scroll.1);
        let ratio = (self.zoom * factor).clamp(MIN_ZOOM, MAX_ZOOM) / self.zoom;
        self.zoom *= ratio;
        self.cursor = match hovered {
            Some(i) => self.cell(i),
            None => (
                ((self.cursor.0 as f32 + 0.5) * ratio).floor() as i32,
                ((self.cursor.1 as f32 + 0.5) * ratio).floor() as i32,
            ),
        };
        self.scroll = (self.cursor.0 - screen.0, self.cursor.1 - screen.1);
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
            Action::Depth => {
                let hovered = self.hovered().map(|i| self.shown[i].0);
                self.depth = if self.depth == 1 { 2 } else { 1 };
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
            Action::Preview => return self.preview(cx),
            Action::Open => {
                if let Some(i) = self.hovered() {
                    let uri = self.params.nodes[self.shown[i].0].uri.clone();
                    let opened = helix_core::Uri::try_from(uri)
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
        let contents = Hover::new(
            vec![(self.server.clone(), hover)],
            cx.editor.syn_loader.clone(),
        );
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
        let text = theme.get("ui.text");
        let hovered = self.hovered();

        // The links, lit when they are the hovered node's.
        let mut dots = vec![(0u16, false); width * height];
        for &(a, b) in &self.edges {
            let lit = hovered.is_some_and(|i| i == a || i == b);
            let (from, to) = (self.dot(a), self.dot(b));
            let steps = (to.0 - from.0).abs().max((to.1 - from.1).abs()).max(1);
            for step in 0..=steps {
                let t = step as f32 / steps as f32;
                let x = from.0 + ((to.0 - from.0) as f32 * t).round() as i32;
                let y = from.1 + ((to.1 - from.1) as f32 * t).round() as i32;
                if let Some((cx, cy)) = at((x.div_euclid(2), y.div_euclid(4))) {
                    let cell = &mut dots[cy * width + cx];
                    cell.0 |= braille::DOTS[y.rem_euclid(4) as usize][x.rem_euclid(2) as usize];
                    cell.1 |= lit;
                }
            }
        }
        let (link, lit) = (theme.get("ui.text.inactive"), text);
        for (i, &(dots, is_lit)) in dots.iter().enumerate() {
            if dots != 0 {
                let symbol = char::from_u32(u32::from(braille::BLANK | dots))
                    .expect("braille patterns are chars");
                surface[(inner.x + (i % width) as u16, inner.y + (i / width) as u16)]
                    .set_char(symbol)
                    .set_style(if is_lit { lit } else { link });
            }
        }

        // The nodes, then their names where they fit.
        let centre = theme.get("ui.text.focus");
        let mut taken = vec![false; width * height];
        for i in 0..self.shown.len() {
            if let Some((x, y)) = at(self.cell(i)) {
                taken[y * width + x] = true;
                let (symbol, style) = if i == 0 {
                    ('◉', centre)
                } else {
                    ('●', text)
                };
                surface[(inner.x + x as u16, inner.y + y as u16)]
                    .set_char(symbol)
                    .set_style(style);
            }
        }
        let linked: Vec<usize> = self
            .edges
            .iter()
            .filter_map(|&(a, b)| match hovered {
                Some(i) if i == a => Some(b),
                Some(i) if i == b => Some(a),
                _ => None,
            })
            .collect();
        let rank = |i: usize| {
            if i == 0 {
                0
            } else if Some(i) == hovered {
                1
            } else if linked.contains(&i) {
                2
            } else {
                3
            }
        };
        let most = match self.labels {
            GraphLabels::All => 3,
            GraphLabels::Linked => 2,
        };
        let mut named: Vec<usize> = (0..self.shown.len()).filter(|&i| rank(i) <= most).collect();
        named.sort_by_key(|&i| rank(i));
        for i in named {
            let Some((x, y)) = at(self.cell(i)) else {
                continue;
            };
            let label = &self.params.nodes[self.shown[i].0].label;
            let row = &mut taken[y * width..(y + 1) * width];
            let Some((start, name)) = place(row, x, label) else {
                continue;
            };
            let style = if i == 0 {
                centre
            } else if Some(i) == hovered {
                text.add_modifier(Modifier::BOLD)
            } else {
                text
            };
            surface.set_stringn(
                inner.x + start as u16,
                inner.y + y as u16,
                &name,
                width - start,
                style,
            );
        }

        if let Some((x, y)) = at(self.cursor) {
            let cell = &mut surface[(inner.x + x as u16, inner.y + y as u16)];
            cell.set_style(theme.get("ui.cursor.primary"));
        }
    }
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

/// Positions of nodes `hops` links from the first one, in link lengths, so that linked ones
/// are about a link's length apart: a force-directed layout (Fruchterman and Reingold), starting
/// from rings around the first node, which stays at the origin.
fn layout(hops: &[usize], edges: &[(usize, usize)]) -> Vec<(f32, f32)> {
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

/// Where the name of the node at column `x` goes in a row of cells, `taken` where nodes and
/// names are: right of the node, else left of it, else cut short on the right. Takes its cells.
fn place(taken: &mut [bool], x: usize, name: &str) -> Option<(usize, String)> {
    let free = |cells: &mut dyn Iterator<Item = usize>| cells.take_while(|&c| !taken[c]).count();
    let right = free(&mut (x + 1..taken.len()));
    let left = free(&mut (0..x).rev());
    // A space between the node and its name.
    let width = name.width() + 1;
    let (start, text) = if width <= right {
        (x + 1, format!(" {name}"))
    } else if width <= left {
        (x - width, format!("{name} "))
    } else if right >= 4 {
        let mut text = String::from(" ");
        let mut used = 2;
        for c in name.chars() {
            used += c.width().unwrap_or(0);
            if used > right {
                break;
            }
            text.push(c);
        }
        text.push('…');
        (x + 1, text)
    } else {
        return None;
    };
    taken[start..start + text.width()].fill(true);
    Some((start, text))
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
        let theme = &cx.editor.theme;
        let text = theme.get("ui.text");
        surface.clear_with(area, theme.get("ui.background").patch(text));
        let title = format!(" {} ", self.params.nodes[self.centre].label);
        let block = Block::bordered().title(title);
        self.inner = block.inner(area);
        block.render(area, surface);
        if self.fit {
            self.fit = false;
            self.fit_view(0);
        }
        self.draw(surface, theme);

        let hovered = self
            .hovered()
            .map(|i| format!("{} · ", self.params.nodes[self.shown[i].0].label))
            .unwrap_or_default();
        let status = format!(" {hovered}depth {} ", self.depth);
        surface.set_stringn(
            area.x + 1,
            area.bottom().saturating_sub(1),
            &status,
            area.width.saturating_sub(2) as usize,
            text,
        );
        if let Some(info) = &mut self.info {
            info.render(area, surface, cx);
        }
    }

    fn id(&self) -> Option<&'static str> {
        Some(Self::ID)
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
    Depth,
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
            Self::Depth => "Toggle one and two links out",
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
    bind(&[&[key!('d')]], Action::Depth),
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
            })
            .collect();
        let params = ShowGraphParams {
            nodes,
            edges: edges.to_vec(),
            focus: 0,
        };
        Graph::new(params, "server".into(), GraphLabels::All)
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
        let positions = layout(&[0, 1, 2, 3], &edges);
        assert_eq!(positions[0], (0.0, 0.0));
        assert_eq!(positions, layout(&[0, 1, 2, 3], &edges));
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
        graph.zoom = 2.0;
        // Cells: 0 at (0, 0), 1 at (10, 1), 2 at (4, 0), 3 at (-6, 5).
        graph.shown = vec![
            (0, (0.0, 0.0)),
            (1, (10.0, 2.0)),
            (2, (4.0, 0.0)),
            (3, (-6.0, 10.0)),
        ];
        graph.fit = false;
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
        graph.step((1, 0));
        assert_eq!(graph.hovered(), None);
    }

    #[test]
    fn names_go_where_they_fit() {
        let mut row = [false; 20];
        row[0] = true;
        assert_eq!(place(&mut row, 0, "Functor"), Some((1, " Functor".into())));
        // The node at 12 has the first name to its left.
        row[12] = true;
        assert_eq!(place(&mut row, 12, "Monad"), Some((13, " Monad".into())));
        row[19] = true;
        assert_eq!(place(&mut row, 19, "Arrow"), None);
        let mut row = [false; 12];
        row[5] = true;
        assert_eq!(place(&mut row, 5, "Category"), Some((6, " Cate…".into())));
    }
}
