//! Smooth scrolling.
//!
//! Views, popups and lists keep their real scroll position; what is drawn glides towards it over
//! a fixed duration, fast at first and slowing down as it arrives. Only the drawing is animated,
//! so commands always act on the real state.

use std::time::Duration;

use helix_core::{
    char_idx_at_visual_offset,
    movement::{move_vertically_visual, Direction, Movement},
    visual_offset_from_anchor, Selection,
};
use tokio::time::Instant;

use crate::{
    editor::SmoothScrollConfig, graphics::Rect, view::ViewPosition, Document, DocumentId, Editor,
    View, ViewId,
};

/// Minimum time between two frames of an animation, about 120 frames per second.
const FRAME_INTERVAL: Duration = Duration::from_millis(8);

/// The timing shared by all smooth scrolling: a movement that takes `duration` whatever its
/// distance, and slows down towards its end.
#[derive(Debug, Clone, Copy)]
struct Transition {
    start: Instant,
    duration: Duration,
}

impl Transition {
    fn new(now: Instant, duration: Duration) -> Self {
        Self {
            // start a frame early, so the frame drawn right after a key press already moves
            start: now.checked_sub(FRAME_INTERVAL).unwrap_or(now),
            duration,
        }
    }

    fn end(&self) -> Instant {
        self.start + self.duration
    }

    fn is_finished(&self, now: Instant) -> bool {
        now >= self.end()
    }

    /// How much of the movement is done at `now`, from 0 to 1.
    fn progress(&self, now: Instant) -> f64 {
        let time = now
            .saturating_duration_since(self.start)
            .div_duration_f64(self.duration);
        ease_out(time.min(1.0))
    }

    /// How many of a movement's `total` steps are done at `now`.
    fn steps(&self, now: Instant, total: usize) -> usize {
        ((total as f64 * self.progress(now)).round() as usize).min(total)
    }

    /// When the step after `step` of a movement with `total` steps is due.
    fn next_step(&self, step: usize, total: usize) -> Option<Instant> {
        // `steps` rounds, so the next step is due once `total * progress` reaches `step + 0.5`
        (step < total).then(|| {
            let progress = (step as f64 + 0.5) / total as f64;
            self.start + self.duration.mul_f64(ease_out_inverse(progress))
        })
    }

    /// When the next frame is due, given when the next step of each moving part is due. `None`
    /// once all parts have arrived.
    fn next_frame(
        &self,
        now: Instant,
        next_steps: impl IntoIterator<Item = Option<Instant>>,
    ) -> Option<Instant> {
        let next_step = next_steps.into_iter().flatten().min()?;
        Some(next_step.max(now + FRAME_INTERVAL).min(self.end()))
    }
}

/// Ease-out cubic: fast at first, slowing down to a halt at the end.
fn ease_out(time: f64) -> f64 {
    1.0 - (1.0 - time).powi(3)
}

fn ease_out_inverse(progress: f64) -> f64 {
    1.0 - (1.0 - progress).cbrt()
}

fn steps_between(from: usize, to: usize) -> isize {
    to as isize - from as isize
}

/// `Transition::steps` for a signed distance.
fn signed_steps(transition: &Transition, now: Instant, total: isize) -> isize {
    total.signum() * transition.steps(now, total.unsigned_abs()) as isize
}

/// Moves `from` by `step` towards `to`.
fn step_towards(from: usize, to: usize, step: usize) -> usize {
    if to >= from {
        from + step
    } else {
        from - step
    }
}

/// The scroll offset of a popup or list, drawn gliding towards its real value.
#[derive(Debug, Default)]
pub struct SmoothOffset {
    /// The real offset and the visible height at the last frame.
    last: Option<(usize, u16)>,
    animation: Option<OffsetAnimation>,
}

#[derive(Debug)]
struct OffsetAnimation {
    transition: Transition,
    from: usize,
    to: usize,
    /// The offset drawn last.
    offset: usize,
}

impl OffsetAnimation {
    fn distance(&self) -> usize {
        self.from.abs_diff(self.to)
    }
}

impl SmoothOffset {
    /// Returns the offset to draw content scrolled to `offset` in `height` rows with, and
    /// schedules the redraw for the next frame.
    pub fn frame(&mut self, offset: usize, height: u16, editor: &mut Editor) -> usize {
        let now = Instant::now();
        let offset = self.update(offset, height, &editor.config().smooth_scroll, now);
        if let Some(next_frame) = self.next_frame(now) {
            editor.schedule_redraw(next_frame);
        }
        offset
    }

