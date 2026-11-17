//! Sized numeric values shared by constant evaluation and execution engines.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum NumberError {
    OutOfRange,
    NonFiniteConversion,
    InvalidSyntax,
    InvalidIntegerSyntaxOrRange,
    ConversionOutOfRange,
    UnsignedNegation,
    OperandTypeMismatch,
    InvalidComparison,
    DivisionByZero,
    InvalidFloatingOperation,
    InvalidIntegerOperation,
    IntegerOverflow,
}
impl std::fmt::Display for NumberError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::OutOfRange => "numeric value out of range",
            Self::NonFiniteConversion => "non-finite numeric conversion",
            Self::InvalidSyntax => "invalid numeric syntax",
            Self::InvalidIntegerSyntaxOrRange => "invalid integer syntax or range",
            Self::ConversionOutOfRange => "numeric conversion out of range",
            Self::UnsignedNegation => "cannot negate an unsigned integer",
            Self::OperandTypeMismatch => "numeric operands require identical types",
            Self::InvalidComparison => "invalid comparison",
            Self::DivisionByZero => "division by zero",
            Self::InvalidFloatingOperation => "invalid floating operation",
            Self::InvalidIntegerOperation => "invalid integer operation",
            Self::IntegerOverflow => "integer overflow",
        })
    }
}
impl std::error::Error for NumberError {}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum NumberKind {
    I8,
    I16,
    I32,
    I64,
    U8,
    U16,
    U32,
    U64,
    Isize,
    Usize,
    F32,
    F64,
}
impl NumberKind {
    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "i8" => Self::I8,
            "i16" => Self::I16,
            "i32" => Self::I32,
            "i64" => Self::I64,
            "u8" => Self::U8,
            "u16" => Self::U16,
            "u32" => Self::U32,
            "u64" => Self::U64,
            "isize" => Self::Isize,
            "usize" => Self::Usize,
            "f32" => Self::F32,
            "f64" => Self::F64,
            _ => return None,
        })
    }
    pub fn bits(self) -> u32 {
        match self {
            Self::I8 | Self::U8 => 8,
            Self::I16 | Self::U16 => 16,
            Self::I32 | Self::U32 | Self::F32 => 32,
            Self::Isize | Self::Usize => usize::BITS,
            _ => 64,
        }
    }
    pub fn float(self) -> bool {
        matches!(self, Self::F32 | Self::F64)
    }
    pub fn signed(self) -> bool {
        matches!(
            self,
            Self::I8 | Self::I16 | Self::I32 | Self::I64 | Self::Isize
        )
    }
    pub fn bounds(self) -> (i128, i128) {
        if self.signed() {
            let max = (1i128 << (self.bits() - 1)) - 1;
            (-max - 1, max)
        } else {
            (0, (1i128 << self.bits()) - 1)
        }
    }
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Number {
    pub kind: NumberKind,
    pub bits: u64,
}
impl Number {
    pub fn integer(kind: NumberKind, value: i128) -> Result<Self, NumberError> {
        if kind.float() {
            return Self::floating(kind, value as f64);
        }
        let (min, max) = kind.bounds();
        if value < min || value > max {
            return Err(NumberError::OutOfRange);
        }
        Ok(Self {
            kind,
            bits: value as u64,
        })
    }
    pub fn floating(kind: NumberKind, value: f64) -> Result<Self, NumberError> {
        let n = if kind == NumberKind::F32 {
            value as f32 as f64
        } else {
            value
        };
        if !n.is_finite() {
            return Err(NumberError::NonFiniteConversion);
        }
        Ok(Self {
            kind,
            bits: if kind == NumberKind::F32 {
                (n as f32).to_bits() as u64
            } else {
                n.to_bits()
            },
        })
    }
    /// A float of `kind` holding `value` rounded to that width, infinities and NaN included.
    pub fn ieee(kind: NumberKind, value: f64) -> Self {
        Self {
            kind,
            bits: if kind == NumberKind::F32 {
                (value as f32).to_bits() as u64
            } else {
                value.to_bits()
            },
        }
    }
    pub fn int(self) -> i128 {
        if self.kind.signed() {
            self.bits as i64 as i128
        } else {
            self.bits as i128
        }
    }
    pub fn real(self) -> f64 {
        match self.kind {
            NumberKind::F32 => f32::from_bits(self.bits as u32) as f64,
            NumberKind::F64 => f64::from_bits(self.bits),
            _ => self.int() as f64,
        }
    }
    pub fn parse(kind: NumberKind, text: &str) -> Result<Self, NumberError> {
        if kind.float() {
            Self::floating(kind, text.parse().map_err(|_| NumberError::InvalidSyntax)?)
        } else {
            Self::integer(
                kind,
                text.parse()
                    .map_err(|_| NumberError::InvalidIntegerSyntaxOrRange)?,
            )
        }
    }
    pub fn cast(self, kind: NumberKind) -> Result<Self, NumberError> {
        if kind.float() {
            return Ok(Self::ieee(kind, self.real()));
        }
        if !self.kind.float() {
            return Self::integer(kind, self.int());
        }
        let n = self.real().trunc();
        let (min, max) = kind.bounds();
        // The exclusive upper bound avoids f64 rounding u64::MAX up to 2^64.
        if !n.is_finite() || n < min as f64 || n >= ((max + 1) as f64) {
            return Err(NumberError::ConversionOutOfRange);
        }
        Self::integer(kind, n as i128)
    }
    pub fn negate(self) -> Result<Self, NumberError> {
        if self.kind.float() {
            return Ok(Self::ieee(self.kind, -self.real()));
        }
        if !self.kind.signed() {
            return Err(NumberError::UnsignedNegation);
        }
        Self::integer(self.kind, -self.int())
    }
    pub fn compare(self, other: Self, op: crate::Operator) -> Result<bool, NumberError> {
        if self.kind != other.kind {
            return Err(NumberError::OperandTypeMismatch);
        }
        macro_rules! compare {
            ($a:expr,$b:expr) => {
                match op {
                    crate::Operator::Equal => $a == $b,
                    crate::Operator::NotEqual => $a != $b,
                    crate::Operator::Less => $a < $b,
                    crate::Operator::Greater => $a > $b,
                    crate::Operator::LessEqual => $a <= $b,
                    crate::Operator::GreaterEqual => $a >= $b,
                    _ => return Err(NumberError::InvalidComparison),
                }
            };
        }
        Ok(if self.kind.float() {
            compare!(self.real(), other.real())
        } else {
            compare!(self.int(), other.int())
        })
    }
    /// Float arithmetic follows IEEE 754: zero divisors and overflow produce
    /// infinities or NaN. Integer arithmetic is checked; `/` truncates.
    pub fn binary(self, other: Self, op: crate::Operator) -> Result<Self, NumberError> {
        use crate::Operator;
        if self.kind != other.kind {
            return Err(NumberError::OperandTypeMismatch);
        }
        if self.kind.float() {
            macro_rules! ieee {
                ($a:expr, $b:expr) => {
                    match op {
                        Operator::Add => $a + $b,
                        Operator::Subtract => $a - $b,
                        Operator::Multiply => $a * $b,
                        Operator::Divide => $a / $b,
                        Operator::Remainder => $a % $b,
                        _ => return Err(NumberError::InvalidFloatingOperation),
                    }
                };
            }
            let bits = if self.kind == NumberKind::F32 {
                ieee!(self.real() as f32, other.real() as f32).to_bits() as u64
            } else {
                ieee!(self.real(), other.real()).to_bits()
            };
            return Ok(Self {
                kind: self.kind,
                bits,
            });
        }
        let a = self.int();
        let b = other.int();
        let n = match op {
            Operator::Add => a.checked_add(b),
            Operator::Subtract => a.checked_sub(b),
            Operator::Multiply => a.checked_mul(b),
            Operator::Divide | Operator::IntegerDivide | Operator::Remainder if b == 0 => {
                return Err(NumberError::DivisionByZero);
            }
            Operator::Divide | Operator::IntegerDivide => a.checked_div(b),
            Operator::Remainder => a.checked_rem(b),
            _ => return Err(NumberError::InvalidIntegerOperation),
        };
        // Operands fit in 64 bits, so i128 never overflows here except for
        // u64 products; a result outside the kind's range is an overflow.
        n.and_then(|n| Self::integer(self.kind, n).ok())
            .ok_or(NumberError::IntegerOverflow)
    }
}
impl std::fmt::Display for Number {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self.kind {
            NumberKind::F32 => write!(f, "{}", f32::from_bits(self.bits as u32)),
            NumberKind::F64 => write!(f, "{}", f64::from_bits(self.bits)),
            _ => write!(f, "{}", self.int()),
        }
    }
}
