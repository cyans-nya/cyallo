//! Machine-readable rendering of a report [`Document`]. It is written by hand
//! so the report adds no dependency: the output is a small, fixed shape.
//!
//! The `trace` member is versioned (`"version": 1`). It carries the timeline
//! and the inline decisions in typed form for `cyallo trace`.
use super::model::{Align, Block, Document, Row, Table, TraceDoc, Value};

/// Version of the `trace` member. Bump when a key changes meaning.
pub(crate) const TRACE_VERSION: u64 = 1;

fn escape(out: &mut String, text: &str) {
    out.push('"');
    for c in text.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
}

fn value(out: &mut String, value: &Value) {
    match value {
        Value::Int(n) => out.push_str(&n.to_string()),
        Value::Ms(ms) if ms.is_finite() => out.push_str(&format!("{ms:.3}")),
        Value::Pct(percent) if percent.is_finite() => out.push_str(&format!("{percent:.2}")),
        Value::Ms(_) | Value::Pct(_) => out.push_str("null"),
        Value::Text(text) => escape(out, text),
        Value::Bool(flag) => out.push_str(if *flag { "true" } else { "false" }),
        Value::Null => out.push_str("null"),
    }
}

fn table(out: &mut String, table: &Table) {
    out.push_str("{\"kind\":\"table\",\"columns\":[");
    for (i, column) in table.columns.iter().enumerate() {
        if i > 0 {
            out.push(',');
        }
        out.push_str("{\"head\":");
        escape(out, column.head);
        out.push_str(",\"align\":");
        escape(
            out,
            match column.align {
                Align::Left => "left",
                Align::Right => "right",
            },
        );
        out.push('}');
    }
    out.push_str("],\"rows\":[");
    for (r, row) in table.rows.iter().enumerate() {
        if r > 0 {
            out.push(',');
        }
        out.push('[');
        for (i, cell) in row.iter().enumerate() {
            if i > 0 {
                out.push(',');
            }
            value(out, &cell.value);
        }
        out.push(']');
    }
    out.push_str("]}");
}

/// Write `"key": value` pairs separated by commas; the caller opens and closes
/// the object.
fn member(out: &mut String, first: &mut bool, key: &str) {
    if !*first {
        out.push(',');
    }
    *first = false;
    escape(out, key);
    out.push(':');
}

fn event(out: &mut String, row: &Row) {
    out.push('{');
    let mut first = true;
    member(out, &mut first, "at_ms");
    value(out, &Value::Ms(row.at.as_secs_f64() * 1000.0));
    member(out, &mut first, "kind");
    escape(out, row.kind);
    member(out, &mut first, "routine");
    out.push_str(&row.function.to_string());
    member(out, &mut first, "name");
    escape(out, &row.routine);
    member(out, &mut first, "detail");
    escape(out, &row.detail);
    for (key, attr) in &row.attrs {
        member(out, &mut first, key);
        value(out, attr);
    }
    out.push('}');
}

fn trace(out: &mut String, doc: &TraceDoc) {
    out.push_str("{\"version\":");
    out.push_str(&TRACE_VERSION.to_string());
    out.push_str(",\"mode\":");
    escape(out, doc.mode);
    out.push_str(",\"engine\":");
    escape(out, doc.engine);
    out.push_str(",\"status\":");
    escape(out, doc.status);
    out.push_str(",\"wall_ms\":");
    value(out, &Value::Ms(doc.wall.as_secs_f64() * 1000.0));
    out.push_str(",\"dropped_events\":");
    out.push_str(&doc.dropped_events.to_string());

    out.push_str(",\"routines\":[");
    for (i, routine) in doc.routines.iter().enumerate() {
        if i > 0 {
            out.push(',');
        }
        out.push_str("{\"index\":");
        out.push_str(&routine.index.to_string());
        out.push_str(",\"name\":");
        escape(out, &routine.name);
        out.push_str(",\"calls\":");
        out.push_str(&routine.calls.to_string());
        out.push_str(",\"instructions\":");
        out.push_str(&routine.work.to_string());
        out.push_str(",\"osr\":");
        match routine.osr {
            Some(count) => out.push_str(&count.to_string()),
            None => out.push_str("null"),
        }
        out.push_str(",\"exec_ms\":");
        match routine.exec {
            Some(exec) => value(out, &Value::Ms(exec.as_secs_f64() * 1000.0)),
            None => out.push_str("null"),
        }
        out.push_str(",\"engines\":");
        match routine.engines {
            Some(counts) => {
                out.push('{');
                for (j, engine) in cyallo::metrics::ExecutionEngine::ALL.iter().enumerate() {
                    if j > 0 {
                        out.push(',');
                    }
                    escape(out, engine.name());
                    out.push(':');
                    out.push_str(&counts[*engine as usize].to_string());
                }
                out.push('}');
            }
            None => out.push_str("null"),
        }
        out.push('}');
    }
    out.push(']');

    out.push_str(",\"events\":[");
    for (i, row) in doc.events.iter().enumerate() {
        if i > 0 {
            out.push(',');
        }
        event(out, row);
    }
    out.push(']');

    out.push_str(",\"inlines\":[");
    for (i, site) in doc.inlines.iter().enumerate() {
        if i > 0 {
            out.push(',');
        }
        out.push_str("{\"routine\":");
        out.push_str(&site.routine.to_string());
        out.push_str(",\"routine_name\":");
        escape(out, &site.routine_name);
        out.push_str(",\"pc\":");
        out.push_str(&site.pc.to_string());
        out.push_str(",\"callee\":");
        out.push_str(&site.callee.to_string());
        out.push_str(",\"callee_name\":");
        escape(out, &site.callee_name);
        out.push_str(",\"decision\":");
        escape(out, site.decision);
        out.push_str(",\"cost\":");
        out.push_str(&site.cost.to_string());
        out.push_str(",\"depth\":");
        out.push_str(&site.depth.to_string());
        out.push('}');
    }
    out.push_str("]}");
}

/// Render the document as one JSON object. `inspection` carries the optional
/// compiler inspection text, which is not structured.
pub(crate) fn render(document: &Document, inspection: Option<&str>) -> String {
    let mut out = String::from("{\"title\":");
    escape(&mut out, &document.title);
    out.push_str(",\"sections\":[");
    for (s, section) in document.sections.iter().enumerate() {
        if s > 0 {
            out.push(',');
        }
        out.push_str("{\"title\":");
        escape(&mut out, section.title);
        out.push_str(",\"blocks\":[");
        for (b, block) in section.blocks.iter().enumerate() {
            if b > 0 {
                out.push(',');
            }
            match block {
                Block::Fields(rows) => {
                    out.push_str("{\"kind\":\"fields\",\"fields\":{");
                    for (i, (label, cell)) in rows.iter().enumerate() {
                        if i > 0 {
                            out.push(',');
                        }
                        escape(&mut out, label);
                        out.push(':');
                        value(&mut out, &cell.value);
                    }
                    out.push_str("}}");
                }
                Block::Table(table_block) => table(&mut out, table_block),
                Block::Note(text) => {
                    out.push_str("{\"kind\":\"note\",\"text\":");
                    escape(&mut out, text);
                    out.push('}');
                }
            }
        }
        out.push_str("]}");
    }
    out.push(']');
    if let Some(doc) = &document.trace {
        out.push_str(",\"trace\":");
        trace(&mut out, doc);
    }
    if let Some(text) = inspection {
        out.push_str(",\"inspection\":");
        escape(&mut out, text);
    }
    out.push_str("}\n");
    out
}
