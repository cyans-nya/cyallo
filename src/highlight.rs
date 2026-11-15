//! Lossless terminal coloring using compiler tokens and lightweight syntax context.
//!
//! Highlighting never resolves names or requires a complete program. Contextual
//! keywords remain identifiers in the lexer; their presentation belongs here.
use crate::{Diagnostic, Dialect, Span};

#[derive(Clone, Copy)]
enum Style {
    Plain,
    Keyword,
    Type,
    Function,
    Namespace,
    Constant,
    Literal,
    String,
    Comment,
    Operator,
    Punctuation,
    Annotation,
}

impl Style {
    fn ansi(self) -> Option<&'static str> {
        match self {
            Self::Plain => None,
            Self::Keyword => Some("\x1b[1;35m"),
            Self::Type => Some("\x1b[1;34m"),
            Self::Function => Some("\x1b[36m"),
            Self::Namespace => Some("\x1b[34m"),
            Self::Constant => Some("\x1b[1;33m"),
            Self::Literal => Some("\x1b[33m"),
            Self::String => Some("\x1b[32m"),
            Self::Comment | Self::Punctuation => Some("\x1b[90m"),
            Self::Operator => Some("\x1b[91m"),
            Self::Annotation => Some("\x1b[95m"),
        }
    }
}

fn paint(out: &mut String, text: &str, style: Style) {
    if text.is_empty() {
        return;
    }
    if let Some(color) = style.ansi() {
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
            paint(out, &line[index..], Style::Comment);
        } else {
            out.push_str(line);
        }
    }
}

fn render(source: &str, tokens: impl Iterator<Item = (Span, Style)>) -> String {
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
    out
}

fn modern_style(tokens: &cyallo_parser::Lexed<'_>, index: usize) -> Style {
    use cyallo_parser::TokenKind as K;
    let token = tokens[index];
    let text = tokens.text(token);
    let previous = index.checked_sub(1).map(|i| tokens[i].kind);
    let previous_text = index.checked_sub(1).map(|i| tokens.text(tokens[i]));
    let next = tokens.get(index + 1).map(|t| t.kind);
    let before_previous = index.checked_sub(2).map(|i| tokens[i].kind);
    let after_next = tokens.get(index + 2).map(|t| t.kind);

    if token.kind == K::Identifier || token.kind.is_keyword() {
        // Path segments can spell reserved words (std::gc, std::string, ...).
        let in_path = (previous == Some(K::Colon) && before_previous == Some(K::Colon))
            || (next == Some(K::Colon) && after_next == Some(K::Colon));
        if in_path {
            return if next == Some(K::LeftParen) {
                Style::Function
            } else {
                Style::Namespace
            };
        }
        if previous == Some(K::Dot) {
            return if next == Some(K::LeftParen) {
                Style::Function
            } else {
                Style::Plain
            };
        }
        if previous == Some(K::LeftBracket) && before_previous == Some(K::Hash) {
            return Style::Annotation;
        }
    }

    match token.kind {
        K::Text | K::Char => Style::String,
        K::Integer | K::Real | K::True | K::False => Style::Literal,
        K::CharType | K::StringType => Style::Type,
        K::Lifetime | K::Hash | K::At => Style::Annotation,
        K::Identifier => {
            // Declaration names take precedence over contextual spellings.
            match previous {
                Some(K::Fn | K::Proc) => return Style::Function,
                Some(K::Record) => return Style::Type,
                Some(K::Const) => return Style::Constant,
                Some(K::Let) if text != "mut" => return Style::Plain,
                _ => {}
            }
            match previous_text {
                Some("mod") => return Style::Namespace,
                Some("trait") => return Style::Type,
                Some("mut") if before_previous == Some(K::Let) => return Style::Plain,
                _ => {}
            }
            // Parameter/field names may also use contextual spellings.
            if next == Some(K::Colon) {
                return Style::Plain;
            }
            let contextual_keyword = match text {
                "use" | "pub" | "mod" | "as" | "trait" | "impl" | "property" | "dyn" => true,
                "mut" => matches!(previous, Some(K::Let | K::Ampersand | K::Lifetime)),
                "from" => previous == Some(K::Yield),
                "local" | "shared" => previous == Some(K::At),
                "crate" => {
                    previous == Some(K::LeftParen)
                        && index
                            .checked_sub(2)
                            .is_some_and(|i| tokens.text(tokens[i]) == "pub")
                }
                "read" => {
                    matches!(previous, Some(K::LeftParen | K::Comma)) && next == Some(K::Identifier)
                }
                _ => false,
            };
            if contextual_keyword {
                Style::Keyword
            } else if next == Some(K::LeftParen) {
                Style::Function
            } else if cyallo_ast::NumberKind::parse(text).is_some()
                || matches!(text, "bool" | "Self")
                || matches!(previous, Some(K::Colon | K::Arrow))
                || matches!(previous_text, Some("as" | "dyn" | "impl"))
            {
                Style::Type
            } else {
                Style::Plain
            }
        }
        K::Eof | K::Newline => Style::Plain,
        K::LeftParen
        | K::RightParen
        | K::LeftBracket
        | K::RightBracket
        | K::LeftBrace
        | K::RightBrace
        | K::Colon
        | K::Comma
        | K::Dot
        | K::Semicolon => Style::Punctuation,
        kind if kind.is_keyword() => Style::Keyword,
        _ => Style::Operator,
    }
}

fn compatibility_style(tokens: &caie_int_parser::Lexed<'_>, index: usize) -> Style {
    use caie_int_parser::TokenKind as K;
    match tokens[index].kind {
        K::Text | K::Char => Style::String,
        K::Integer | K::Real | K::True | K::False => Style::Literal,
        K::IntegerType | K::RealType | K::Boolean | K::CharType | K::StringType | K::Date => {
            Style::Type
        }
        K::Identifier => {
            let previous = index.checked_sub(1).map(|i| tokens[i].kind);
            if previous == Some(K::Constant) {
                Style::Constant
            } else if previous == Some(K::Type) {
                Style::Type
            } else if tokens
                .get(index + 1)
                .is_some_and(|t| t.kind == K::LeftParen)
            {
                Style::Function
            } else {
                Style::Plain
            }
        }
        K::Eof | K::Newline => Style::Plain,
        K::LeftParen
        | K::RightParen
        | K::LeftBracket
        | K::RightBracket
        | K::LeftBrace
        | K::RightBrace
        | K::Colon
        | K::Comma
        | K::Dot
        | K::Semicolon => Style::Punctuation,
        K::And | K::Or | K::Not | K::Div | K::Mod => Style::Operator,
        kind if kind.is_keyword() => Style::Keyword,
        _ => Style::Operator,
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
    match dialect {
        Dialect::Modern => {
            let tokens = cyallo_parser::lex(source)?;
            Ok(if color {
                render(
                    source,
                    tokens
                        .iter()
                        .enumerate()
                        .map(|(index, token)| (token.span, modern_style(&tokens, index))),
                )
            } else {
                source.to_owned()
            })
        }
        Dialect::Compatibility | Dialect::Simplified | Dialect::Strict => {
            let tokens = caie_int_parser::lex(source)?;
            Ok(if color {
                render(
                    source,
                    tokens
                        .iter()
                        .enumerate()
                        .map(|(index, token)| (token.span, compatibility_style(&tokens, index))),
                )
            } else {
                source.to_owned()
            })
        }
    }
}
