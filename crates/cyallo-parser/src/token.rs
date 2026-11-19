//! Compact token tags. Text is recovered by span from the borrowed source.
use cyallo_ast::Operator;
use cyallo_ast::{GetSpan, Span};
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum TokenKind {
    Eof,
    Newline,
    Identifier,
    Integer,
    Real,
    Text,
    Char,
    Plus,
    Minus,
    Star,
    Slash,
    Ampersand,
    Equal,
    Less,
    Greater,
    LessEqual,
    GreaterEqual,
    LeftParen,
    RightParen,
    LeftBracket,
    RightBracket,
    Colon,
    Comma,
    Dot,
    LeftBrace,
    RightBrace,
    Semicolon,
    Arrow,
    FatArrow,
    Pipe,
    EqualEqual,
    BangEqual,
    AndAnd,
    OrOr,
    Bang,
    Percent,
    PlusAssign,
    MinusAssign,
    StarAssign,
    SlashAssign,
    Hash,
    At,
    Range,
    RangeInclusive,
    Lifetime,
    Object,
    Dynamic,
    Delete,
    TypeAlias,
    Let,
    Gen,
    Gc,
    Rc,
    Yield,
    Const,
    Fn,
    Proc,
    Break,
    Continue,
    Record,
    Enum,
    Match,
    If,
    Else,
    While,
    For,
    In,
    Return,
    CharType,
    StringType,
    True,
    False,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(C)]
pub struct Token {
    pub span: Span,
    pub kind: TokenKind,
}
impl GetSpan for Token {
    fn span(&self) -> Span {
        self.span
    }
}
impl TokenKind {
    pub fn is_keyword(self) -> bool {
        self as u8 >= Self::Object as u8
    }
    pub fn from_word(word: &str) -> Self {
        match word {
            "object" => Self::Object,
            "dynamic" => Self::Dynamic,
            "delete" => Self::Delete,
            "type" => Self::TypeAlias,
            "let" => Self::Let,
            "gen" => Self::Gen,
            "gc" => Self::Gc,
            "rc" => Self::Rc,
            "yield" => Self::Yield,
            "const" => Self::Const,
            "fn" => Self::Fn,
            "proc" => Self::Proc,
            "record" => Self::Record,
            "enum" => Self::Enum,
            "match" => Self::Match,
            "return" => Self::Return,
            "if" => Self::If,
            "else" => Self::Else,
            "while" => Self::While,
            "for" => Self::For,
            "in" => Self::In,
            "break" => Self::Break,
            "continue" => Self::Continue,
            "true" => Self::True,
            "false" => Self::False,
            "string" => Self::StringType,
            "char" => Self::CharType,
            _ => Self::Identifier,
        }
    }
    /// Keyword lookup by a word of at most 8 bytes packed little-endian and
    /// zero padded. Identifier bytes are never NUL, so the packed value is unique
    /// per spelling. One multiply picks the slot and one compare verifies it.
    #[inline(always)]
    pub(crate) fn from_packed_word(packed: u64) -> Self {
        let slot = (packed.wrapping_mul(KEYWORD_HASH) >> (64 - KEYWORD_SLOT_BITS)) as usize;
        let (key, kind) = KEYWORD_TABLE[slot];
        if key == packed { kind } else { Self::Identifier }
    }
    /// Keyword lookup that reads at most 8 bytes of `bytes` at `start`.
    #[inline(always)]
    pub(crate) fn from_word_at(bytes: &[u8], start: usize, len: usize) -> Self {
        if len > 8 {
            return Self::Identifier;
        }
        let packed = match bytes.get(start..start + 8) {
            Some(w) => {
                let all = u64::from_le_bytes(w.try_into().unwrap());
                if len == 8 { all } else { all & ((1u64 << (8 * len)) - 1) }
            }
            None => {
                let mut buf = [0u8; 8];
                buf[..len].copy_from_slice(&bytes[start..start + len]);
                u64::from_le_bytes(buf)
            }
        };
        Self::from_packed_word(packed)
    }
    pub fn from_spelling(spelling: &str) -> Option<Self> {
        if spelling == "#" {
            return Some(Self::Hash);
        }
        let kind = match spelling {
            "@" => Self::At,
            ".." => Self::Range,
            "..=" => Self::RangeInclusive,
            "." => Self::Dot,
            "{" => Self::LeftBrace,
            "}" => Self::RightBrace,
            ";" => Self::Semicolon,
            "->" => Self::Arrow,
            "=>" => Self::FatArrow,
            "|" => Self::Pipe,
            "==" => Self::EqualEqual,
            "!=" => Self::BangEqual,
            "&&" => Self::AndAnd,
            "||" => Self::OrOr,
            "!" => Self::Bang,
            "%" => Self::Percent,
            "+=" => Self::PlusAssign,
            "-=" => Self::MinusAssign,
            "*=" => Self::StarAssign,
            "/=" => Self::SlashAssign,
            "+" => Self::Plus,
            "-" => Self::Minus,
            "*" => Self::Star,
            "/" => Self::Slash,
            "&" => Self::Ampersand,
            "=" => Self::Equal,
            "<" => Self::Less,
            ">" => Self::Greater,
            "<=" => Self::LessEqual,
            ">=" => Self::GreaterEqual,
            "(" => Self::LeftParen,
            ")" => Self::RightParen,
            "[" => Self::LeftBracket,
            "]" => Self::RightBracket,
            ":" => Self::Colon,
            "," => Self::Comma,
            _ => Self::from_word(spelling),
        };
        (kind != Self::Identifier).then_some(kind)
    }
    pub fn binary_op(self) -> Option<(cyallo_ast::Operator, u8)> {
        Some(match self {
            Self::Range => (Operator::Range, 0),
            Self::RangeInclusive => (Operator::RangeInclusive, 0),
            Self::EqualEqual => (Operator::Equal, 3),
            Self::BangEqual => (Operator::NotEqual, 3),
            Self::AndAnd => (Operator::And, 2),
            Self::OrOr => (Operator::Or, 1),
            Self::Percent => (Operator::Remainder, 6),
            Self::Less => (Operator::Less, 3),
            Self::Greater => (Operator::Greater, 3),
            Self::LessEqual => (Operator::LessEqual, 3),
            Self::GreaterEqual => (Operator::GreaterEqual, 3),
            Self::Plus => (Operator::Add, 5),
            Self::Minus => (Operator::Subtract, 5),
            Self::Star => (Operator::Multiply, 6),
            Self::Slash => (Operator::Divide, 6),
            _ => return None,
        })
    }
}
const _: () = {
    assert!(size_of::<TokenKind>() == 1);
    assert!(size_of::<Token>() == 16);
};

