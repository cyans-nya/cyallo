//! Human-readable layout for a report [`Document`].
use super::{
    Look,
    model::{Align, Block, Cell, Document, Section, Table, Tone, Value},
};

const WIDTH: usize = 72;
const LABEL_WIDTH: usize = 24;
const BAR_WIDTH: usize = 10;

fn paint(look: Look, tone: Tone, text: &str) -> String {
    let code = match tone {
        Tone::Plain => return text.to_owned(),
        Tone::Strong => "1",
        Tone::Dim => "2",
        Tone::Good => "32",
        Tone::Warn => "33",
        Tone::Bad => "1;31",
        Tone::Accent => "36",
        Tone::Osr => "35",
        Tone::Speed => "1;32",
        Tone::Title => "1;36",
    };
    if look.color {
        format!("\x1b[{code}m{text}\x1b[0m")
    } else {
        text.to_owned()
    }
}

fn group(digits: &str) -> String {
    let mut out = String::with_capacity(digits.len() + digits.len() / 3);
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i) % 3 == 0 {
            out.push(',');
        }
        out.push(c);
    }
    out
}

pub(super) fn count(n: u64) -> String {
    group(&n.to_string())
}

fn value_text(value: &Value) -> String {
    match value {
        Value::Int(n) => count(*n),
        Value::Ms(ms) => format!("{ms:.3} ms"),
        Value::Pct(percent) => format!("{percent:.2}%"),
        Value::Text(text) => text.clone(),
        Value::Bool(flag) => flag.to_string(),
        Value::Null => "-".to_owned(),
    }
}

fn bar(look: Look, percent: f64) -> String {
    let filled = ((percent / 100.0) * BAR_WIDTH as f64)
        .round()
        .clamp(0.0, BAR_WIDTH as f64) as usize;
    if look.ascii {
        format!("{}{}", "#".repeat(filled), ".".repeat(BAR_WIDTH - filled))
    } else {
        format!("{}{}", "█".repeat(filled), "░".repeat(BAR_WIDTH - filled))
    }
}

/// Every output line starts with this prefix: a frame edge when boxed,
/// otherwise a plain indent.
fn edge(look: Look) -> &'static str {
    if look.boxed { "│ " } else { "  " }
}

fn rule_char(look: Look) -> &'static str {
    if look.ascii { "-" } else { "─" }
}

fn heading(out: &mut String, look: Look, title: &str) {
    let used = title.chars().count() + 4;
    let fill = WIDTH.saturating_sub(used).max(3);
    let line = if look.ascii {
        format!("== {title} ==")
    } else if look.boxed {
        format!("╭─ {title} {}", "─".repeat(fill))
    } else {
        format!("━━ {title} {}", "━".repeat(fill))
    };
    out.push_str(&paint(look, Tone::Title, &line));
    out.push('\n');
}

fn fields(out: &mut String, look: Look, rows: &[(&'static str, Cell)]) {
    for (label, cell) in rows {
        let value = paint(look, cell.tone, &value_text(&cell.value));
        out.push_str(edge(look));
        out.push_str(&format!("{label:<LABEL_WIDTH$}"));
        out.push_str(&value);
        out.push('\n');
    }
}

fn note(out: &mut String, look: Look, text: &str) {
    out.push_str(edge(look));
    out.push_str(&paint(look, Tone::Dim, text));
    out.push('\n');
}

fn table(out: &mut String, look: Look, table: &Table) {
    // Each cell as (plain text, painted text, tone) after the bar is added.
    let texts: Vec<Vec<(String, Tone)>> = table
        .rows
        .iter()
        .map(|row| {
            row.iter()
                .zip(&table.columns)
                .map(|(cell, column)| {
                    let text = match (&cell.value, column.bar) {
                        (Value::Pct(percent), true) => {
                            format!("{} {}", bar(look, *percent), value_text(&cell.value))
                        }
                        _ => value_text(&cell.value),
                    };
                    (text, cell.tone)
                })
                .collect()
        })
        .collect();
    let widths: Vec<usize> = table
        .columns
        .iter()
        .enumerate()
        .map(|(i, column)| {
            texts
                .iter()
                .map(|row| row[i].0.chars().count())
                .chain([column.head.chars().count()])
                .max()
                .unwrap_or(0)
        })
        .collect();
    let separator = if look.boxed { " │ " } else { "  " };
    let last = table.columns.len().saturating_sub(1);
    let write_row = |out: &mut String, cells: Vec<(String, Tone)>| {
        out.push_str(edge(look));
        for (i, (text, tone)) in cells.iter().enumerate() {
            if i > 0 {
                out.push_str(separator);
            }
            let pad = widths[i].saturating_sub(text.chars().count());
            let painted = paint(look, *tone, text);
            match table.columns[i].align {
                Align::Right => {
                    out.push_str(&" ".repeat(pad));
                    out.push_str(&painted);
                }
                Align::Left => {
                    out.push_str(&painted);
                    if i != last {
                        out.push_str(&" ".repeat(pad));
                    }
                }
            }
        }
        out.push('\n');
    };
    let heads = table
        .columns
        .iter()
        .map(|column| (column.head.to_owned(), Tone::Strong))
        .collect();
    write_row(out, heads);
    let total: usize = widths.iter().sum::<usize>() + separator.chars().count() * last;
    out.push_str(edge(look));
    if look.boxed {
        let mut rule = String::new();
        for (i, width) in widths.iter().enumerate() {
            if i > 0 {
                rule.push_str("─┼─");
            }
            rule.push_str(&"─".repeat(*width));
        }
        out.push_str(&paint(look, Tone::Dim, &rule));
    } else {
        out.push_str(&paint(look, Tone::Dim, &rule_char(look).repeat(total)));
    }
    out.push('\n');
    if table.rows.is_empty() {
        note(out, look, "(none)");
        return;
    }
    for row in texts {
        write_row(out, row);
    }
}

fn section(out: &mut String, look: Look, section: &Section) {
    if !section.title.is_empty() {
        out.push('\n');
        heading(out, look, section.title);
    }
    for block in &section.blocks {
        match block {
            Block::Fields(rows) => fields(out, look, rows),
            Block::Table(table_block) => table(out, look, table_block),
            Block::Note(text) => note(out, look, text),
        }
    }
    if look.boxed && !section.title.is_empty() {
        out.push_str(&paint(
            look,
            Tone::Dim,
            &format!("╰{}", "─".repeat(WIDTH - 1)),
        ));
        out.push('\n');
    }
}

/// Render the whole document in the given look. The output ends with a
/// newline after the last line.
pub(crate) fn render(document: &Document, look: Look) -> String {
    let mut out = String::new();
    heading(&mut out, look, &document.title);
    for section_block in &document.sections {
        section(&mut out, look, section_block);
    }
    out
}