    /// Draws the next frame at the real offset right away, e.g. because the content was replaced.
    pub fn reset(&mut self) {
        *self = Self::default();
    }

    fn update(
        &mut self,
        offset: usize,
        height: u16,
        config: &SmoothScrollConfig,
        now: Instant,
    ) -> usize {
        let last = self.last.replace((offset, height));
        match last {
            Some((last_offset, last_height)) if config.is_enabled() && last_height == height => {
                if offset != last_offset {
                    let from = self
                        .animation
                        .as_ref()
                        .map_or(last_offset, |animation| animation.offset);
                    self.animation = (from.abs_diff(offset) > 1).then(|| OffsetAnimation {
                        transition: Transition::new(now, config.duration),
                        from,
                        to: offset,
                        offset: from,
                    });
                }
            }
            _ => self.animation = None,
        }

        let Some(animation) = &mut self.animation else {
            return offset;
        };
        let step = animation.transition.steps(now, animation.distance());
        if step == animation.distance() || animation.transition.is_finished(now) {
            self.animation = None;
            return offset;
        }
        animation.offset = step_towards(animation.from, animation.to, step);
        animation.offset
    }

    fn next_frame(&self, now: Instant) -> Option<Instant> {
        let animation = self.animation.as_ref()?;
        let step = animation.offset.abs_diff(animation.from);
        let next_step = animation.transition.next_step(step, animation.distance());
        animation.transition.next_frame(now, [next_step])
    }
}

/// A view's smooth scrolling: what the last frame showed, and the animation from there to the
/// view's real offset.
#[derive(Default)]
pub(crate) struct SmoothScroll {
    last: Option<Shown>,
    /// How `View::scroll` moved the selections since the last frame.
    hint: Option<ScrollHint>,
    animation: Option<Animation>,
}

impl Clone for SmoothScroll {
    /// A copied view (a new split) has not drawn anything yet.
    fn clone(&self) -> Self {
        Self::default()
    }
}

/// Everything besides the offset a frame depends on. When any of it changes, the view is drawn
/// at its real offset right away.
#[derive(Clone, Copy, PartialEq, Eq)]
struct FrameKey {
    doc: DocumentId,
    version: i32,
    area: Rect,
    soft_wrap: bool,
    /// Opening or closing folds changes the layout.
    folds: usize,
}

impl FrameKey {
    fn new(view: &View, doc: &Document) -> Self {
        let area = view.inner_area(doc);
        Self {
            doc: doc.id(),
            version: doc.version(),
            area,
            soft_wrap: doc.text_format(area.width, None).soft_wrap,
            folds: doc.fold_revision(view.id),
        }
    }
}

struct Shown {
    key: FrameKey,
    offset: ViewPosition,
}

struct ScrollHint {
    /// The selection on screen before the scroll.
    from: Selection,
    motion: SelectionMotion,
}

/// How a scroll moves the selections, so that the frames drawn in between move them alike.
#[derive(Clone, Copy)]
pub(crate) enum SelectionMotion {
    /// The selections move this many visual rows along with the view, upwards if negative.
    Synced { rows: isize, movement: Movement },
    /// The primary cursor stays put until the scrolloff margin pushes it along.
    Pushed {
        direction: Direction,
        movement: Movement,
    },
}

impl SelectionMotion {
    /// This motion following `previous`: synced motions add up.
    fn after(self, previous: Option<SelectionMotion>) -> Self {
        match (previous, self) {
            (Some(Self::Synced { rows: previous, .. }), Self::Synced { rows, movement }) => {
                Self::Synced {
                    rows: previous + rows,
                    movement,
                }
            }
            _ => self,
        }
    }
}

struct Animation {
    transition: Transition,
    key: FrameKey,
    from: ViewPosition,
    to: ViewPosition,
    path: Path,
    /// The offset drawn last and how many steps of `path` it covers.
    offset: ViewPosition,
    step: usize,
    selection: Option<SelectionAnimation>,
}

enum Path {
    /// Walk this many visual rows, upwards if negative.
    Rows(isize),
    /// Too far to lay out every row: cover `lines` document lines from the row `start_row`
    /// (upwards if negative), counting the lines of a folded row as one, then walk `rows` visual
    /// rows from `approach` to the target.
    Far {
        start_row: usize,
        lines: isize,
        approach: ViewPosition,
        rows: isize,
    },
}