const KEYWORD_SLOT_BITS: u32 = 6;
/// Multiplier found by search: all keywords land in distinct slots.
const KEYWORD_HASH: u64 = 0x87c0_35ef_16b8_b107;
const KEYWORDS: [(&str, TokenKind); 27] = [
    ("object", TokenKind::Object),
    ("dynamic", TokenKind::Dynamic),
    ("delete", TokenKind::Delete),
    ("type", TokenKind::TypeAlias),
    ("let", TokenKind::Let),
    ("gen", TokenKind::Gen),
    ("gc", TokenKind::Gc),
    ("rc", TokenKind::Rc),
    ("yield", TokenKind::Yield),
    ("const", TokenKind::Const),
    ("fn", TokenKind::Fn),
    ("proc", TokenKind::Proc),
    ("record", TokenKind::Record),
    ("enum", TokenKind::Enum),
    ("match", TokenKind::Match),
    ("return", TokenKind::Return),
    ("if", TokenKind::If),
    ("else", TokenKind::Else),
    ("while", TokenKind::While),
    ("for", TokenKind::For),
    ("in", TokenKind::In),
    ("break", TokenKind::Break),
    ("continue", TokenKind::Continue),
    ("true", TokenKind::True),
    ("false", TokenKind::False),
    ("string", TokenKind::StringType),
    ("char", TokenKind::CharType),
];
const fn pack(word: &str) -> u64 {
    let b = word.as_bytes();
    let mut v = 0u64;
    let mut i = 0;
    while i < b.len() {
        v |= (b[i] as u64) << (8 * i);
        i += 1;
    }
    v
}
/// Compile-time table; a slot collision fails the build.
static KEYWORD_TABLE: [(u64, TokenKind); 1 << KEYWORD_SLOT_BITS] = {
    let mut table = [(0u64, TokenKind::Identifier); 1 << KEYWORD_SLOT_BITS];
    let mut i = 0;
    while i < KEYWORDS.len() {
        let packed = pack(KEYWORDS[i].0);
        let slot = (packed.wrapping_mul(KEYWORD_HASH) >> (64 - KEYWORD_SLOT_BITS)) as usize;
        assert!(table[slot].0 == 0, "keyword hash collision");
        table[slot] = (packed, KEYWORDS[i].1);
        i += 1;
    }
    table
};

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hashed_keywords_match_the_match_classifier() {
        for (word, kind) in KEYWORDS {
            assert_eq!(TokenKind::from_word(word), kind);
        }
        // Words near keywords plus every identifier-ish string of small length.
        let mut words: Vec<String> = KEYWORDS
            .iter()
            .flat_map(|(w, _)| {
                [w.to_string(), w.to_uppercase(), format!("{w}_"), format!("{w}x"), w[1..].into(), format!("_{w}")]
            })
            .collect();
        let alphabet = b"aefnotrx_9";
        let mut stack = vec![String::new()];
        while let Some(w) = stack.pop() {
            if !w.is_empty() {
                words.push(w.clone());
            }
            if w.len() < 4 {
                for &c in alphabet {
                    stack.push(format!("{w}{}", c as char));
                }
            }
        }
        for w in words {
            let b = w.as_bytes();
            // Both the in-bounds fast path and the tail path (word at end of input).
            let mut padded = b.to_vec();
            padded.extend_from_slice(b"        ");
            assert_eq!(TokenKind::from_word_at(&padded, 0, b.len()), TokenKind::from_word(&w), "{w}");
            assert_eq!(TokenKind::from_word_at(b, 0, b.len()), TokenKind::from_word(&w), "{w} at end");
        }
    }
}
