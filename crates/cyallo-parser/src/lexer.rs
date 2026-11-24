//! Allocation-free token text: the stream borrows source and owns only token tags/spans.
use crate::{Token, TokenKind};
use cyallo_ast::{Diagnostic, SPAN, Span};
#[derive(Debug)]
pub struct Lexed<'src> {
    source: &'src str,
    tokens: Box<[Token]>,
}
impl<'src> Lexed<'src> {
    pub(crate) fn fragment(&self, range: std::ops::Range<usize>) -> TokenWindow<'_, 'src> {
        let end = self.tokens[range.end].span.start;
        TokenWindow {
            source: self.source,
            tokens: &self.tokens[range],
            eof: Token {
                kind: TokenKind::Eof,
                span: Span::new(end, end),
            },
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

    pub(crate) fn into_tokens(self) -> Box<[Token]> {
        self.tokens
    }
}

/// A borrowed parse-unit range with its own terminal token. Lookahead cannot
/// read the next unit, and diagnostics retain the original source coordinates.
pub(crate) struct TokenWindow<'tokens, 'src> {
    source: &'src str,
    tokens: &'tokens [Token],
    eof: Token,
}

impl<'src> TokenWindow<'_, 'src> {
    pub(crate) fn get(&self, index: usize) -> Option<&Token> {
        if index == self.tokens.len() {
            Some(&self.eof)
        } else {
            self.tokens.get(index)
        }
    }

    pub(crate) fn text(&self, token: Token) -> &'src str {
        token.span.source_text(self.source)
    }
}

impl std::ops::Index<usize> for TokenWindow<'_, '_> {
    type Output = Token;

    fn index(&self, index: usize) -> &Self::Output {
        self.get(index).expect("parser token index within unit")
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
    lex_with::<scan::Adopted>(source)
}

/// Lexer scan strategy for benchmarks and differential tests. Every variant
/// must produce identical tokens and diagnostics; only the byte scans differ.
#[doc(hidden)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ScanVariant {
    /// One byte per whitespace step, scalar comment loop (the original lexer).
    Original,
    /// Scalar whitespace runs, scalar comment scan.
    ScalarRuns,
    CommentSimd,
    WhitespaceSimd,
    BothSimd,
    /// Comment SIMD plus perfect-hash keyword lookup.
    CommentSimdHashed,
    /// Comment SIMD plus a whitespace scan that fast-paths a single blank.
    BothHybrid,
    /// Comment bodies via `memchr`, scalar whitespace (benchmark control).
    #[cfg(feature = "memchr-control")]
    CommentMemchr,
}

#[doc(hidden)]
pub fn lex_variant(source: &str, variant: ScanVariant) -> Result<Lexed<'_>, Diagnostic> {
    match variant {
        ScanVariant::Original => lex_with::<scan::Original>(source),
        ScanVariant::ScalarRuns => lex_with::<scan::ScalarRuns>(source),
        ScanVariant::CommentSimd => lex_with::<scan::CommentSimd>(source),
        ScanVariant::WhitespaceSimd => lex_with::<scan::WhitespaceSimd>(source),
        ScanVariant::BothSimd => lex_with::<scan::BothSimd>(source),
        ScanVariant::CommentSimdHashed => lex_with::<scan::CommentSimdHashed>(source),
        ScanVariant::BothHybrid => lex_with::<scan::BothHybrid>(source),
        #[cfg(feature = "memchr-control")]
        ScanVariant::CommentMemchr => lex_with::<scan::CommentMemchr>(source),
    }
}

#[doc(hidden)]
pub mod scan {
    use crate::TokenKind;
    use std::simd::{cmp::SimdPartialEq, u8x16};

    /// Block width in bytes for the vector scans.
    pub const LANES: usize = u8x16::LEN;

    pub fn line_comment_len_scalar(bytes: &[u8]) -> usize {
        bytes
            .iter()
            .position(|&b| b == b'\n')
            .unwrap_or(bytes.len())
    }

