//! Resolved operator kinds shared by source syntax and checked IR.
//! Only the parsers translate source spellings into these kinds.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Operator {
    Add,
    Subtract,
    Multiply,
    /// `/`: integer division truncates and is checked; float division is IEEE.
    Divide,
    /// CAIE `/`: both operands become REAL, and a zero divisor is an error.
    RealDivide,
    IntegerDivide,
    Remainder,
    Concat,
    Equal,
    NotEqual,
    Less,
    Greater,
    LessEqual,
    GreaterEqual,
    And,
    Or,
    Not,
    Borrow,
    BorrowMut,
    Dereference,
    Range,
    RangeInclusive,
}
impl Operator {
    pub fn is_comparison(self) -> bool {
        matches!(
            self,
            Self::Equal
                | Self::NotEqual
                | Self::Less
                | Self::Greater
                | Self::LessEqual
                | Self::GreaterEqual
        )
    }
    pub fn is_arithmetic(self) -> bool {
        matches!(
            self,
            Self::Add
                | Self::Subtract
                | Self::Multiply
                | Self::Divide
                | Self::RealDivide
                | Self::IntegerDivide
                | Self::Remainder
        )
    }
}
impl std::fmt::Display for Operator {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Add => "+",
            Self::Subtract => "-",
            Self::Multiply => "*",
            Self::Divide | Self::RealDivide => "/",
            Self::IntegerDivide => "div",
            Self::Remainder => "%",
            Self::Concat | Self::Borrow => "&",
            Self::Equal => "==",
            Self::NotEqual => "!=",
            Self::Less => "<",
            Self::Greater => ">",
            Self::LessEqual => "<=",
            Self::GreaterEqual => ">=",
            Self::And => "&&",
            Self::Or => "||",
            Self::Not => "!",
            Self::BorrowMut => "&mut",
            Self::Dereference => "*",
            Self::Range => "..",
            Self::RangeInclusive => "..=",
        })
    }
}
