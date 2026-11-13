//! Oxc's renderer consumes the same byte spans used by the frontend and runtime.
use crate::{Diagnostic, Dialect, highlight::highlight_with_dialect};
use oxc_diagnostics::{NamedSource, OxcDiagnostic};
pub fn render_program(
    name: &str,
    program: &crate::ir::Program,
    diagnostic: &Diagnostic,
    color: bool,
    dialect: Dialect,
) -> String {
    if let Some((path, source, local)) =
        crate::modules::diagnostic_context(program.source(), program.source_modules(), diagnostic)
    {
        render_with_color_and_dialect(path, source, &local, color, dialect)
    } else {
        render_with_color_and_dialect(name, program.source(), diagnostic, color, dialect)
    }
}
/// Adapt structured compiler/runtime diagnostics without changing their public API.
pub fn to_oxc(diagnostic: &Diagnostic) -> OxcDiagnostic {
    OxcDiagnostic::error(diagnostic.message.clone())
        .with_error_code("cyallo", diagnostic.code)
        .with_label(diagnostic.span.primary())
        .and_labels(
            diagnostic
                .related
                .iter()
                .map(|(span, message)| span.label(message.clone())),
        )
}
/// Render a named source using Oxc's deterministic, non-interactive theme.
/// Source is borrowed; reporting does not copy the entire program text.
pub fn render(name: &str, source: &str, diagnostic: &Diagnostic) -> String {
    to_oxc(diagnostic).render_with_source_code(NamedSource::new(name, source))
}

/// Render with Oxc's colored terminal theme, or its plain deterministic theme.
pub fn render_with_color(name: &str, source: &str, diagnostic: &Diagnostic, color: bool) -> String {
    render_with_color_and_dialect(name, source, diagnostic, color, Dialect::Modern)
}

/// Render a diagnostic and syntax-highlight its source excerpt using the selected dialect.
pub fn render_with_color_and_dialect(
    name: &str,
    source: &str,
    diagnostic: &Diagnostic,
    color: bool,
    dialect: Dialect,
) -> String {
    use oxc_diagnostics::{
        Diagnostic as OxcDiagnosticTrait, GraphicalReportHandler, GraphicalTheme, LabeledSpan,
        SourceCode,
    };
    use std::{borrow::Cow, fmt};
    #[derive(Debug)]
    struct BorrowedReport<'a> {
        inner: OxcDiagnostic,
        source: NamedSource<&'a str>,
    }
    impl fmt::Display for BorrowedReport<'_> {
        fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            fmt::Display::fmt(&self.inner, f)
        }
    }
    impl std::error::Error for BorrowedReport<'_> {}
    impl OxcDiagnosticTrait for BorrowedReport<'_> {
        fn code(&self) -> Option<Cow<'_, str>> {
            self.inner.code()
        }
        fn labels(&self) -> &[LabeledSpan] {
            self.inner.labels()
        }
        fn source_code(&self) -> Option<&dyn SourceCode> {
            Some(&self.source)
        }
    }
    let report = BorrowedReport {
        inner: to_oxc(diagnostic),
        source: NamedSource::new(name, source),
    };
    let theme = if color {
        GraphicalTheme::unicode()
    } else {
        GraphicalTheme::none()
    };
    let mut output = String::new();
    GraphicalReportHandler::new_themed(theme)
        .with_width(80)
        .with_links(false)
        .render_report(&mut output, &report)
        .expect("formatting into String cannot fail");
    if color {
        highlight_source_lines(&output, dialect)
    } else {
        output
    }
}

fn highlight_source_lines(report: &str, dialect: Dialect) -> String {
    let mut highlighted = String::with_capacity(report.len());
    for line in report.split_inclusive('\n') {
        let Some(separator) = line.find(" │ ") else {
            highlighted.push_str(line);
            continue;
        };
        let source_start = separator + " │ ".len();
        match highlight_with_dialect(&line[source_start..], true, dialect) {
            Ok(source_line) => {
                highlighted.push_str(&line[..source_start]);
                highlighted.push_str(&source_line);
            }
            Err(_) => highlighted.push_str(line),
        }
    }
    highlighted
}