    /// Bytes before the first LF, or the whole slice.
    pub fn line_comment_len_simd(bytes: &[u8]) -> usize {
        let mut offset = 0;
        let newline = u8x16::splat(b'\n');
        while bytes.len() - offset >= LANES {
            let block = u8x16::from_slice(&bytes[offset..]);
            if let Some(lane) = block.simd_eq(newline).first_set() {
                return offset + lane;
            }
            offset += LANES;
        }
        offset + line_comment_len_scalar(&bytes[offset..])
    }

    fn is_blank(b: u8) -> bool {
        matches!(b, b' ' | b'\t' | b'\r')
    }

    /// Length of the leading run of space, tab and CR.
    pub fn blank_run_len_scalar(bytes: &[u8]) -> usize {
        bytes
            .iter()
            .position(|&b| !is_blank(b))
            .unwrap_or(bytes.len())
    }

    pub fn blank_run_len_simd(bytes: &[u8]) -> usize {
        let mut offset = 0;
        let (sp, tab, cr) = (u8x16::splat(b' '), u8x16::splat(b'\t'), u8x16::splat(b'\r'));
        while bytes.len() - offset >= LANES {
            let block = u8x16::from_slice(&bytes[offset..]);
            let blank = block.simd_eq(sp) | block.simd_eq(tab) | block.simd_eq(cr);
            if let Some(lane) = (!blank).first_set() {
                return offset + lane;
            }
            offset += LANES;
        }
        offset + blank_run_len_scalar(&bytes[offset..])
    }

    /// Single blank (the common token separator) returns after one compare;
    /// longer runs hand over to the vector loop.
    pub fn blank_run_len_hybrid(bytes: &[u8]) -> usize {
        match bytes.get(1) {
            Some(&b) if is_blank(b) => 1 + blank_run_len_simd(&bytes[1..]),
            _ => 1,
        }
    }

    pub(super) trait Scanner {
        /// Blank bytes to skip from a position holding space, tab or CR (>= 1).
        fn blank(bytes: &[u8]) -> usize;
        /// Comment body bytes to skip after `//`.
        fn comment(bytes: &[u8]) -> usize;
        /// Classify the ASCII word `source[start..end]`.
        #[inline(always)]
        fn keyword(source: &str, start: usize, end: usize) -> TokenKind {
            TokenKind::from_word(&source[start..end])
        }
    }
    macro_rules! scanner {
        ($name:ident, $blank:expr, $comment:expr) => {
            pub(super) struct $name;
            impl Scanner for $name {
                #[inline(always)]
                fn blank(bytes: &[u8]) -> usize {
                    $blank(bytes)
                }
                #[inline(always)]
                fn comment(bytes: &[u8]) -> usize {
                    $comment(bytes)
                }
            }
        };
    }
    scanner!(Original, |_: &[u8]| 1, line_comment_len_scalar);
    scanner!(ScalarRuns, blank_run_len_scalar, line_comment_len_scalar);
    scanner!(CommentSimd, |_: &[u8]| 1, line_comment_len_simd);
    scanner!(WhitespaceSimd, blank_run_len_simd, line_comment_len_scalar);
    scanner!(BothSimd, blank_run_len_simd, line_comment_len_simd);
    pub(super) struct CommentSimdHashed;
    impl Scanner for CommentSimdHashed {
        #[inline(always)]
        fn blank(_: &[u8]) -> usize {
            1
        }
        #[inline(always)]
        fn comment(bytes: &[u8]) -> usize {
            line_comment_len_simd(bytes)
        }
        #[inline(always)]
        fn keyword(source: &str, start: usize, end: usize) -> TokenKind {
            TokenKind::from_word_at(source.as_bytes(), start, end - start)
        }
    }
    scanner!(BothHybrid, blank_run_len_hybrid, line_comment_len_simd);
    #[cfg(feature = "memchr-control")]
    scanner!(CommentMemchr, blank_run_len_scalar, |b: &[u8]| {
        memchr::memchr(b'\n', b).unwrap_or(b.len())
    });
    pub(super) type Adopted = CommentSimdHashed;
}