struct SelectionAnimation {
    motion: SelectionMotion,
    /// The selection drawn when the animation started.
    from: Selection,
    /// The real selection the animation ends with. Once the real selection changes, it is drawn
    /// instead.
    target: Selection,
    /// The selection of the last frame and the synced rows it moved.
    stepped: Selection,
    moved: isize,
    /// `stepped` fit for drawing: overlapping ranges merged.
    drawn: Selection,
}

impl SmoothScroll {
    /// Records that `View::scroll` moves the selections by `motion`, `drawn` being the selection
    /// on screen.
    pub(crate) fn hint(
        &mut self,
        drawn: Selection,
        motion: SelectionMotion,
        doc: &Document,
        view: ViewId,
    ) {
        let hint = match self.hint.take() {
            // several scrolls since the last frame: the screen still shows the first one's start
            Some(pending) => ScrollHint {
                from: pending.from,
                motion: motion.after(Some(pending.motion)),
            },
            None => {
                let remaining = self
                    .current(doc, view)
                    .and_then(|animation| animation.selection.as_ref())
                    .map(SelectionAnimation::remaining);
                ScrollHint {
                    from: drawn,
                    motion: motion.after(remaining),
                }
            }
        };
        self.hint = Some(hint);
    }

    /// Advances to the frame drawn at `now` and returns when the next frame is due.
    pub(crate) fn update(&mut self, view: &View, doc: &Document, now: Instant) -> Option<Instant> {
        let config = doc.config.load();
        let hint = self.hint.take();
        if !config.smooth_scroll.is_enabled() {
            *self = Self::default();
            return None;
        }

        let key = FrameKey::new(view, doc);
        let offset = doc.view_offset(view.id);
        match self.last.replace(Shown { key, offset }) {
            Some(last) if last.key == key => {
                if offset != last.offset {
                    let from = self
                        .animation
                        .as_ref()
                        .map_or(last.offset, |animation| animation.offset);
                    let hint = hint.filter(|_| !config.smooth_scroll.hide_cursor);
                    self.animation = Animation::new(
                        view,
                        doc,
                        key,
                        from,
                        offset,
                        hint,
                        config.smooth_scroll.duration,
                        now,
                    );
                }
            }
            _ => self.animation = None,
        }

        let next_frame = self
            .animation
            .as_mut()?
            .advance(view, doc, config.scrolloff, now);
        if next_frame.is_none() {
            self.animation = None;
        }
        next_frame
    }

    /// The animation drawn for `doc` in the view, unless `doc` or its folds changed since the
    /// last frame.
    fn current(&self, doc: &Document, view: ViewId) -> Option<&Animation> {
        self.animation.as_ref().filter(|animation| {
            animation.key.doc == doc.id()
                && animation.key.version == doc.version()
                && animation.key.folds == doc.fold_revision(view)
        })
    }

    pub(crate) fn offset(&self, doc: &Document, view: ViewId) -> Option<ViewPosition> {
        self.current(doc, view).map(|animation| animation.offset)
    }

    pub(crate) fn selection(&self, doc: &Document, view: ViewId) -> Option<&Selection> {
        let selection = self.current(doc, view)?.selection.as_ref()?;
        Some(&selection.drawn)
    }

    pub(crate) fn is_animating(&self, doc: &Document, view: ViewId) -> bool {
        self.current(doc, view).is_some()
    }
}

impl Animation {
    #[allow(clippy::too_many_arguments)]
    fn new(
        view: &View,
        doc: &Document,
        key: FrameKey,
        from: ViewPosition,
        to: ViewPosition,
        hint: Option<ScrollHint>,
        duration: Duration,
        now: Instant,
    ) -> Option<Self> {
        let path = Path::new(view, doc, from, to);
        let columns = from.horizontal_offset.abs_diff(to.horizontal_offset);
        if path.len() <= 1 && columns <= 1 {
            return None;
        }
        let target = doc.selection(view.id).clone();
        Some(Self {
            transition: Transition::new(now, duration),
            key,
            from,
            to,
            path,
            offset: from,
            step: 0,
            selection: hint.map(|hint| SelectionAnimation::new(hint, target)),
        })
    }

