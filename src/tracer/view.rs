//! Terminal rendering for the trace viewer.
//!
//! Each routine is a lane of two lines. The first line is its installed tier
//! over time (a band) with markers on top: installs, OSR entries, failures and
//! inline decisions. The second line shows compile work: solid bars while a
//! compiler worker runs, dotted bars while a request waits for one.
use super::app::{App, Filter, LINES_PER_LANE};
use super::timeline::{Kind, Lane, Level, Marker, MarkerKind, Trace};
use ratatui::{
    Frame,
    layout::{Constraint, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span as TextSpan},
    widgets::{Block, Clear, Paragraph},
};

const LABEL_WIDTH: u16 = 22;
const DETAIL_LINES: u16 = 8;
const RULER_EVERY: usize = 16;

pub(crate) fn level_style(level: Level) -> Style {
    match level {
        Level::Interpreted => Style::new().fg(Color::DarkGray),
        Level::Winch => Style::new().fg(Color::Yellow),
        Level::Cranelift => Style::new().fg(Color::Cyan),
        Level::Speed => Style::new()
            .fg(Color::LightGreen)
            .add_modifier(Modifier::BOLD),
    }
}

fn level_glyph(level: Level) -> char {
    match level {
        Level::Interpreted => '·',
        Level::Winch => '▒',
        Level::Cranelift | Level::Speed => '█',
    }
}

fn kind_style(kind: Kind) -> Style {
    match kind {
        Kind::Install | Kind::CompileEnd => Style::new().fg(Color::Green),
        Kind::Osr => Style::new().fg(Color::Magenta).add_modifier(Modifier::BOLD),
        Kind::Failed => Style::new().fg(Color::Red).add_modifier(Modifier::BOLD),
        Kind::Request | Kind::Admit => Style::new().fg(Color::Cyan),
        Kind::CompileStart | Kind::Discard | Kind::Supersede => Style::new().fg(Color::DarkGray),
        Kind::Enter | Kind::Handoff => Style::new().fg(Color::Gray),
    }
}

fn marker_glyph(kind: MarkerKind) -> (char, Style) {
    match kind {
        MarkerKind::Failed => (
            '✖',
            Style::new().fg(Color::Red).add_modifier(Modifier::BOLD),
        ),
        MarkerKind::Install(level) => ('▲', level_style(level).add_modifier(Modifier::BOLD)),
        MarkerKind::Osr => (
            '◆',
            Style::new().fg(Color::Magenta).add_modifier(Modifier::BOLD),
        ),
        MarkerKind::Inline => ('◇', Style::new().fg(Color::LightBlue)),
    }
}

/// Markers take precedence over the band when several share a column: a
/// failure is more important than an install, which beats OSR and inlining.
fn marker_rank(kind: MarkerKind) -> u8 {
    match kind {
        MarkerKind::Failed => 3,
        MarkerKind::Install(_) => 2,
        MarkerKind::Osr => 1,
        MarkerKind::Inline => 0,
    }
}

fn marker_shown(filter: Filter, kind: MarkerKind) -> bool {
    matches!(
        (filter, kind),
        (_, MarkerKind::Failed)
            | (Filter::All, _)
            | (Filter::Lifecycle, MarkerKind::Install(_))
            | (Filter::Transitions, MarkerKind::Osr | MarkerKind::Inline)
    )
}

/// The glyph for one column of a band line.
fn band_cell(lane: &Lane, filter: Filter, t0: f64, t1: f64) -> (char, Style) {
    let level = lane.level_at((t0 + t1) / 2.0);
    let mut glyph = level_glyph(level);
    let mut style = level_style(level);
    let first = lane.markers.partition_point(|marker| marker.at < t0);
    let best: Option<&Marker> = lane.markers[first..]
        .iter()
        .take_while(|marker| marker.at < t1)
        .filter(|marker| marker_shown(filter, marker.kind))
        .max_by_key(|marker| marker_rank(marker.kind));
    if let Some(marker) = best {
        (glyph, style) = marker_glyph(marker.kind);
    }
    (glyph, style)
}

/// The glyph for one column of a compile line.
fn compile_cell(lane: &Lane, filter: Filter, t0: f64, t1: f64) -> (char, Style) {
    if filter == Filter::Transitions {
        return (' ', Style::new());
    }
    let busy = lane
        .compiles
        .iter()
        .rfind(|span| span.start < t1 && span.end > t0);
    if let Some(span) = busy {
        return ('━', level_style(span.level));
    }
    if lane
        .queued
        .iter()
        .any(|span| span.start < t1 && span.end > t0)
    {
        return ('┄', Style::new().fg(Color::DarkGray));
    }
    (' ', Style::new())
}

/// Groups consecutive cells with the same style into one span.
struct LineBuilder {
    spans: Vec<TextSpan<'static>>,
    style: Option<Style>,
    text: String,
}

impl LineBuilder {
    fn new() -> Self {
        Self {
            spans: Vec::new(),
            style: None,
            text: String::new(),
        }
    }

