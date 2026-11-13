//! Lossless terminal coloring using the compiler's compact lexer.
use crate::{Diagnostic, Dialect};
fn paint(out: &mut String, text: &str, color: Option<&str>) {
    if text.is_empty() {
        return;
    }
    if let Some(color) = color {
        out.push_str(color);
        out.push_str(text);
        out.push_str("\x1b[0m");
    } else {
        out.push_str(text);
    }
}
fn gap(out: &mut String, text: &str) {
    for line in text.split_inclusive('\n') {
        if let Some(index) = line.find("//") {
            out.push_str(&line[..index]);
            paint(out, &line[index..], Some("\x1b[90m"));
        } else {
            out.push_str(line);
        }
    }
}
/// Preserve every original byte, adding only ANSI SGR sequences when color is enabled.
pub fn highlight(source: &str, color: bool) -> Result<String, Diagnostic> {
    highlight_with_dialect(source, color, Dialect::Modern)
}
pub fn highlight_with_dialect(
    source: &str,
    color: bool,
    dialect: Dialect,
) -> Result<String, Diagnostic> {
    macro_rules! tokens {
        ($parser:ident) => {{
            use $parser::TokenKind;
            let tokens = $parser::lex(source)?;
            tokens
                .iter()
                .enumerate()
                .map(|(index, token)| {
                    let style = match token.kind {
                        TokenKind::Text | TokenKind::Char => Some("\x1b[32m"),
                        TokenKind::Integer | TokenKind::Real => Some("\x1b[33m"),
                        TokenKind::Identifier
                            if tokens
                                .get(index + 1)
                                .is_some_and(|t| t.kind == TokenKind::LeftParen) =>
                        {
                            Some("\x1b[36m")
                        }
                        TokenKind::Identifier | TokenKind::Newline | TokenKind::Eof => None,
                        kind if kind.is_keyword() => Some("\x1b[1;35m"),
                        _ => Some("\x1b[36m"),
                    };
                    (token.span, style)
                })
                .collect::<Vec<_>>()
        }};
    }
    let tokens = match dialect {
        Dialect::Modern => tokens!(cyallo_parser),
        Dialect::Compatibility | Dialect::Simplified | Dialect::Strict => tokens!(caie_int_parser),
    };
    if !color {
        return Ok(source.to_owned());
    }
    let mut out = String::with_capacity(source.len());
    let mut offset = 0;
    for (span, style) in tokens {
        let start = span.start as usize;
        let end = span.end as usize;
        gap(&mut out, &source[offset..start]);
        paint(&mut out, &source[start..end], style);
        offset = end;
    }
    gap(&mut out, &source[offset..]);
    Ok(out)
}