    /// Moves to the frame drawn at `now` and returns when the next frame is due, or `None` once
    /// the real state is reached.
    fn advance(
        &mut self,
        view: &View,
        doc: &Document,
        scrolloff: usize,
        now: Instant,
    ) -> Option<Instant> {
        if self.transition.is_finished(now) {
            return None;
        }
        let columns = self
            .from
            .horizontal_offset
            .abs_diff(self.to.horizontal_offset);
        let column_step = self.transition.steps(now, columns);
        let horizontal_offset = step_towards(
            self.from.horizontal_offset,
            self.to.horizontal_offset,
            column_step,
        );

        let step = self.transition.steps(now, self.path.len());
        self.offset = ViewPosition {
            horizontal_offset,
            ..self
                .path
                .walk(view, doc, self.offset, self.step, step, horizontal_offset)
        };
        self.step = step;

        if self
            .selection
            .as_ref()
            .is_some_and(|selection| doc.selection(view.id) != &selection.target)
        {
            self.selection = None;
        }
        let view_rows = match self.path {
            Path::Rows(rows) => Some((rows.signum() * step as isize, rows)),
            Path::Far { .. } => None,
        };
        if let Some(selection) = &mut self.selection {
            selection.advance(
                view,
                doc,
                self.offset,
                view_rows,
                &self.transition,
                scrolloff,
                now,
            );
        }

        let selection_step = self
            .selection
            .as_ref()
            .and_then(|selection| selection.next_step(view_rows, &self.transition));
        self.transition.next_frame(
            now,
            [
                self.transition.next_step(step, self.path.len()),
                self.transition.next_step(column_step, columns),
                selection_step,
            ],
        )
    }
}

impl Path {
    fn new(view: &View, doc: &Document, from: ViewPosition, to: ViewPosition) -> Self {
        let text = doc.text().slice(..);
        let text_fmt = doc.text_format(view.inner_area(doc).width, None);
        let annotations = view.text_annotations_at(doc, None, to.horizontal_offset);
        // walk the last two screens row by row, which also bounds the cost of measuring
        let limit = (2 * view.inner_height()).max(2);

        let forward = (to.anchor, to.vertical_offset) >= (from.anchor, from.vertical_offset);
        let (top, bottom) = if forward { (from, to) } else { (to, from) };
        let distance = visual_offset_from_anchor(
            text,
            top.anchor,
            bottom.anchor,
            &text_fmt,
            &annotations,
            limit,
        );
        let rows = match distance {
            Ok((position, _)) => {
                (position.row + bottom.vertical_offset) as isize - top.vertical_offset as isize
            }
            Err(_) => limit as isize,
        };
        let rows = if forward { rows } else { -rows };
        if distance.is_ok() {
            return Self::Rows(rows);
        }

        let (anchor, vertical_offset) = char_idx_at_visual_offset(
            text,
            to.anchor,
            to.vertical_offset as isize - rows,
            0,
            &text_fmt,
            &annotations,
        );
        // closed folds join lines into one row, so count rows rather than lines
        let folds = doc.folds(view.id);
        let start_row = folds.row(text, text.char_to_line(from.anchor));
        Self::Far {
            start_row,
            lines: folds.row(text, text.char_to_line(anchor)) as isize - start_row as isize,
            approach: ViewPosition {
                anchor,
                vertical_offset,
                horizontal_offset: to.horizontal_offset,
            },
            rows,
        }
    }

    fn len(&self) -> usize {
        match *self {
            Self::Rows(rows) => rows.unsigned_abs(),
            Self::Far { lines, rows, .. } => lines.unsigned_abs() + rows.unsigned_abs(),
        }
    }

    /// Walks from `offset`, the frame at step `from_step`, to the frame at step `to_step`.
    fn walk(
        &self,
        view: &View,
        doc: &Document,
        offset: ViewPosition,
        from_step: usize,
        to_step: usize,
        horizontal_offset: usize,
    ) -> ViewPosition {
        let text = doc.text().slice(..);
        let text_fmt = doc.text_format(view.inner_area(doc).width, None);
        let annotations = view.text_annotations_at(doc, None, horizontal_offset);
        let walk_rows = |offset: ViewPosition, rows: isize| {
            let (anchor, vertical_offset) = char_idx_at_visual_offset(
                text,
                offset.anchor,
                offset.vertical_offset as isize + rows,
                0,
                &text_fmt,
                &annotations,
            );
            ViewPosition {
                anchor,
                vertical_offset,
                ..offset
            }
        };

        match *self {
            Self::Rows(rows) => {
                walk_rows(offset, rows.signum() * steps_between(from_step, to_step))
            }
            Self::Far {
                start_row,
                lines,
                approach,
                rows,
            } => {
                let bulk = lines.unsigned_abs();
                if to_step <= bulk {
                    // cover the bulk of the distance in whole document lines, which is cheap
                    let row = start_row as isize + lines.signum() * to_step as isize;
                    let line = doc.folds(view.id).row_start(text, row as usize);
                    let line_start = ViewPosition {
                        anchor: text.line_to_char(line),
                        vertical_offset: 0,
                        ..offset
                    };
                    walk_rows(line_start, 0)
                } else if from_step <= bulk {
                    walk_rows(approach, rows.signum() * steps_between(bulk, to_step))
                } else {
                    walk_rows(offset, rows.signum() * steps_between(from_step, to_step))
                }
            }
        }
    }
}