    fn push(&mut self, glyph: char, style: Style) {
        if self.style != Some(style) {
            self.flush();
            self.style = Some(style);
        }
        self.text.push(glyph);
    }

    fn flush(&mut self) {
        if let Some(style) = self.style.take() {
            self.spans
                .push(TextSpan::styled(std::mem::take(&mut self.text), style));
        }
    }

    fn finish(mut self) -> Line<'static> {
        self.flush();
        Line::from(self.spans)
    }
}

fn truncate(text: &str, width: usize) -> String {
    if text.chars().count() <= width {
        text.to_owned()
    } else {
        let kept: String = text.chars().take(width.saturating_sub(1)).collect();
        format!("{kept}…")
    }
}

/// Column the cursor falls in, if it is inside the window.
fn cursor_column(app: &App, width: usize) -> Option<usize> {
    let per_cell = app.span / width.max(1) as f64;
    let column = ((app.cursor - app.start) / per_cell).floor();
    (column >= 0.0 && (column as usize) < width).then_some(column as usize)
}

fn title_line(app: &App, trace: &Trace, file: &str) -> Line<'static> {
    let text = format!(
        " cyallo trace · {file} · {} {} · wall {:.3} ms · view {:.3}–{:.3} ms · cursor {:.3} ms · events: {} ",
        trace.engine,
        trace.status,
        trace.wall_ms,
        app.start,
        app.start + app.span,
        app.cursor,
        app.filter.label(),
    );
    Line::from(TextSpan::styled(
        text,
        Style::new().add_modifier(Modifier::REVERSED),
    ))
}

fn ruler(app: &App, width: usize) -> Line<'static> {
    let per_cell = app.span / width.max(1) as f64;
    let mut cells = vec![' '; width];
    let mut column = 1;
    while column < width {
        let label = format!("{:.2}ms", app.start + (column - 1) as f64 * per_cell);
        for (offset, glyph) in label.chars().enumerate() {
            if column + offset < width && offset < RULER_EVERY - 1 {
                cells[column + offset] = glyph;
            }
        }
        column += RULER_EVERY;
    }
    let mut builder = LineBuilder::new();
    let cursor = cursor_column(app, width);
    for (index, glyph) in cells.into_iter().enumerate() {
        if Some(index) == cursor {
            builder.push('▼', Style::new().fg(Color::Yellow));
        } else {
            builder.push(glyph, Style::new().fg(Color::DarkGray));
        }
    }
    builder.finish()
}

/// The left column: routine name and the level at the cursor. Each lane
/// is `lane_height` lines, as on the timeline.
fn label_lines(app: &App, shown: &[&Lane], width: u16, lane_height: usize) -> Vec<Line<'static>> {
    let mut lines = vec![Line::from("")];
    let name_width = width.saturating_sub(3) as usize;
    for (slot, lane) in shown.iter().enumerate() {
        let selected = app.scroll + slot == app.selected;
        let marker = if selected { "▸ " } else { "  " };
        let name_style = if selected {
            Style::new().add_modifier(Modifier::BOLD)
        } else {
            Style::new()
        };
        lines.push(Line::from(TextSpan::styled(
            format!("{marker}{}", truncate(&lane.name, name_width)),
            name_style,
        )));
        let level = lane.level_at(app.cursor);
        lines.push(Line::from(TextSpan::styled(
            format!("    now: {}", level.name()),
            level_style(level),
        )));
        for _ in 2..lane_height {
            lines.push(Line::from(""));
        }
    }
    lines
}

/// The timeline: a ruler, then `lane_height` lines per lane. The tier band
/// fills all but the last line; the last line carries compile work.
fn timeline_lines(
    app: &App,
    shown: &[&Lane],
    width: usize,
    lane_height: usize,
) -> Vec<Line<'static>> {
    let mut lines = vec![ruler(app, width)];
    let per_cell = app.span / width.max(1) as f64;
    let cursor = cursor_column(app, width);
    for lane in shown {
        let mut band = LineBuilder::new();
        let mut compile = LineBuilder::new();
        for column in 0..width {
            let t0 = app.start + column as f64 * per_cell;
            let t1 = t0 + per_cell;
            let (glyph, style) = band_cell(lane, app.filter, t0, t1);
            let (cglyph, cstyle) = compile_cell(lane, app.filter, t0, t1);
            let highlight = Some(column) == cursor;
            let style = if highlight {
                style.bg(Color::Indexed(236))
            } else {
                style
            };
            let cstyle = if highlight {
                cstyle.bg(Color::Indexed(236))
            } else {
                cstyle
            };
            band.push(glyph, style);
            compile.push(cglyph, cstyle);
        }
        let band = band.finish();
        for _ in 1..lane_height {
            lines.push(band.clone());
        }
        lines.push(compile.finish());
    }
    lines
}