fn lex_with<S: scan::Scanner>(source: &str) -> Result<Lexed<'_>, Diagnostic> {
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
                pos += S::blank(&bytes[pos..]);
                continue;
            }
            b'\n' => {
                pos += 1;
                TokenKind::Newline
            }
            b'/' if bytes.get(pos + 1) == Some(&b'/') => {
                pos += 2;
                pos += S::comment(&bytes[pos..]);
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
                S::keyword(source, start, pos)
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

#[cfg(test)]
mod tests {
    use super::scan::*;
    use super::*;

    const VARIANTS: [ScanVariant; 7] = [
        ScanVariant::CommentSimdHashed,
        ScanVariant::BothHybrid,
        ScanVariant::Original,
        ScanVariant::ScalarRuns,
        ScanVariant::CommentSimd,
        ScanVariant::WhitespaceSimd,
        ScanVariant::BothSimd,
    ];

    fn outcome(source: &str, variant: ScanVariant) -> String {
        match lex_variant(source, variant) {
            Ok(l) => format!(
                "{:?}",
                l.tokens()
                    .iter()
                    .map(|t| (t.kind, t.span, l.text(*t)))
                    .collect::<Vec<_>>()
            ),
            Err(d) => format!("ERR {d:?}"),
        }
    }

    /// Every variant, and the public `lex`, must agree with the original lexer.
    fn assert_same(source: &str) {
        let reference = outcome(source, ScanVariant::Original);
        for v in VARIANTS {
            assert_eq!(reference, outcome(source, v), "{v:?} differs on {source:?}");
        }
        let adopted = match lex(source) {
            Ok(l) => format!(
                "{:?}",
                l.tokens()
                    .iter()
                    .map(|t| (t.kind, t.span, l.text(*t)))
                    .collect::<Vec<_>>()
            ),
            Err(d) => format!("ERR {d:?}"),
        };
        assert_eq!(reference, adopted, "lex differs on {source:?}");
    }

    struct Rng(u64);
    impl Rng {
        fn next(&mut self) -> u64 {
            self.0 ^= self.0 << 13;
            self.0 ^= self.0 >> 7;
            self.0 ^= self.0 << 17;
            self.0
        }
        fn below(&mut self, n: usize) -> usize {
            (self.next() % n as u64) as usize
        }
    }

    #[test]
    fn scanners_agree_on_all_lengths_positions_and_bytes() {
        let probes = [b'\n', b' ', b'\t', b'\r', b'a', b'/', 0x80, 0xff, 0, b'"'];
        for len in 0..=3 * LANES + 1 {
            for fill in *b" x\n\r\t" {
                let base = vec![fill; len];
                assert_eq!(line_comment_len_scalar(&base), line_comment_len_simd(&base));
                assert_eq!(blank_run_len_scalar(&base), blank_run_len_simd(&base));
                if len > 0 {
                    assert_eq!(
                        blank_run_len_hybrid(&base).min(blank_run_len_scalar(&base).max(1)),
                        blank_run_len_scalar(&base).max(1)
                    );
                }
                for at in 0..len {
                    for p in probes {
                        let mut b = base.clone();
                        b[at] = p;
                        assert_eq!(
                            line_comment_len_scalar(&b),
                            line_comment_len_simd(&b),
                            "{b:?}"
                        );
                        assert_eq!(blank_run_len_scalar(&b), blank_run_len_simd(&b), "{b:?}");
                    }
                }
            }
        }
    }

    #[test]
    fn variants_agree_on_boundary_cases() {
        for n in 0..=3 * LANES + 2 {
            for prefix in 0..LANES {
                let pad = " ".repeat(prefix);
                let body = "c".repeat(n);
                let blanks = " ".repeat(n);
                for src in [
                    format!("{pad}//{body}"),
                    format!("{pad}//{body}\nx"),
                    format!("{pad}//{body}\r\nx"),
                    format!("{pad}//{body}\rx\ny"),
                    format!("{pad}//{body}é\nx"),
                    format!("{pad}//{}é{body}\nx", "é".repeat(n % 5)),
                    format!("{pad}x{blanks}y"),
                    format!("{pad}x{blanks}\ny"),
                    format!("{pad}x{blanks}é"),
                    format!("{pad}x{blanks}\r\n\ty{blanks}"),
                    format!("{pad}x{}\ry\r", "\t ".repeat(n)),
                    format!("{pad}\"{body}//{blanks}\" // t\nz"),
                    format!("{pad}x{blanks}\"{body}"),
                    format!("{pad}x{blanks}'é'{blanks}'ab'"),
                    format!("{pad}a /{blanks}/ b //{body}"),
                    format!("{pad}a{blanks}/{blanks}b"),
                    format!("{pad}{blanks}\u{7f}"),
                ] {
                    assert_same(&src);
                }
            }
        }
    }

    #[test]
    fn variants_agree_on_edge_inputs() {
        for src in [
            "",
            " ",
            "\r",
            "\n",
            "//",
            "///",
            "// \r",
            "\r//\r\n",
            "/",
            "/ /",
            "a/=b",
            "a//b",
            "\"//\"",
            "'//'",
            "x \"unterminated // c",
            "é",
            " é",
            "//é",
            "\t\t\t\u{a0}",
            "\r\r\r\n\n\r",
            "// c\n// d\n\n// e",
        ] {
            assert_same(src);
        }
    }

    #[test]
    fn variants_agree_on_random_sources() {
        let frags = [
            " ",
            "  ",
            "\t",
            "\r",
            "\n",
            "\r\n",
            "//",
            "// c ",
            "//é",
            "/",
            "a",
            "bc_9",
            "if",
            "1",
            "2.5",
            "1e",
            "\"s\"",
            "\"//\"",
            "'x'",
            "'é'",
            "'a",
            "\"",
            "é",
            "😀",
            "->",
            "..=",
            "{",
            "}",
            "#",
            "\u{a0}",
            "                ",
            "////////////////////",
            "x",
        ];
        let mut rng = Rng(0x9E3779B97F4A7C15);
        for _ in 0..20_000 {
            let mut s = String::new();
            for _ in 0..rng.below(24) {
                s.push_str(frags[rng.below(frags.len())]);
                if rng.below(6) == 0 {
                    s.push_str(&"c".repeat(rng.below(3 * LANES)));
                }
            }
            assert_same(&s);
        }
    }

    #[test]
    fn variants_agree_on_repository_sources() {
        fn walk(dir: &std::path::Path, out: &mut Vec<std::path::PathBuf>) {
            let Ok(rd) = std::fs::read_dir(dir) else {
                return;
            };
            for e in rd.flatten() {
                let p = e.path();
                if p.is_dir() {
                    walk(&p, out);
                } else if p.extension().is_some_and(|x| x == "ya") {
                    out.push(p);
                }
            }
        }
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
        let mut files = Vec::new();
        for d in ["examples", "std", "tests"] {
            walk(&root.join(d), &mut files);
        }
        files.push(root.join("std.ya"));
        let mut checked = 0;
        for f in files {
            if let Ok(src) = std::fs::read_to_string(&f) {
                assert_same(&src);
                // Also at every byte-prefix alignment relative to vector blocks.
                for pad in 1..LANES {
                    assert_same(&format!("{}{src}", " ".repeat(pad)));
                }
                checked += 1;
            }
        }
        assert!(checked > 0, "no .ya sources found");
    }
}