impl SelectionAnimation {
    fn new(hint: ScrollHint, target: Selection) -> Self {
        Self {
            motion: hint.motion,
            stepped: hint.from.clone(),
            drawn: hint.from.clone(),
            from: hint.from,
            target,
            moved: 0,
        }
    }

    /// The part of the motion that is not drawn yet.
    fn remaining(&self) -> SelectionMotion {
        match self.motion {
            SelectionMotion::Synced { rows, movement } => SelectionMotion::Synced {
                rows: rows - self.moved,
                movement,
            },
            motion => motion,
        }
    }

    /// Moves to the frame drawn at `now`, for which the view is at `offset` and, when walked row
    /// by row, has moved `view_rows.0` of its `view_rows.1` rows.
    #[allow(clippy::too_many_arguments)]
    fn advance(
        &mut self,
        view: &View,
        doc: &Document,
        offset: ViewPosition,
        view_rows: Option<(isize, isize)>,
        transition: &Transition,
        scrolloff: usize,
        now: Instant,
    ) {
        let text = doc.text().slice(..);
        match self.motion {
            SelectionMotion::Synced { rows, movement } => {
                // move in lockstep with the view, so the selections keep their place on screen,
                // plus whatever distance the view can't follow (at the start or end of the text)
                let moved = match view_rows {
                    Some((view_moved, view_total)) => {
                        view_moved + signed_steps(transition, now, rows - view_total)
                    }
                    None => signed_steps(transition, now, rows),
                };
                let delta = moved - self.moved;
                if delta == 0 {
                    return;
                }
                let direction = if delta > 0 {
                    Direction::Forward
                } else {
                    Direction::Backward
                };
                let text_fmt = doc.text_format(view.inner_area(doc).width, None);
                // `move_vertically_visual` clears the line annotations
                let mut annotations = view.text_annotations_at(doc, None, offset.horizontal_offset);
                self.stepped = self.stepped.clone().transform(|range| {
                    move_vertically_visual(
                        text,
                        range,
                        direction,
                        delta.unsigned_abs(),
                        movement,
                        &text_fmt,
                        &mut annotations,
                    )
                });
                self.moved = moved;
                self.drawn = self.stepped.clone().ensure_invariants(text);
            }
            SelectionMotion::Pushed {
                direction,
                movement,
            } => {
                let primary = self.from.primary();
                let pushed = view
                    .push_cursor_into_view(doc, offset, primary, direction, movement, scrolloff)
                    .unwrap_or(primary);
                self.drawn = self
                    .from
                    .clone()
                    .replace(self.from.primary_index(), pushed)
                    .ensure_invariants(text);
            }
        }
    }

