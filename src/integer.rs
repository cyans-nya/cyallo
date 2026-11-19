//! Signed integer division semantics shared by the interpreters.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum IntegerError {
    DivisionByZero,
    Overflow,
}

impl std::fmt::Display for IntegerError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::DivisionByZero => "division by zero",
            Self::Overflow => "integer overflow",
        })
    }
}

impl std::error::Error for IntegerError {}

pub(crate) enum IntegerDivision {
    Quotient,
    Remainder,
}

impl IntegerDivision {
    pub(crate) fn evaluate(self, lhs: i64, rhs: i64) -> Result<i64, IntegerError> {
        if rhs == 0 {
            return Err(IntegerError::DivisionByZero);
        }
        match self {
            Self::Quotient => lhs.checked_div(rhs).ok_or(IntegerError::Overflow),
            // The remainder of MIN / -1 is zero even though the quotient
            // cannot be represented. The zero divisor was rejected above.
            Self::Remainder => Ok(lhs.wrapping_rem(rhs)),
        }
    }
}
