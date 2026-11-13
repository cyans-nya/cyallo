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
    Assign,
    Plus,
    Minus,
    Star,
    Slash,
    Ampersand,
    Equal,
    NotEqual,
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
    Caret,
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
    Const,
    Fn,
    Proc,
    Print,
    Break,
    Continue,
    Ref,
    Record,
    Declare,
    Constant,
    If,
    Then,
    Else,
    Endif,
    While,
    Endwhile,
    Repeat,
    Until,
    For,
    To,
    Step,
    Next,
    Case,
    Of,
    Otherwise,
    Endcase,
    Procedure,
    Endprocedure,
    Function,
    Endfunction,
    Returns,
    Return,
    Call,
    Input,
    Output,
    IntegerType,
    RealType,
    Boolean,
    CharType,
    StringType,
    Array,
    Byref,
    Byval,
    True,
    False,
    And,
    Or,
    Not,
    Div,
    Mod,
    Type,
    Endtype,
    Class,
    Endclass,
    Date,
    Set,
    Define,
    Openfile,
    Closefile,
    Readfile,
    Writefile,
    Seek,
    Getrecord,
    Putrecord,
    Read,
    Write,
    Append,
    EofKeyword,
    New,
    Private,
    Public,
    Inherits,
    Endmethod,
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
    pub fn modern_keyword(self) -> bool {
        matches!(
            self,
            Self::Let
                | Self::Const
                | Self::Fn
                | Self::Proc
                | Self::Print
                | Self::Break
                | Self::Continue
                | Self::Ref
                | Self::Record
        )
    }
    pub fn modern_operator(self) -> bool {
        matches!(
            self,
            Self::EqualEqual | Self::BangEqual | Self::AndAnd | Self::OrOr | Self::Percent
        )
    }
    pub fn is_keyword(self) -> bool {
        self as u8 >= Self::Let as u8
    }
    /// Case-insensitive classification without allocating an uppercase copy.
    pub fn from_word(word: &str) -> Self {
        for (spelling, kind) in [
            ("let", Self::Let),
            ("const", Self::Const),
            ("fn", Self::Fn),
            ("proc", Self::Proc),
            ("print", Self::Print),
            ("break", Self::Break),
            ("continue", Self::Continue),
            ("ref", Self::Ref),
            ("record", Self::Record),
        ] {
            if word.eq_ignore_ascii_case(spelling) {
                return kind;
            }
        }
        match word.as_bytes().first().map(u8::to_ascii_uppercase) {
            Some(b'A') => {
                if word.eq_ignore_ascii_case("ARRAY") {
                    return Self::Array;
                }
                if word.eq_ignore_ascii_case("AND") {
                    return Self::And;
                }
                if word.eq_ignore_ascii_case("APPEND") {
                    return Self::Append;
                }
            }
            Some(b'B') => {
                if word.eq_ignore_ascii_case("BOOLEAN") {
                    return Self::Boolean;
                }
                if word.eq_ignore_ascii_case("BYREF") {
                    return Self::Byref;
                }
                if word.eq_ignore_ascii_case("BYVAL") {
                    return Self::Byval;
                }
            }
            Some(b'C') => {
                if word.eq_ignore_ascii_case("CONSTANT") {
                    return Self::Constant;
                }
                if word.eq_ignore_ascii_case("CASE") {
                    return Self::Case;
                }
                if word.eq_ignore_ascii_case("CALL") {
                    return Self::Call;
                }
                if word.eq_ignore_ascii_case("CHAR") {
                    return Self::CharType;
                }
                if word.eq_ignore_ascii_case("CLASS") {
                    return Self::Class;
                }
                if word.eq_ignore_ascii_case("CLOSEFILE") {
                    return Self::Closefile;
                }
            }
            Some(b'D') => {
                if word.eq_ignore_ascii_case("DECLARE") {
                    return Self::Declare;
                }
                if word.eq_ignore_ascii_case("DIV") {
                    return Self::Div;
                }
                if word.eq_ignore_ascii_case("DATE") {
                    return Self::Date;
                }
                if word.eq_ignore_ascii_case("DEFINE") {
                    return Self::Define;
                }
            }
            Some(b'E') => {
                if word.eq_ignore_ascii_case("ELSE") {
                    return Self::Else;
                }
                if word.eq_ignore_ascii_case("ENDIF") {
                    return Self::Endif;
                }
                if word.eq_ignore_ascii_case("ENDWHILE") {
                    return Self::Endwhile;
                }
                if word.eq_ignore_ascii_case("ENDCASE") {
                    return Self::Endcase;
                }
                if word.eq_ignore_ascii_case("ENDPROCEDURE") {
                    return Self::Endprocedure;
                }
                if word.eq_ignore_ascii_case("ENDFUNCTION") {
                    return Self::Endfunction;
                }
                if word.eq_ignore_ascii_case("ENDTYPE") {
                    return Self::Endtype;
                }
                if word.eq_ignore_ascii_case("ENDCLASS") {
                    return Self::Endclass;
                }
                if word.eq_ignore_ascii_case("EOF") {
                    return Self::EofKeyword;
                }
                if word.eq_ignore_ascii_case("ENDMETHOD") {
                    return Self::Endmethod;
                }
            }
            Some(b'F') => {
                if word.eq_ignore_ascii_case("FOR") {
                    return Self::For;
                }
                if word.eq_ignore_ascii_case("FUNCTION") {
                    return Self::Function;
                }
                if word.eq_ignore_ascii_case("FALSE") {
                    return Self::False;
                }
            }
            Some(b'G') => {
                if word.eq_ignore_ascii_case("GETRECORD") {
                    return Self::Getrecord;
                }
            }
            Some(b'I') => {
                if word.eq_ignore_ascii_case("IF") {
                    return Self::If;
                }
                if word.eq_ignore_ascii_case("INPUT") {
                    return Self::Input;
                }
                if word.eq_ignore_ascii_case("INTEGER") {
                    return Self::IntegerType;
                }
                if word.eq_ignore_ascii_case("INHERITS") {
                    return Self::Inherits;
                }
            }
            Some(b'M') => {
                if word.eq_ignore_ascii_case("MOD") {
                    return Self::Mod;
                }
            }
            Some(b'N') => {
                if word.eq_ignore_ascii_case("NEXT") {
                    return Self::Next;
                }
                if word.eq_ignore_ascii_case("NOT") {
                    return Self::Not;
                }
                if word.eq_ignore_ascii_case("NEW") {
                    return Self::New;
                }
            }
            Some(b'O') => {
                if word.eq_ignore_ascii_case("OF") {
                    return Self::Of;
                }
                if word.eq_ignore_ascii_case("OTHERWISE") {
                    return Self::Otherwise;
                }
                if word.eq_ignore_ascii_case("OUTPUT") {
                    return Self::Output;
                }
                if word.eq_ignore_ascii_case("OR") {
                    return Self::Or;
                }
                if word.eq_ignore_ascii_case("OPENFILE") {
                    return Self::Openfile;
                }
            }
            Some(b'P') => {
                if word.eq_ignore_ascii_case("PROCEDURE") {
                    return Self::Procedure;
                }
                if word.eq_ignore_ascii_case("PUTRECORD") {
                    return Self::Putrecord;
                }
                if word.eq_ignore_ascii_case("PRIVATE") {
                    return Self::Private;
                }
                if word.eq_ignore_ascii_case("PUBLIC") {
                    return Self::Public;
                }
            }
            Some(b'R') => {
                if word.eq_ignore_ascii_case("REPEAT") {
                    return Self::Repeat;
                }
                if word.eq_ignore_ascii_case("RETURNS") {
                    return Self::Returns;
                }
                if word.eq_ignore_ascii_case("RETURN") {
                    return Self::Return;
                }
                if word.eq_ignore_ascii_case("REAL") {
                    return Self::RealType;
                }
                if word.eq_ignore_ascii_case("READFILE") {
                    return Self::Readfile;
                }
                if word.eq_ignore_ascii_case("READ") {
                    return Self::Read;
                }
            }
            Some(b'S') => {
                if word.eq_ignore_ascii_case("STEP") {
                    return Self::Step;
                }
                if word.eq_ignore_ascii_case("STRING") {
                    return Self::StringType;
                }
                if word.eq_ignore_ascii_case("SET") {
                    return Self::Set;
                }
                if word.eq_ignore_ascii_case("SEEK") {
                    return Self::Seek;
                }
            }
            Some(b'T') => {
                if word.eq_ignore_ascii_case("THEN") {
                    return Self::Then;
                }
                if word.eq_ignore_ascii_case("TO") {
                    return Self::To;
                }
                if word.eq_ignore_ascii_case("TRUE") {
                    return Self::True;
                }
                if word.eq_ignore_ascii_case("TYPE") {
                    return Self::Type;
                }
            }
            Some(b'U') => {
                if word.eq_ignore_ascii_case("UNTIL") {
                    return Self::Until;
                }
            }
            Some(b'W') => {
                if word.eq_ignore_ascii_case("WHILE") {
                    return Self::While;
                }
                if word.eq_ignore_ascii_case("WRITEFILE") {
                    return Self::Writefile;
                }
                if word.eq_ignore_ascii_case("WRITE") {
                    return Self::Write;
                }
            }
            _ => {}
        }
        Self::Identifier
    }
    pub fn from_spelling(spelling: &str) -> Option<Self> {
        if spelling == "#" {
            return Some(Self::Hash);
        }
        let kind = match spelling {
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
            "<-" => Self::Assign,
            "+" => Self::Plus,
            "-" => Self::Minus,
            "*" => Self::Star,
            "/" => Self::Slash,
            "&" => Self::Ampersand,
            "=" => Self::Equal,
            "<>" => Self::NotEqual,
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
            "^" => Self::Caret,
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
            Self::Or => (Operator::Or, 1),
            Self::And => (Operator::And, 2),
            Self::Equal => (Operator::Equal, 3),
            Self::NotEqual => (Operator::NotEqual, 3),
            Self::Less => (Operator::Less, 3),
            Self::Greater => (Operator::Greater, 3),
            Self::LessEqual => (Operator::LessEqual, 3),
            Self::GreaterEqual => (Operator::GreaterEqual, 3),
            Self::Ampersand => (Operator::Concat, 4),
            Self::Plus => (Operator::Add, 5),
            Self::Minus => (Operator::Subtract, 5),
            Self::Star => (Operator::Multiply, 6),
            Self::Slash => (Operator::RealDivide, 6),
            Self::Div => (Operator::IntegerDivide, 6),
            Self::Mod => (Operator::Remainder, 6),
            _ => return None,
        })
    }
}
const _: () = {
    assert!(size_of::<TokenKind>() == 1);
    assert!(size_of::<Token>() == 16);
};
