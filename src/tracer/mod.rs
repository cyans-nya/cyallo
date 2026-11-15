//! `cyallo trace`: an interactive timeline of a JIT trace written with
//! `--jit-trace --jit-trace-format=json`. It shows, per routine, which tier
//! was installed when, compile work, OSR entries, failures and inline
//! decisions.
mod app;
mod json;
mod timeline;
mod view;

use app::{Action, App};
use color_eyre::eyre::{WrapErr, eyre};
use crossterm::event::{self, Event, KeyEventKind};
use std::{
    io::{self, IsTerminal},
    path::Path,
    time::Duration,
};
use timeline::Trace;

/// Restores the terminal even when drawing fails.
struct TerminalGuard;

impl Drop for TerminalGuard {
    fn drop(&mut self) {
        ratatui::restore();
    }
}

/// Open the viewer on a trace file.
pub(crate) fn run(path: &Path) -> color_eyre::Result<()> {
    if !(io::stdin().is_terminal() && io::stdout().is_terminal()) {
        return Err(eyre!(
            "cyallo trace is interactive; run it in a terminal (stdin and stdout must be terminals)"
        ));
    }
    let text = std::fs::read_to_string(path)
        .wrap_err_with(|| format!("cannot read {}", path.display()))?;
    let trace = Trace::parse(&text).map_err(|error| eyre!("{}: {error}", path.display()))?;
    let lanes = timeline::lanes(&trace);
    let file = path.file_name().map_or_else(
        || path.display().to_string(),
        |name| name.to_string_lossy().into_owned(),
    );
    let mut app = App::new(trace.span_ms);

    let mut terminal = ratatui::try_init()?;
    let _guard = TerminalGuard;
    loop {
        terminal.draw(|frame| view::render(frame, &mut app, &trace, &lanes, &file))?;
        if event::poll(Duration::from_millis(200))?
            && let Event::Key(key) = event::read()?
            && key.kind == KeyEventKind::Press
            && app.key(key.code, &trace, lanes.len()) == Action::Quit
        {
            break;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::timeline::{Trace, fixtures::SAMPLE, lanes};
    use super::{app::App, view};
    use ratatui::{Terminal, backend::TestBackend, buffer::Buffer};

    fn screen(width: u16, height: u16, app: &mut App) -> String {
        let trace = Trace::parse(SAMPLE).unwrap();
        let lanes = lanes(&trace);
        let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
        terminal
            .draw(|frame| view::render(frame, app, &trace, &lanes, "sample.json"))
            .unwrap();
        buffer_text(terminal.backend().buffer())
    }

    fn buffer_text(buffer: &Buffer) -> String {
        let area = buffer.area;
        let mut out = String::new();
        for y in 0..area.height {
            for x in 0..area.width {
                out.push_str(buffer[(x, y)].symbol());
            }
            out.push('\n');
        }
        out
    }

    #[test]
    fn draws_lanes_markers_and_compile_bars() {
        let mut app = App::new(Trace::parse(SAMPLE).unwrap().span_ms);
        let text = screen(120, 24, &mut app);
        assert!(text.contains("cyallo trace · sample.json"), "{text}");
        assert!(text.contains("merge_sort"), "{text}");
        assert!(text.contains("<main>"), "{text}");
        // Installs, the OSR entry and the failure all have markers on screen.
        assert!(text.contains('▲'), "{text}");
        assert!(text.contains('◆'), "{text}");
        assert!(text.contains('✖'), "{text}");
        // Winch and Cranelift bands, and compile bars for the Cranelift job.
        assert!(text.contains('▒'), "{text}");
        assert!(text.contains('█'), "{text}");
        assert!(text.contains('━'), "{text}");
        assert!(text.contains("install"), "{text}");
    }

    #[test]
    fn filters_hide_markers_of_the_other_kind() {
        let mut app = App::new(Trace::parse(SAMPLE).unwrap().span_ms);
        app.filter = super::app::Filter::Transitions;
        let text = screen(120, 24, &mut app);
        assert!(text.contains('◆'), "{text}");
        assert!(!text.contains('▲'), "{text}");
        assert!(!text.contains('━'), "{text}");
    }

    #[test]
    fn help_overlay_lists_the_keys() {
        let mut app = App::new(Trace::parse(SAMPLE).unwrap().span_ms);
        app.help = true;
        let text = screen(100, 30, &mut app);
        assert!(text.contains("keys"), "{text}");
        assert!(text.contains("zoom"), "{text}");
    }

    #[test]
    fn small_terminals_do_not_panic() {
        let mut app = App::new(Trace::parse(SAMPLE).unwrap().span_ms);
        for (width, height) in [(20, 6), (1, 1), (40, 12)] {
            let _ = screen(width, height, &mut app);
        }
    }
}
