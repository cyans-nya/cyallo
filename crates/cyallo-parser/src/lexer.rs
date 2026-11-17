//! Allocation-free token text: the stream borrows source and owns only token tags/spans.
use crate::{Token, TokenKind};
use cyallo_ast::{Diagnostic, SPAN, Span};
#[derive(Debug)]
pub struct Lexed<'src> {
    source: &'src str,
    tokens: Box<[Token]>,
}
impl<'src> Lexed<'src> {
    pub(crate) fn fragment(&self, range: std::ops::Range<usize>) -> Self {
        let end = self.tokens[range.end].span.start;
        let mut tokens = self.tokens[range].to_vec();
        tokens.push(Token {
            kind: TokenKind::Eof,
            span: Span::new(end, end),
        });
        Self {
            source: self.source,
            tokens: tokens.into_boxed_slice(),
        }
    }
    pub fn source(&self) -> &'src str {
        self.source
    }
    pub fn text(&self, token: Token) -> &'src str {
        token.span.source_text(self.source)
    }
    pub fn tokens(&self) -> &[Token] {
        &self.tokens
    }
}
impl std::ops::Deref for Lexed<'_> {
    type Target = [Token];
    fn deref(&self) -> &Self::Target {
        &self.tokens
    }
}
impl IntoIterator for Lexed<'_> {
    type Item = Token;
    type IntoIter = std::vec::IntoIter<Token>;
    fn into_iter(self) -> Self::IntoIter {
        self.tokens.into_vec().into_iter()
    }
}
pub fn lex_modern(source: &str) -> Result<Lexed<'_>, Diagnostic> {
    lex(source)
}
pub fn lex(source: &str) -> Result<Lexed<'_>, Diagnostic> {
    if source.len() > u32::MAX as usize {
        return Err(Diagnostic::new(
            "E001",
            "source exceeds 4 GiB span limit",
            SPAN,
        ));
    }
    let bytes = source.as_bytes();
    let mut pos = 0;
    let mut tokens = Vec::with_capacity((bytes.len() / 2 + 1).min(4096));
    while pos < bytes.len() {
        let start = pos;
        let kind = match bytes[pos] {
            b' ' | b'\t' | b'\r' => {
                pos += 1;
                continue;
            }
            b'\n' => {
                pos += 1;
                TokenKind::Newline
            }
            b'/' if bytes.get(pos + 1) == Some(&b'/') => {
                pos += 2;
                while pos < bytes.len() && bytes[pos] != b'\n' {
                    pos += 1;
                }
                continue;
            }
            b'\''
                if bytes.get(pos + 1).is_some_and(u8::is_ascii_alphabetic)
                    && bytes.get(pos + 2) != Some(&b'\'') =>
            {
                pos += 1;
                while bytes
                    .get(pos)
                    .is_some_and(|b| b.is_ascii_alphanumeric() || *b == b'_')
                {
                    pos += 1;
                }
                TokenKind::Lifetime
            }
            quote @ (b'"' | b'\'') => {
                pos += 1;
                let content = pos;
                while pos < bytes.len()
                    && !matches!(bytes[pos], b'\r' | b'\n')
                    && bytes[pos] != quote
                {
                    if bytes[pos] == b'\\'
                        && bytes
                            .get(pos + 1)
                            .is_some_and(|b| !matches!(b, b'\n' | b'\r'))
                    {
                        pos += 2;
                    } else {
                        pos += 1;
                    }
                }
                if bytes.get(pos) != Some(&quote) {
                    return Err(Diagnostic::new(
                        "E001",
                        "unterminated literal",
                        Span::new(start as u32, pos as u32),
                    ));
                }
                let text = &source[content..pos];
                pos += 1;
                if quote == b'\'' {
                    if text.chars().count() != 1 && !(text.starts_with('\\') && text.len() == 2) {
                        return Err(Diagnostic::new(
                            "E001",
                            "CHAR requires one Unicode scalar",
                            Span::new(start as u32, pos as u32),
                        ));
                    }
                    TokenKind::Char
                } else {
                    TokenKind::Text
                }
            }
            b'0'..=b'9' => {
                pos += 1;
                while bytes.get(pos).is_some_and(u8::is_ascii_digit) {
                    pos += 1;
                }
                let mut kind = if bytes.get(pos) == Some(&b'.')
                    && bytes.get(pos + 1) != Some(&b'.')
                    && bytes.get(pos + 1) != Some(&b'*')
                    && !(tokens
                        .last()
                        .is_some_and(|t: &Token| t.kind == TokenKind::Dot))
                {
                    pos += 1;
                    let fraction = pos;
                    while bytes.get(pos).is_some_and(u8::is_ascii_digit) {
                        pos += 1;
                    }
                    if pos == fraction {
                        return Err(Diagnostic::new(
                            "E001",
                            "REAL requires digits after decimal point",
                            Span::new(start as u32, pos as u32),
                        ));
                    }
                    TokenKind::Real
                } else {
                    TokenKind::Integer
                };
                if matches!(bytes.get(pos), Some(b'e' | b'E')) {
                    kind = TokenKind::Real;
                    pos += 1;
                    if matches!(bytes.get(pos), Some(b'+' | b'-')) {
                        pos += 1;
                    }
                    let digits = pos;
                    while bytes.get(pos).is_some_and(u8::is_ascii_digit) {
                        pos += 1;
                    }
                    if pos == digits {
                        return Err(Diagnostic::new(
                            "E001",
                            "expected exponent digits",
                            Span::new(start as u32, pos as u32),
                        ));
                    }
                }
                while bytes
                    .get(pos)
                    .is_some_and(|b| b.is_ascii_alphanumeric() || *b == b'_')
                {
                    pos += 1;
                }
                kind
            }
            b'a'..=b'z' | b'A'..=b'Z' | b'_' => {
                pos += 1;
                while bytes
                    .get(pos)
                    .is_some_and(|c| c.is_ascii_alphanumeric() || *c == b'_')
                {
                    pos += 1;
                }
                let word = &source[start..pos];
                TokenKind::from_word(word)
            }
            _ => {
                let two = match (bytes[pos], bytes.get(pos + 1)) {
                    (b'-', Some(b'>')) => Some(TokenKind::Arrow),
                    (b'=', Some(b'>')) => Some(TokenKind::FatArrow),
                    (b'=', Some(b'=')) => Some(TokenKind::EqualEqual),
                    (b'!', Some(b'=')) => Some(TokenKind::BangEqual),
                    (b'&', Some(b'&')) => Some(TokenKind::AndAnd),
                    (b'|', Some(b'|')) => Some(TokenKind::OrOr),
                    (b'+', Some(b'=')) => Some(TokenKind::PlusAssign),
                    (b'-', Some(b'=')) => Some(TokenKind::MinusAssign),
                    (b'*', Some(b'=')) => Some(TokenKind::StarAssign),
                    (b'/', Some(b'=')) => Some(TokenKind::SlashAssign),
                    (b'.', Some(b'.')) => Some(TokenKind::Range),
                    (b'<', Some(b'=')) => Some(TokenKind::LessEqual),
                    (b'>', Some(b'=')) => Some(TokenKind::GreaterEqual),
                    _ => None,
                };
                if let Some(kind) = two {
                    pos += 2;
                    if kind == TokenKind::Range && bytes.get(pos) == Some(&b'=') {
                        pos += 1;
                        TokenKind::RangeInclusive
                    } else {
                        kind
                    }
                } else {
                    let c = source[pos..].chars().next().unwrap();
                    pos += c.len_utf8();
                    match c {
                        '#' => TokenKind::Hash,
                        '@' => TokenKind::At,
                        '.' => TokenKind::Dot,
                        '{' => TokenKind::LeftBrace,
                        '}' => TokenKind::RightBrace,
                        ';' => TokenKind::Semicolon,
                        '!' => TokenKind::Bang,
                        '%' => TokenKind::Percent,
                        '+' => TokenKind::Plus,
                        '-' => TokenKind::Minus,
                        '*' => TokenKind::Star,
                        '/' => TokenKind::Slash,
                        '&' => TokenKind::Ampersand,
                        '|' => TokenKind::Pipe,
                        '=' => TokenKind::Equal,
                        '<' => TokenKind::Less,
                        '>' => TokenKind::Greater,
                        '(' => TokenKind::LeftParen,
                        ')' => TokenKind::RightParen,
                        '[' => TokenKind::LeftBracket,
                        ']' => TokenKind::RightBracket,
                        ':' => TokenKind::Colon,
                        ',' => TokenKind::Comma,
                        _ => {
                            return Err(Diagnostic::new(
                                "E001",
                                format!("unexpected character {c:?}"),
                                Span::new(start as u32, pos as u32),
                            ));
                        }
                    }
                }
            }
        };
        tokens.push(Token {
            kind,
            span: Span::new(start as u32, pos as u32),
        });
    }
    tokens.push(Token {
        kind: TokenKind::Eof,
        span: Span::empty(pos as u32),
    });
    Ok(Lexed {
        source,
        tokens: tokens.into_boxed_slice(),
    })
}
