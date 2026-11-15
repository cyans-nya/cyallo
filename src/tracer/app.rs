//! Viewer state and key handling. Nothing here touches the terminal, so the
//! navigation rules are unit-tested directly.
use super::timeline::{Kind, Trace};
use crossterm::event::KeyCode;

/// Which events the viewer shows.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Filter {
    All,
    /// Compile, install and discard activity, plus failures.
    Lifecycle,
    /// VM entries, OSR and inline decisions.
    Transitions,
}

impl Filter {
    pub(crate) fn next(self) -> Self {
        match self {
            Self::All => Self::Lifecycle,
            Self::Lifecycle => Self::Transitions,
            Self::Transitions => Self::All,
        }
    }

    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::All => "all",
            Self::Lifecycle => "compile/install",
            Self::Transitions => "entries/OSR/inline",
        }
    }

    pub(crate) fn allows(self, kind: Kind) -> bool {
        match self {
            Self::All => true,
            Self::Lifecycle => kind.is_lifecycle(),
            Self::Transitions => kind.is_transition(),
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Action {
    Continue,
    Quit,
}

const MIN_SPAN_FRACTION: f64 = 1.0 / 5_000.0;
/// Routines are drawn two lines high; this is the number of routine rows a
/// given body height can hold.
pub(crate) const LINES_PER_LANE: u16 = 2;

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct App {
    total: f64,
    /// Start and width of the visible window, in milliseconds.
    pub start: f64,
    pub span: f64,
    pub cursor: f64,
    /// Index into the lane list.
    pub selected: usize,
    /// First lane shown; keeps `selected` on screen.
    pub scroll: usize,
    pub filter: Filter,
    pub help: bool,
}

impl App {
    pub(crate) fn new(total_ms: f64) -> Self {
        let total = total_ms.max(1e-6);
        Self {
            total,
            start: 0.0,
            span: total,
            cursor: 0.0,
            selected: 0,
            scroll: 0,
            filter: Filter::All,
            help: false,
        }
    }

    /// Handle one key press. `lanes` is the number of routine lanes shown.
    pub(crate) fn key(&mut self, code: KeyCode, trace: &Trace, lanes: usize) -> Action {
        if self.help {
            match code {
                KeyCode::Char('q') => return Action::Quit,
                _ => {
                    self.help = false;
                    return Action::Continue;
                }
            }
        }
        let step = self.span / 40.0;
        match code {
            KeyCode::Char('q') | KeyCode::Esc => return Action::Quit,
            KeyCode::Char('?') => self.help = true,
            KeyCode::Left | KeyCode::Char('h') => self.move_cursor(-step),
            KeyCode::Right | KeyCode::Char('l') => self.move_cursor(step),
            KeyCode::Char('[') => self.pan(-0.5),
            KeyCode::Char(']') => self.pan(0.5),
            KeyCode::Char('+' | '=') => self.zoom(0.5),
            KeyCode::Char('-' | '_') => self.zoom(2.0),
            KeyCode::Char('0') => self.fit(),
            KeyCode::Home => self.set_cursor(0.0),
            KeyCode::End => self.set_cursor(self.total),
            KeyCode::Char('n') => self.jump(trace, true),
            KeyCode::Char('p') => self.jump(trace, false),
            KeyCode::Up | KeyCode::Char('k') => self.select(-1, lanes),
            KeyCode::Down | KeyCode::Char('j') => self.select(1, lanes),
            KeyCode::Char('f') => self.filter = self.filter.next(),
            _ => {}
        }
        Action::Continue
    }

    /// Move the cursor, keeping the window around it.
    pub(crate) fn move_cursor(&mut self, delta: f64) {
        self.set_cursor(self.cursor + delta);
    }

    pub(crate) fn set_cursor(&mut self, at: f64) {
        self.cursor = at.clamp(0.0, self.total);
        self.follow();
    }

    /// Shift the window by `fraction` of its width, without moving the cursor.
    pub(crate) fn pan(&mut self, fraction: f64) {
        self.start += fraction * self.span;
        self.clamp_window();
    }

    /// Zoom by `factor` (below one zooms in), keeping the cursor in place on
    /// screen when possible.
    pub(crate) fn zoom(&mut self, factor: f64) {
        let min_span = (self.total * MIN_SPAN_FRACTION).max(1e-6);
        let anchor = self.cursor;
        let offset = if self.span > 0.0 {
            (anchor - self.start) / self.span
        } else {
            0.5
        };
        self.span = (self.span * factor).clamp(min_span.min(self.total), self.total);
        self.start = anchor - offset * self.span;
        self.clamp_window();
        self.follow();
    }

    pub(crate) fn fit(&mut self) {
        self.span = self.total;
        self.start = 0.0;
    }

    /// Keep the cursor inside the window.
    fn follow(&mut self) {
        if self.cursor < self.start || self.cursor > self.start + self.span {
            self.start = self.cursor - self.span / 2.0;
        }
        self.clamp_window();
    }

    fn clamp_window(&mut self) {
        let max_start = (self.total - self.span).max(0.0);
        self.start = self.start.clamp(0.0, max_start);
    }

    /// Move the cursor to the next or previous visible event.
    pub(crate) fn jump(&mut self, trace: &Trace, forward: bool) {
        let target = trace
            .events
            .iter()
            .filter(|event| self.filter.allows(event.kind))
            .map(|event| event.at_ms)
            .filter(|at| {
                if forward {
                    *at > self.cursor + 1e-9
                } else {
                    *at < self.cursor - 1e-9
                }
            });
        let next = if forward {
            target.min_by(f64::total_cmp)
        } else {
            target.max_by(f64::total_cmp)
        };
        if let Some(at) = next {
            self.set_cursor(at);
        }
    }

    fn select(&mut self, delta: isize, lanes: usize) {
        if lanes == 0 {
            return;
        }
        let last = lanes - 1;
        self.selected = if delta < 0 {
            self.selected.saturating_sub(1)
        } else {
            (self.selected + 1).min(last)
        };
    }

    /// Adjust the scroll so the selected lane is visible in `rows` lanes.
    pub(crate) fn fit_lanes(&mut self, rows: usize, lanes: usize) {
        if lanes == 0 || rows == 0 {
            self.scroll = 0;
            return;
        }
        self.selected = self.selected.min(lanes - 1);
        if self.selected < self.scroll {
            self.scroll = self.selected;
        } else if self.selected >= self.scroll + rows {
            self.scroll = self.selected + 1 - rows;
        }
        self.scroll = self.scroll.min(lanes.saturating_sub(rows));
    }
}

#[cfg(test)]
mod tests {
    use super::super::timeline::{Trace, fixtures::SAMPLE};
    use super::*;

    fn sample() -> (Trace, App) {
        let trace = Trace::parse(SAMPLE).unwrap();
        let app = App::new(trace.span_ms);
        (trace, app)
    }

    #[test]
    fn jumps_between_visible_events() {
        let (trace, mut app) = sample();
        app.jump(&trace, true);
        assert_eq!(app.cursor, 1.0);
        app.jump(&trace, true);
        assert_eq!(app.cursor, 1.2);
        app.set_cursor(6.0);
        app.jump(&trace, false);
        assert_eq!(app.cursor, 3.0);
        // Filtering skips lifecycle events when only transitions are shown.
        app.filter = Filter::Transitions;
        app.set_cursor(0.0);
        app.jump(&trace, true);
        assert_eq!(app.cursor, 6.5);
    }

    #[test]
    fn zoom_keeps_the_cursor_on_screen_and_respects_bounds() {
        let (_, mut app) = sample();
        app.set_cursor(5.0);
        app.zoom(0.5);
        assert!(app.span <= 5.0 + 1e-9);
        assert!(app.cursor >= app.start && app.cursor <= app.start + app.span);
        for _ in 0..40 {
            app.zoom(0.5);
        }
        assert!(app.span >= app.total * MIN_SPAN_FRACTION * 0.99);
        for _ in 0..40 {
            app.zoom(2.0);
        }
        assert!((app.span - app.total).abs() < 1e-9);
        assert_eq!(app.start, 0.0);
    }

    #[test]
    fn pan_and_cursor_stay_inside_the_trace() {
        let (_, mut app) = sample();
        app.zoom(0.25);
        app.pan(-10.0);
        assert_eq!(app.start, 0.0);
        app.pan(10.0);
        assert!((app.start + app.span - app.total).abs() < 1e-9);
        app.set_cursor(1e9);
        assert_eq!(app.cursor, app.total);
        app.set_cursor(-1.0);
        assert_eq!(app.cursor, 0.0);
    }

    #[test]
    fn lane_selection_scrolls_to_keep_the_selected_lane_visible() {
        let (_, mut app) = sample();
        for _ in 0..5 {
            app.select(1, 8);
        }
        assert_eq!(app.selected, 5);
        app.fit_lanes(3, 8);
        assert_eq!(app.scroll, 3);
        app.select(-1, 8);
        app.select(-1, 8);
        app.select(-1, 8);
        app.fit_lanes(3, 8);
        assert_eq!(app.scroll, 2);
        app.fit_lanes(10, 8);
        assert_eq!(app.scroll, 0);
    }

    #[test]
    fn keys_map_to_actions() {
        let (trace, mut app) = sample();
        assert_eq!(app.key(KeyCode::Char('q'), &trace, 2), Action::Quit);
        assert_eq!(app.key(KeyCode::Char('?'), &trace, 2), Action::Continue);
        assert!(app.help);
        // Any key other than q closes the help overlay without acting on it.
        assert_eq!(app.key(KeyCode::Char('n'), &trace, 2), Action::Continue);
        assert!(!app.help);
        assert_eq!(app.cursor, 0.0);
        app.key(KeyCode::Char('f'), &trace, 2);
        assert_eq!(app.filter, Filter::Lifecycle);
    }
}