fn details(app: &App, trace: &Trace, lanes: &[Lane], inner_height: usize) -> Vec<Line<'static>> {
    let mut lines = Vec::new();
    if let Some(lane) = lanes.get(app.selected) {
        let mut summary = format!(
            "{}: {} at cursor, {} inline site(s)",
            lane.name,
            lane.level_at(app.cursor).name(),
            lane.inline_count
        );
        if let Some(record) = trace.routines.iter().find(|r| r.index == lane.routine)
            && let Some(counts) = record.engines
        {
            summary.push_str(&format!(
                " · instructions {} (interp {}, winch {}, cranelift {}, speed {})",
                record.instructions, counts[0], counts[1], counts[2], counts[3]
            ));
        }
        lines.push(Line::from(TextSpan::styled(
            summary,
            Style::new().add_modifier(Modifier::BOLD),
        )));
    }
    let mut nearest: Vec<usize> = (0..trace.events.len())
        .filter(|index| app.filter.allows(trace.events[*index].kind))
        .collect();
    nearest.sort_by(|a, b| {
        let da = (trace.events[*a].at_ms - app.cursor).abs();
        let db = (trace.events[*b].at_ms - app.cursor).abs();
        da.total_cmp(&db)
    });
    nearest.truncate(inner_height.saturating_sub(1));
    nearest.sort_by(|a, b| trace.events[*a].at_ms.total_cmp(&trace.events[*b].at_ms));
    for index in nearest {
        let event = &trace.events[index];
        lines.push(Line::from(vec![
            TextSpan::raw(format!("{:>10.3} ms  ", event.at_ms)),
            TextSpan::raw(format!("{:<12} ", truncate(&event.name, 12))),
            TextSpan::styled(
                format!("{:<10} ", event.kind.name()),
                kind_style(event.kind),
            ),
            TextSpan::raw(event.detail.clone()),
        ]));
    }
    lines
}

fn help_lines() -> Vec<Line<'static>> {
    [
        "Cursor      ← → (h l)   pan [ ]   zoom + -   fit 0   start/end Home End",
        "Events      n next   p previous (within the current filter)",
        "Lanes       ↑ ↓ (k j)  select a routine",
        "Filter      f cycles all / compile+install / entries+OSR+inline",
        "Markers     ▲ install   ◆ OSR   ◇ inline (at first native run)   ✖ failed",
        "Bars        █▒ tier at that time   ━ compiling   ┄ queued for a worker",
        "Other       ? help   q quit",
    ]
    .into_iter()
    .map(|text| Line::from(text.to_owned()))
    .collect()
}

/// Draw the whole viewer.
pub(crate) fn render(frame: &mut Frame, app: &mut App, trace: &Trace, lanes: &[Lane], file: &str) {
    let area = frame.area();
    let [header, body, detail_area, footer] = Layout::vertical([
        Constraint::Length(1),
        Constraint::Min(3),
        Constraint::Length(DETAIL_LINES),
        Constraint::Length(1),
    ])
    .areas(area);

    frame.render_widget(Paragraph::new(title_line(app, trace, file)), header);

    let [labels, timeline] =
        Layout::horizontal([Constraint::Length(LABEL_WIDTH), Constraint::Min(10)]).areas(body);
    // Use spare height for thicker lanes: two lines minimum, four at most.
    let available = body.height.saturating_sub(1) as usize;
    let lane_height = (available / lanes.len().max(1)).clamp(LINES_PER_LANE as usize, 4);
    let rows = available / lane_height;
    app.fit_lanes(rows, lanes.len());
    let shown: Vec<&Lane> = lanes.iter().skip(app.scroll).take(rows).collect();
    if lanes.is_empty() {
        frame.render_widget(
            Paragraph::new(
                "no routine activity was recorded; run with --jit-trace to observe execution",
            )
            .style(Style::new().fg(Color::DarkGray)),
            body,
        );
    }
    frame.render_widget(
        Paragraph::new(label_lines(app, &shown, labels.width, lane_height)),
        labels,
    );
    frame.render_widget(
        Paragraph::new(timeline_lines(
            app,
            &shown,
            timeline.width as usize,
            lane_height,
        )),
        timeline,
    );

    let detail_block = Block::bordered().title(" events near cursor ");
    let inner = detail_block.inner(detail_area);
    frame.render_widget(detail_block, detail_area);
    let detail_lines = details(app, trace, lanes, inner.height as usize);
    frame.render_widget(Paragraph::new(detail_lines), inner);

    frame.render_widget(
        Paragraph::new(
            " ←→ cursor · [ ] pan · +/- zoom · 0 fit · ↑↓ lane · n/p event · f filter · ? help · q quit",
        )
        .style(Style::new().add_modifier(Modifier::REVERSED)),
        footer,
    );

    if app.help {
        let height = (help_lines().len() as u16 + 2).min(area.height);
        let width = area.width.min(84);
        let popup = Rect {
            x: area.x + (area.width.saturating_sub(width)) / 2,
            y: area.y + (area.height.saturating_sub(height)) / 2,
            width,
            height,
        };
        frame.render_widget(Clear, popup);
        frame.render_widget(
            Paragraph::new(help_lines()).block(Block::bordered().title(" keys ")),
            popup,
        );
    }
}