    /// When the selection moves next on its own, apart from following the view.
    fn next_step(
        &self,
        view_rows: Option<(isize, isize)>,
        transition: &Transition,
    ) -> Option<Instant> {
        match self.motion {
            SelectionMotion::Synced { rows, .. } => {
                let (drift, drifted) = match view_rows {
                    Some((view_moved, view_total)) => (rows - view_total, self.moved - view_moved),
                    None => (rows, self.moved),
                };
                transition.next_step(drifted.unsigned_abs(), drift.unsigned_abs())
            }
            SelectionMotion::Pushed { .. } => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use arc_swap::ArcSwap;
    use helix_core::{fold::Fold, syntax, Rope, Transaction};

    use super::*;
    use crate::editor::{Config, GutterConfig};

    const DURATION: Duration = Duration::from_millis(100);
    const SCROLLOFF: usize = 5;

    fn smooth_scroll(enable: bool, hide_cursor: bool) -> SmoothScrollConfig {
        SmoothScrollConfig {
            enable,
            duration: DURATION,
            hide_cursor,
        }
    }

    /// A document of `lines` numbered lines, shown in a view with 20 rows of text.
    fn setup(lines: usize, smooth_scroll: SmoothScrollConfig) -> (View, Document) {
        let text: String = (0..lines).map(|line| format!("line {line}\n")).collect();
        let config = Config {
            smooth_scroll,
            scrolloff: SCROLLOFF,
            ..Default::default()
        };
        let mut doc = Document::from(
            Rope::from(text),
            None,
            Arc::new(ArcSwap::new(Arc::new(config))),
            Arc::new(ArcSwap::from_pointee(syntax::Loader::default())),
        );
        let mut view = View::new(doc.id(), GutterConfig::default());
        view.area = Rect::new(0, 0, 80, 21);
        doc.ensure_view_init(view.id);
        (view, doc)
    }

    fn scroll_to_line(view: &View, doc: &mut Document, line: usize) {
        let offset = ViewPosition {
            anchor: doc.text().line_to_char(line),
            ..doc.view_offset(view.id)
        };
        doc.set_view_offset(view.id, offset);
    }

    fn top_line(view: &View, doc: &Document) -> usize {
        doc.text().char_to_line(view.render_offset(doc).anchor)
    }

    fn cursor_line(view: &View, doc: &Document) -> usize {
        let text = doc.text().slice(..);
        text.char_to_line(view.render_selection(doc).primary().cursor(text))
    }

    /// Draws frames until the animation ends, returning `frame(view, doc)` of each frame.
    fn frames<T>(
        view: &mut View,
        doc: &Document,
        now: Instant,
        frame: impl Fn(&View, &Document) -> T,
    ) -> Vec<T> {
        let mut next_frame = view.update_smooth_scroll(doc, now);
        let mut frames = vec![frame(view, doc)];
        while let Some(deadline) = next_frame {
            next_frame = view.update_smooth_scroll(doc, deadline);
            frames.push(frame(view, doc));
        }
        frames
    }

    #[test]
    fn transition_slows_down_towards_the_end() {
        let now = Instant::now();
        let transition = Transition {
            start: now,
            duration: DURATION,
        };
        let total = 100;
        let steps: Vec<_> = (0..=10)
            .map(|slice| transition.steps(now + DURATION * slice / 10, total))
            .collect();
        assert_eq!((steps[0], steps[10]), (0, total));

        let covered: Vec<_> = steps.windows(2).map(|steps| steps[1] - steps[0]).collect();
        assert!(
            covered.windows(2).all(|covered| covered[1] <= covered[0]),
            "{covered:?}"
        );
        assert!(covered[9] < covered[0], "{covered:?}");

        for step in 0..total {
            let due = transition.next_step(step, total).unwrap();
            let margin = Duration::from_micros(50);
            assert_eq!(transition.steps(due - margin, total), step);
            assert_eq!(transition.steps(due + margin, total), step + 1);
        }
        assert_eq!(transition.next_step(total, total), None);
    }

    #[test]
    fn smooth_offset_glides_to_a_new_offset() {
        let config = smooth_scroll(true, false);
        let now = Instant::now();
        let mut offset = SmoothOffset::default();
        // nothing to glide from on the first frame, and single rows snap
        assert_eq!(offset.update(0, 20, &config, now), 0);
        assert_eq!(offset.update(1, 20, &config, now), 1);
        assert_eq!(offset.next_frame(now), None);

        let drawn: Vec<_> = [0, 10, 20, 40, 60, 80, 99, 100]
            .into_iter()
            .map(|ms| offset.update(31, 20, &config, now + Duration::from_millis(ms)))
            .collect();
        assert!(drawn[0] > 1 && drawn[0] < 31, "{drawn:?}");
        assert!(
            drawn.windows(2).all(|drawn| drawn[0] <= drawn[1]),
            "{drawn:?}"
        );
        assert_eq!(drawn.last(), Some(&31));
        assert_eq!(offset.next_frame(now + DURATION), None);
    }

    #[test]
    fn smooth_offset_snaps() {
        let now = Instant::now();
        let config = smooth_scroll(true, false);
        let mut offset = SmoothOffset::default();
        offset.update(0, 20, &config, now);
        // the visible height changed
        assert_eq!(offset.update(30, 10, &config, now), 30);
        // the content was replaced
        offset.reset();
        assert_eq!(offset.update(0, 10, &config, now), 0);
        // disabled
        assert_eq!(offset.update(30, 10, &smooth_scroll(false, false), now), 30);
    }

    #[test]
    fn view_glides_to_its_new_offset() {
        for target in [12, 300] {
            let (mut view, mut doc) = setup(500, smooth_scroll(true, false));
            let now = Instant::now();
            assert_eq!(view.update_smooth_scroll(&doc, now), None);

            scroll_to_line(&view, &mut doc, target);
            let lines = frames(&mut view, &doc, now, top_line);
            assert!(lines.len() > 2, "{lines:?}");
            assert!(lines[0] > 0, "the first frame moves: {lines:?}");
            assert!(
                lines.windows(2).all(|lines| lines[0] <= lines[1]),
                "{lines:?}"
            );
            assert_eq!(lines.last(), Some(&target));
            assert_eq!(view.render_offset(&doc), doc.view_offset(view.id));

            // and back, gliding the other way
            scroll_to_line(&view, &mut doc, 0);
            let lines = frames(&mut view, &doc, now + DURATION, top_line);
            assert!(
                lines.windows(2).all(|lines| lines[0] >= lines[1]),
                "{lines:?}"
            );
            assert_eq!(lines.last(), Some(&0));
        }
    }

    #[test]
    fn view_glides_horizontally() {
        let (mut view, mut doc) = setup(10, smooth_scroll(true, false));
        let now = Instant::now();
        view.update_smooth_scroll(&doc, now);

        let offset = ViewPosition {
            horizontal_offset: 50,
            ..doc.view_offset(view.id)
        };
        doc.set_view_offset(view.id, offset);
        let columns = frames(&mut view, &doc, now, |view, doc| {
            view.render_offset(doc).horizontal_offset
        });
        assert!(columns.windows(2).all(|columns| columns[0] <= columns[1]));
        assert!(columns[0] > 0 && columns[0] < 50, "{columns:?}");
        assert_eq!(columns.last(), Some(&50));
    }

    #[test]
    fn view_snaps() {
        let now = Instant::now();

        // a single row
        let (mut view, mut doc) = setup(500, smooth_scroll(true, false));
        view.update_smooth_scroll(&doc, now);
        scroll_to_line(&view, &mut doc, 1);
        assert_eq!(view.update_smooth_scroll(&doc, now), None);
        assert_eq!(top_line(&view, &doc), 1);

        // the document was edited
        scroll_to_line(&view, &mut doc, 100);
        let transaction = Transaction::insert(doc.text(), &Selection::point(0), "x".into());
        doc.apply(&transaction, view.id);
        assert_eq!(view.update_smooth_scroll(&doc, now), None);
        assert_eq!(top_line(&view, &doc), 100);

        // the view was resized
        scroll_to_line(&view, &mut doc, 0);
        view.area = Rect::new(0, 0, 80, 30);
        assert_eq!(view.update_smooth_scroll(&doc, now), None);
        assert_eq!(top_line(&view, &doc), 0);

        // smooth scrolling is disabled
        let (mut view, mut doc) = setup(500, smooth_scroll(false, false));
        view.update_smooth_scroll(&doc, now);
        scroll_to_line(&view, &mut doc, 100);
        assert_eq!(view.update_smooth_scroll(&doc, now), None);
        assert_eq!(top_line(&view, &doc), 100);
    }

    #[test]
    fn synced_scroll_keeps_the_cursor_on_its_screen_row() {
        let (mut view, mut doc) = setup(500, smooth_scroll(true, false));
        let now = Instant::now();
        doc.set_selection(view.id, Selection::point(doc.text().line_to_char(8)));
        view.update_smooth_scroll(&doc, now);

        view.scroll(
            &mut doc,
            10,
            Direction::Forward,
            true,
            Movement::Move,
            SCROLLOFF,
        );
        let rows = frames(&mut view, &doc, now, |view, doc| {
            cursor_line(view, doc) - top_line(view, doc)
        });
        assert!(rows.len() > 2, "{rows:?}");
        assert!(rows.iter().all(|&row| row == 8), "{rows:?}");
        assert_eq!(view.render_selection(&doc), doc.selection(view.id));
        assert_eq!(cursor_line(&view, &doc), 18);
    }

    #[test]
    fn synced_scroll_drifts_smoothly_where_the_view_cannot_follow() {
        let (mut view, mut doc) = setup(500, smooth_scroll(true, false));
        let now = Instant::now();
        view.update_smooth_scroll(&doc, now);

        // `<C-d>` at the start of the document: keeping the cursor out of the scrolloff margin
        // afterwards moves the view back, so the cursor ends up lower on screen
        view.scroll(
            &mut doc,
            10,
            Direction::Forward,
            true,
            Movement::Move,
            SCROLLOFF,
        );
        view.ensure_cursor_in_view(&mut doc, SCROLLOFF);
        let rows = frames(&mut view, &doc, now, |view, doc| {
            cursor_line(view, doc) - top_line(view, doc)
        });
        assert!(rows.windows(2).all(|rows| rows[0] <= rows[1]), "{rows:?}");
        assert_eq!(rows.last(), Some(&SCROLLOFF));
        assert_eq!(view.render_selection(&doc), doc.selection(view.id));
    }

    #[test]
    fn pushed_scroll_keeps_the_cursor_within_the_margin() {
        let (mut view, mut doc) = setup(500, smooth_scroll(true, false));
        let now = Instant::now();
        doc.set_selection(view.id, Selection::point(doc.text().line_to_char(8)));
        view.update_smooth_scroll(&doc, now);

        view.scroll(
            &mut doc,
            20,
            Direction::Forward,
            false,
            Movement::Move,
            SCROLLOFF,
        );
        let frames = frames(&mut view, &doc, now, |view, doc| {
            (top_line(view, doc), cursor_line(view, doc))
        });
        for &(top, cursor) in &frames {
            // the cursor keeps its line until the margin reaches it, then rides the margin
            assert_eq!(cursor, (top + SCROLLOFF).max(8), "{frames:?}");
        }
        assert_eq!(frames.last(), Some(&(20, 25)));
        assert_eq!(view.render_selection(&doc), doc.selection(view.id));
    }

    #[test]
    fn real_selection_change_is_drawn_at_once() {
        let (mut view, mut doc) = setup(500, smooth_scroll(true, false));
        let now = Instant::now();
        view.update_smooth_scroll(&doc, now);
        view.scroll(
            &mut doc,
            10,
            Direction::Forward,
            true,
            Movement::Move,
            SCROLLOFF,
        );
        view.update_smooth_scroll(&doc, now);
        assert_ne!(view.render_selection(&doc), doc.selection(view.id));

        doc.set_selection(view.id, Selection::point(doc.text().line_to_char(15)));
        assert!(view.update_smooth_scroll(&doc, now).is_some());
        assert_eq!(view.render_selection(&doc), doc.selection(view.id));
    }

    #[test]
    fn stale_frames_are_not_drawn() {
        let (mut view, mut doc) = setup(500, smooth_scroll(true, true));
        let now = Instant::now();
        view.update_smooth_scroll(&doc, now);
        scroll_to_line(&view, &mut doc, 100);
        view.update_smooth_scroll(&doc, now);
        assert_ne!(view.render_offset(&doc), doc.view_offset(view.id));
        assert!(view.hides_cursor(&doc));

        // a copy of the view (a new split) has not drawn anything yet
        let split = view.clone();
        assert_eq!(split.render_offset(&doc), doc.view_offset(view.id));

        // an edit before the next frame
        let transaction = Transaction::insert(doc.text(), &Selection::point(0), "x".into());
        doc.apply(&transaction, view.id);
        assert_eq!(view.render_offset(&doc), doc.view_offset(view.id));
        assert!(!view.hides_cursor(&doc));
    }

    /// Folds the lines after `header` up to and including the start of `last`.
    fn fold_lines(view: &View, doc: &mut Document, header: usize, last: usize) {
        let text = doc.text().slice(..);
        let region = text.line_to_char(header)..text.line_to_char(last) + 1;
        let mut folds = doc.folds(view.id).clone();
        folds.close(Fold::from_region(text, region, None));
        doc.set_folds(view.id, folds);
    }

    #[test]
    fn view_snaps_when_folds_change() {
        let (mut view, mut doc) = setup(500, smooth_scroll(true, false));
        let now = Instant::now();
        view.update_smooth_scroll(&doc, now);
        scroll_to_line(&view, &mut doc, 300);
        view.update_smooth_scroll(&doc, now);
        assert_ne!(view.render_offset(&doc), doc.view_offset(view.id));

        // a fold changes the layout under the animation, so its frames are stale
        fold_lines(&view, &mut doc, 10, 20);
        assert_eq!(view.render_offset(&doc), doc.view_offset(view.id));
        assert_eq!(view.update_smooth_scroll(&doc, now), None);
        assert_eq!(top_line(&view, &doc), 300);
    }

    #[test]
    fn view_glides_across_folds_row_by_row() {
        let (mut view, mut doc) = setup(2000, smooth_scroll(true, false));
        fold_lines(&view, &mut doc, 0, 1900);
        let now = Instant::now();
        view.update_smooth_scroll(&doc, now);

        scroll_to_line(&view, &mut doc, 1960);
        let rows = frames(&mut view, &doc, now, |view, doc| {
            doc.folds(view.id)
                .row(doc.text().slice(..), top_line(view, doc))
        });
        // no frame lands inside the fold and every frame moves, rather than stalling on the
        // lines the fold hides
        assert!(rows.windows(2).all(|rows| rows[0] < rows[1]), "{rows:?}");
        assert_eq!(rows.last(), Some(&60));
        assert_eq!(top_line(&view, &doc), 1960);
    }
}
