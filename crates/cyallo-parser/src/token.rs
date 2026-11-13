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
    Let,
    Gen,
    Yield,
    Const,
    Fn,
    Proc,
    Break,
    Continue,
    Record,
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
        self as u8 >= Self::Let as u8
    }
    pub fn from_word(word: &str) -> Self {
        match word {
            "let" => Self::Let,
            "gen" => Self::Gen,
            "yield" => Self::Yield,
            "const" => Self::Const,
            "fn" => Self::Fn,
            "proc" => Self::Proc,
            "record" => Self::Record,
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
