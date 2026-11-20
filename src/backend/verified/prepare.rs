//! Legalize verified scalar operations and classify their ABI homes.

use super::{AbiLayout, ByteOffset, Failure, NativeTarget, Relocation, RelocationKind};
use crate::{
    Span,
    bytecode::verified::{
        BooleanOp, CallSiteId, CallTarget, FloatOp, FunctionId, Instruction, InstructionId,
        IntegerOp, Register, RegisterType, Scalar, ScalarType, Shape, StorageState, ValueType,
        VerifiedFunction, VerifiedModule,
    },
};
use std::{collections::BTreeMap, fmt};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NativeHome {
    pub register: Register,
    pub offset: ByteOffset,
    /// The scalar type of a one-word home. Composite homes carry `I64` here
    /// and describe their words through `layout`.
    pub ty: ScalarType,
    /// The layout of a composite home, which spans `Layout::words` words
    /// starting at `offset`.
    pub layout: Option<LayoutId>,
}

/// An entry in the plan's layout table.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LayoutId(pub u32);

/// How a value is spread over 64-bit words: scalars take one word, a tuple
/// its fields in order, and an enum a tag word followed by words shared by
/// the variants' payloads.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Layout {
    Scalar(ScalarType),
    Tuple(Box<[Layout]>),
    Enum(Box<[Option<Layout>]>),
}

impl Layout {
    pub fn words(&self) -> u32 {
        match self {
            Self::Scalar(_) => 1,
            Self::Tuple(fields) => fields.iter().map(Layout::words).sum(),
            Self::Enum(variants) => {
                1 + variants
                    .iter()
                    .flatten()
                    .map(Layout::words)
                    .max()
                    .unwrap_or(0)
            }
        }
    }

    /// Words before field `index` of a tuple.
    pub fn field_words(&self, index: usize) -> Option<u32> {
        let Self::Tuple(fields) = self else {
            return None;
        };
        (index < fields.len()).then(|| fields[..index].iter().map(Layout::words).sum())
    }
}

/// One 64-bit move within the frame, by byte offset.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WordOp {
    Copy {
        destination: ByteOffset,
        source: ByteOffset,
    },
    Constant {
        destination: ByteOffset,
        value: u64,
    },
}

/// A run of `WordOp`s in the plan's word table.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WordRange {
    pub start: u32,
    pub len: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CodeVersion(pub u64);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FunctionVersion {
    pub function: FunctionId,
    pub version: CodeVersion,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Origin {
    pub function: FunctionId,
    pub version: CodeVersion,
    pub instruction: InstructionId,
    pub span: Span,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Operation {
    StorageLive(NativeHome),
    StorageDead(NativeHome),
    Constant {
        destination: NativeHome,
        value: Scalar,
    },
    Copy {
        destination: NativeHome,
        source: NativeHome,
    },
    Integer {
        operation: IntegerOp,
        destination: NativeHome,
        left: NativeHome,
        right: NativeHome,
    },
    Float {
        operation: FloatOp,
        destination: NativeHome,
        left: NativeHome,
        right: NativeHome,
    },
    Boolean {
        operation: BooleanOp,
        destination: NativeHome,
        left: NativeHome,
        right: NativeHome,
    },
    /// Integer operands divided as binary64, as CAIE `/`; a zero divisor fails.
    RealDivide {
        destination: NativeHome,
        left: NativeHome,
        right: NativeHome,
    },
    Unary {
        operation: UnaryOp,
        destination: NativeHome,
        source: NativeHome,
    },
    /// Composite moves, built from word operations.
    Words(WordRange),
    /// Whether the enum with this tag word holds the variant.
    TagIs {
        destination: NativeHome,
        tag: ByteOffset,
        variant: u32,
    },
    /// Fail unless the tag word names the variant, then run the word moves.
    Variant {
        tag: ByteOffset,
        variant: u32,
        then: WordRange,
    },
    Jump(InstructionId),
    Branch {
        condition: NativeHome,
        yes: InstructionId,
        no: InstructionId,
    },
    Return(Option<NativeHome>),
    Call(InstructionId),
    Trap,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UnaryOp {
    /// Checked: the minimum integer overflows.
    NegateInteger,
    NegateFloat,
    Not,
    IntegerToFloat,
    /// Truncates toward zero; a value outside the `i64` range or NaN fails.
    FloatToInteger,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Site {
    pub origin: Origin,
    pub operation: Operation,
    pub successor: Option<InstructionId>,
    pub fuel: u64,
    pub recover: Box<[RecoverySlot]>,
    pub failures: &'static [Failure],
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RecoverySlot {
    Uninitialized(NativeHome),
    Initialized(NativeHome),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CallArgument {
    pub caller: NativeHome,
    pub callee: NativeHome,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Call {
    pub target: FunctionVersion,
    pub arguments: Box<[CallArgument]>,
    pub destination: Option<NativeHome>,
    pub result: Option<ScalarType>,
    /// The layout of a composite result, in place of `result`.
    pub result_layout: Option<Layout>,
    pub successor: InstructionId,
    pub link: Relocation,
}

const INSTRUCTION_FUEL: u64 = 1;

#[derive(Debug)]
pub struct PreparedFunction {
    target: NativeTarget,
    function: FunctionId,
    version: CodeVersion,
    arguments: Box<[NativeHome]>,
    result: Option<ScalarType>,
    result_layout: Option<Layout>,
    frame_bytes: u32,
    layouts: Box<[Layout]>,
    words: Box<[WordOp]>,
    sites: Box<[Site]>,
    calls: BTreeMap<InstructionId, Call>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PrepareError {
    UnknownFunction(FunctionId),
    MissingHome(Register),
    NonScalar(Register),
    FrameTooLarge,
    InstructionOverflow,
    ConditionalInitialization {
        origin: Origin,
        register: Register,
    },
    UnsupportedOperation {
        origin: Origin,
        instruction: Instruction,
    },
    NonScalarResult(FunctionId),
}

impl fmt::Display for PrepareError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnknownFunction(function) => {
                write!(f, "unknown verified function {}", function.0)
            }
            Self::MissingHome(register) => {
                write!(f, "register {} has no scalar frame home", register.0)
            }
            Self::NonScalar(register) => write!(
                f,
                "register {} requires a non-scalar native layout",
                register.0
            ),
            Self::NonScalarResult(function) => {
                write!(f, "function {} returns a composite value", function.0)
            }
            Self::FrameTooLarge => f.write_str("scalar frame exceeds the native ABI offset range"),
            Self::InstructionOverflow => {
                f.write_str("instruction index exceeds the native ABI range")
            }
            Self::ConditionalInitialization { origin, register } => write!(
                f,
                "register {} has path-dependent initialization at instruction {}",
                register.0, origin.instruction.0
            ),
            Self::UnsupportedOperation {
                origin,
                instruction,
            } => write!(
                f,
                "unsupported native operation {instruction:?} in function {} at instruction {}",
                origin.function.0, origin.instruction.0
            ),
        }
    }
}

impl std::error::Error for PrepareError {}

impl PreparedFunction {
    pub fn prepare(
        module: &VerifiedModule,
        function: FunctionId,
        version: CodeVersion,
        target: NativeTarget,
    ) -> Result<Self, PrepareError> {
        let body = module
            .function(function)
            .ok_or(PrepareError::UnknownFunction(function))?;
        let mut map = FrameMap::new(body)?;
        let frame_bytes = map.frame_bytes;
        let arguments = body
            .parameters()
            .iter()
            .map(|&reg| map.home(reg))
            .collect::<Result<_, _>>()?;
        let mut sites = Vec::new();
        let mut calls = BTreeMap::new();
        body.visit_storage(|pc, state| -> Result<(), PrepareError> {
            let index = usize::try_from(pc.0).map_err(|_| PrepareError::InstructionOverflow)?;
            let instruction = body.instructions()[index];
            let origin = Origin {
                function,
                version,
                instruction: pc,
                span: body.spans()[index],
            };
            let successor = match instruction {
                Instruction::Branch(..)
                | Instruction::Jump(..)
                | Instruction::Return(..)
                | Instruction::Trap => None,
                _ => Some(InstructionId(
                    pc.0.checked_add(1)
                        .ok_or(PrepareError::InstructionOverflow)?,
                )),
            };
            if let (Instruction::Call(id), Some(next)) = (instruction, successor) {
                let link_offset = frame_offset(calls.len())?;
                calls.insert(
                    pc,
                    prepare_call(module, &mut map, id, origin, next, link_offset)?,
                );
            }
            let operation = legalize(&mut map, instruction, origin)?;
            let recover = recovery_map(&mut map, state, origin)?;
            sites.push(Site {
                origin,
                operation,
                successor,
                fuel: INSTRUCTION_FUEL,
                recover,
                failures: failures(operation),
            });
            Ok(())
        })?;
        let (result, result_layout) = result_kind(body)?;
        Ok(Self {
            target,
            function,
            version,
            arguments,
            result,
            result_layout,
            frame_bytes,
            layouts: map.layouts.into_boxed_slice(),
            words: map.words.into_boxed_slice(),
            sites: sites.into_boxed_slice(),
            calls,
        })
    }

    pub fn target(&self) -> NativeTarget {
        self.target
    }

    pub fn function(&self) -> FunctionId {
        self.function
    }

    pub fn version(&self) -> CodeVersion {
        self.version
    }

    pub fn call(&self, pc: InstructionId) -> Option<&Call> {
        self.calls.get(&pc)
    }

    pub fn relocations(&self) -> impl ExactSizeIterator<Item = Relocation> + '_ {
        self.calls.values().map(|call| call.link)
    }

    pub fn abi(&self) -> AbiLayout {
        AbiLayout::SCALARS
    }

    pub fn arguments(&self) -> &[NativeHome] {
        &self.arguments
    }

    pub fn result(&self) -> Option<ScalarType> {
        self.result
    }

    pub fn frame_bytes(&self) -> u32 {
        self.frame_bytes
    }

    pub fn result_layout(&self) -> Option<&Layout> {
        self.result_layout.as_ref()
    }

    pub fn layout(&self, id: LayoutId) -> Option<&Layout> {
        self.layouts.get(usize::try_from(id.0).ok()?)
    }

    pub fn words(&self, range: WordRange) -> &[WordOp] {
        let start = range.start as usize;
        &self.words[start..start + range.len as usize]
    }

    pub fn sites(&self) -> &[Site] {
        &self.sites
    }

    pub fn site(&self, instruction: InstructionId) -> Option<&Site> {
        let index = self
            .sites
            .binary_search_by_key(&instruction, |site| site.origin.instruction)
            .ok()?;
        self.sites.get(index)
    }
}

/// The scalar or composite result of a function.
fn result_kind(
    body: &VerifiedFunction,
) -> Result<(Option<ScalarType>, Option<Layout>), PrepareError> {
    let Some(ty) = body.result() else {
        return Ok((None, None));
    };
    match ty {
        ValueType::Scalar(scalar) => Ok((Some(scalar), None)),
        other => match layout_of(body, other) {
            Some(layout) => Ok((None, Some(layout))),
            None => Err(PrepareError::NonScalarResult(FunctionId(0))),
        },
    }
}

/// The word layout of a value type, if it is made of scalars, tuples and enums.
fn layout_of(body: &VerifiedFunction, ty: ValueType) -> Option<Layout> {
    match ty {
        ValueType::Scalar(ScalarType::U64) => None,
        ValueType::Scalar(scalar) => Some(Layout::Scalar(scalar)),
        ValueType::Composite(id) => match body.shape_table().get(usize::try_from(id.0).ok()?)? {
            Shape::Tuple(fields) => fields
                .iter()
                .map(|&field| layout_of(body, field))
                .collect::<Option<Box<[_]>>>()
                .map(Layout::Tuple),
            Shape::Enum(variants) => variants
                .iter()
                .map(|variant| match variant {
                    Some(ty) => layout_of(body, *ty).map(Some),
                    None => Some(None),
                })
                .collect::<Option<Box<[_]>>>()
                .map(Layout::Enum),
            _ => None,
        },
        _ => None,
    }
}

/// Placement of one function's registers in its native frame, and the layout
/// and word tables the plan keeps.
struct FrameMap<'a> {
    body: &'a VerifiedFunction,
    /// Byte offset of each frame slot. Registers that share a slot share its
    /// words, so a slot spans its largest occupant.
    slot_offsets: Vec<u32>,
    frame_bytes: u32,
    layouts: Vec<Layout>,
    words: Vec<WordOp>,
}

impl<'a> FrameMap<'a> {
    fn new(body: &'a VerifiedFunction) -> Result<Self, PrepareError> {
        let frame = body.frame_layout();
        let mut sizes = vec![1u32; frame.slot_count()];
        let mut layouts: Vec<Layout> = Vec::new();
        for (index, kind) in body.registers().iter().enumerate() {
            let Some(slot) = u32::try_from(index)
                .ok()
                .and_then(|index| frame.slot(Register(index)))
            else {
                continue;
            };
            if let RegisterType::Composite(id) = *kind
                && let Some(layout) = layout_of(body, ValueType::Composite(id))
            {
                sizes[slot.index()] = sizes[slot.index()].max(layout.words());
                if !layouts.contains(&layout) {
                    layouts.push(layout);
                }
            }
        }
        let mut slot_offsets = Vec::with_capacity(sizes.len());
        let mut next = 0u32;
        for &words in &sizes {
            slot_offsets.push(next.checked_mul(8).ok_or(PrepareError::FrameTooLarge)?);
            next = next.checked_add(words).ok_or(PrepareError::FrameTooLarge)?;
        }
        Ok(Self {
            body,
            slot_offsets,
            frame_bytes: next.checked_mul(8).ok_or(PrepareError::FrameTooLarge)?,
            layouts,
            words: Vec::new(),
        })
    }

    fn intern(&mut self, layout: Layout) -> LayoutId {
        let index = self
            .layouts
            .iter()
            .position(|known| *known == layout)
            .unwrap_or_else(|| {
                self.layouts.push(layout);
                self.layouts.len() - 1
            });
        LayoutId(index as u32)
    }

    fn layout_of_register(&self, register: Register) -> Result<Layout, PrepareError> {
        let index = usize::try_from(register.0).map_err(|_| PrepareError::MissingHome(register))?;
        match self.body.registers().get(index).copied() {
            Some(RegisterType::Scalar(ty)) => {
                layout_of(self.body, ValueType::Scalar(ty)).ok_or(PrepareError::NonScalar(register))
            }
            Some(RegisterType::Composite(id)) => layout_of(self.body, ValueType::Composite(id))
                .ok_or(PrepareError::NonScalar(register)),
            _ => Err(PrepareError::NonScalar(register)),
        }
    }

    fn home(&mut self, register: Register) -> Result<NativeHome, PrepareError> {
        let layout = self.layout_of_register(register)?;
        let slot = self
            .body
            .frame_layout()
            .slot(register)
            .ok_or(PrepareError::MissingHome(register))?;
        let offset = ByteOffset(self.slot_offsets[slot.index()]);
        Ok(match layout {
            Layout::Scalar(ty) => NativeHome {
                register,
                offset,
                ty,
                layout: None,
            },
            composite => NativeHome {
                register,
                offset,
                ty: ScalarType::I64,
                layout: Some(self.intern(composite)),
            },
        })
    }

    fn layout_of_home(&self, home: NativeHome) -> Option<Layout> {
        self.layouts.get(home.layout?.0 as usize).cloned()
    }

    fn words_of(&self, home: NativeHome) -> u32 {
        home.layout
            .and_then(|id| self.layouts.get(id.0 as usize))
            .map_or(1, Layout::words)
    }

    /// Record word operations and return their range.
    fn push(&mut self, ops: impl IntoIterator<Item = WordOp>) -> Result<WordRange, PrepareError> {
        let start = u32::try_from(self.words.len()).map_err(|_| PrepareError::FrameTooLarge)?;
        self.words.extend(ops);
        let end = u32::try_from(self.words.len()).map_err(|_| PrepareError::FrameTooLarge)?;
        Ok(WordRange {
            start,
            len: end - start,
        })
    }

    /// Copy `count` words from `source` (a byte offset) to `destination`.
    fn copies(destination: u32, source: u32, count: u32) -> impl Iterator<Item = WordOp> {
        (0..count).map(move |word| WordOp::Copy {
            destination: ByteOffset(destination + word * 8),
            source: ByteOffset(source + word * 8),
        })
    }

    fn overlap(&self, left: NativeHome, right: NativeHome) -> bool {
        let (a, b) = (left.offset.0, right.offset.0);
        let (la, lb) = (self.words_of(left) * 8, self.words_of(right) * 8);
        a < b + lb && b < a + la
    }
}

fn frame_offset(index: usize) -> Result<ByteOffset, PrepareError> {
    index
        .checked_mul(AbiLayout::SCALARS.slot_bytes)
        .and_then(|bytes| u32::try_from(bytes).ok())
        .map(ByteOffset)
        .ok_or(PrepareError::FrameTooLarge)
}

fn recovery_map(
    map: &mut FrameMap<'_>,
    state: &[StorageState],
    origin: Origin,
) -> Result<Box<[RecoverySlot]>, PrepareError> {
    let mut slots = Vec::new();
    for (index, &value) in state.iter().enumerate() {
        if value == StorageState::Dead {
            continue;
        }
        let register = Register(u32::try_from(index).map_err(|_| PrepareError::FrameTooLarge)?);
        let slot = map.home(register)?;
        slots.push(match value {
            StorageState::Initialized => RecoverySlot::Initialized(slot),
            StorageState::Uninitialized => RecoverySlot::Uninitialized(slot),
            StorageState::MaybeInitialized => {
                return Err(PrepareError::ConditionalInitialization { origin, register });
            }
            StorageState::Dead => continue,
        });
    }
    Ok(slots.into_boxed_slice())
}

fn prepare_call(
    module: &VerifiedModule,
    map: &mut FrameMap<'_>,
    id: CallSiteId,
    origin: Origin,
    successor: InstructionId,
    offset: ByteOffset,
) -> Result<Call, PrepareError> {
    let index = usize::try_from(id.0).map_err(|_| PrepareError::InstructionOverflow)?;
    let unsupported = || PrepareError::UnsupportedOperation {
        origin,
        instruction: Instruction::Call(id),
    };
    let call = map.body.calls().get(index).ok_or_else(unsupported)?;
    let CallTarget::Direct(function) = call.target else {
        return Err(unsupported());
    };
    let callee = module
        .function(function)
        .ok_or(PrepareError::UnknownFunction(function))?;
    let mut callee_map = FrameMap::new(callee)?;
    let arguments = call
        .arguments
        .iter()
        .zip(callee.parameters())
        .map(|(&arg, &param)| {
            Ok(CallArgument {
                caller: map.home(arg)?,
                callee: callee_map.home(param)?,
            })
        })
        .collect::<Result<_, PrepareError>>()?;
    let destination = call
        .destination
        .map(|register| map.home(register))
        .transpose()?;
    let (result, result_layout) = result_kind(callee).map_err(|error| match error {
        PrepareError::NonScalarResult(_) => PrepareError::NonScalarResult(function),
        other => other,
    })?;
    let target = FunctionVersion {
        function,
        version: origin.version,
    };
    Ok(Call {
        target,
        arguments,
        destination,
        result,
        result_layout,
        successor,
        link: Relocation {
            instruction: origin.instruction,
            offset,
            kind: RelocationKind::FunctionAddress,
            target,
            addend: 0,
        },
    })
}

fn failures(operation: Operation) -> &'static [Failure] {
    use IntegerOp::*;
    match operation {
        Operation::Integer {
            operation: Add | Subtract | Multiply,
            ..
        } => &[Failure::Overflow],
        Operation::Integer {
            operation: Divide, ..
        } => &[Failure::DivisionByZero, Failure::Overflow],
        Operation::Integer {
            operation: Remainder,
            ..
        } => &[Failure::DivisionByZero],
        Operation::Float {
            operation: FloatOp::CheckedDivide,
            ..
        }
        | Operation::RealDivide { .. } => &[Failure::DivisionByZero],
        Operation::Unary {
            operation: UnaryOp::NegateInteger,
            ..
        } => &[Failure::Overflow],
        Operation::Unary {
            operation: UnaryOp::FloatToInteger,
            ..
        } => &[Failure::ConversionOutOfRange],
        Operation::Variant { .. } => &[Failure::EnumVariant],
        Operation::Trap => &[Failure::Trap],
        _ => &[],
    }
}

fn legalize(
    map: &mut FrameMap<'_>,
    instruction: Instruction,
    origin: Origin,
) -> Result<Operation, PrepareError> {
    let unsupported = PrepareError::UnsupportedOperation {
        origin,
        instruction,
    };
    Ok(match instruction {
        Instruction::StorageLive(reg) => Operation::StorageLive(map.home(reg)?),
        Instruction::StorageDead(reg) => Operation::StorageDead(map.home(reg)?),
        Instruction::Constant(reg, value) => Operation::Constant {
            destination: map.home(reg)?,
            value,
        },
        Instruction::Copy(dst, src) | Instruction::Move(dst, src) => {
            let (destination, source) = (map.home(dst)?, map.home(src)?);
            if destination.layout.is_some() {
                if destination.layout != source.layout
                    && map.words_of(destination) != map.words_of(source)
                {
                    return Err(unsupported);
                }
                if map.overlap(destination, source) && destination.offset != source.offset {
                    return Err(unsupported);
                }
                let count = map.words_of(destination);
                Operation::Words(map.push(FrameMap::copies(
                    destination.offset.0,
                    source.offset.0,
                    count,
                ))?)
            } else if matches!(instruction, Instruction::Move(..)) && destination.ty != source.ty {
                return Err(unsupported);
            } else {
                Operation::Copy {
                    destination,
                    source,
                }
            }
        }
        Instruction::Tuple(dst, list) => {
            let destination = map.home(dst)?;
            let Some(layout) = map.layout_of_home(destination) else {
                return Err(unsupported);
            };
            let mut ops = Vec::new();
            for (index, &operand) in map.body.lists()[list.0 as usize].iter().enumerate() {
                let source = map.home(operand)?;
                let Some(offset) = layout.field_words(index) else {
                    return Err(unsupported);
                };
                if map.overlap(destination, source) {
                    return Err(unsupported);
                }
                ops.extend(FrameMap::copies(
                    destination.offset.0 + offset * 8,
                    source.offset.0,
                    map.words_of(source),
                ));
            }
            Operation::Words(map.push(ops)?)
        }
        Instruction::Extract(dst, src, index) => {
            let (destination, source) = (map.home(dst)?, map.home(src)?);
            let Some(layout) = map.layout_of_home(source) else {
                return Err(unsupported);
            };
            let Some(offset) = layout.field_words(index as usize) else {
                return Err(unsupported);
            };
            if map.overlap(destination, source) {
                return Err(unsupported);
            }
            let count = map.words_of(destination);
            Operation::Words(map.push(FrameMap::copies(
                destination.offset.0,
                source.offset.0 + offset * 8,
                count,
            ))?)
        }
        Instruction::EnumUnit(dst, variant) => {
            let destination = map.home(dst)?;
            Operation::Words(map.push([WordOp::Constant {
                destination: destination.offset,
                value: u64::from(variant),
            }])?)
        }
        Instruction::EnumWrap(dst, variant, src) => {
            let (destination, source) = (map.home(dst)?, map.home(src)?);
            if map.overlap(destination, source) {
                return Err(unsupported);
            }
            let mut ops = vec![WordOp::Constant {
                destination: destination.offset,
                value: u64::from(variant),
            }];
            ops.extend(FrameMap::copies(
                destination.offset.0 + 8,
                source.offset.0,
                map.words_of(source),
            ));
            Operation::Words(map.push(ops)?)
        }
        Instruction::EnumIs(dst, src, variant) => Operation::TagIs {
            destination: map.home(dst)?,
            tag: map.home(src)?.offset,
            variant,
        },
        Instruction::EnumExpect(src, variant) => Operation::Variant {
            tag: map.home(src)?.offset,
            variant,
            then: map.push([])?,
        },
        Instruction::EnumPayload(dst, src, variant) => {
            let (destination, source) = (map.home(dst)?, map.home(src)?);
            if map.overlap(destination, source) {
                return Err(unsupported);
            }
            let count = map.words_of(destination);
            Operation::Variant {
                tag: source.offset,
                variant,
                then: map.push(FrameMap::copies(
                    destination.offset.0,
                    source.offset.0 + 8,
                    count,
                ))?,
            }
        }
        Instruction::Integer(operation, dst, lhs, rhs) => Operation::Integer {
            operation,
            destination: map.home(dst)?,
            left: map.home(lhs)?,
            right: map.home(rhs)?,
        },
        Instruction::Float(operation, dst, lhs, rhs) => {
            if operation == FloatOp::Remainder {
                // No native floating-point remainder instruction.
                return Err(PrepareError::UnsupportedOperation {
                    origin,
                    instruction,
                });
            }
            Operation::Float {
                operation,
                destination: map.home(dst)?,
                left: map.home(lhs)?,
                right: map.home(rhs)?,
            }
        }
        Instruction::Boolean(operation, dst, lhs, rhs) => Operation::Boolean {
            operation,
            destination: map.home(dst)?,
            left: map.home(lhs)?,
            right: map.home(rhs)?,
        },
        Instruction::RealDivide(dst, lhs, rhs) => Operation::RealDivide {
            destination: map.home(dst)?,
            left: map.home(lhs)?,
            right: map.home(rhs)?,
        },
        Instruction::NegateInteger(dst, src) => Operation::Unary {
            operation: UnaryOp::NegateInteger,
            destination: map.home(dst)?,
            source: map.home(src)?,
        },
        Instruction::NegateFloat(dst, src) => Operation::Unary {
            operation: UnaryOp::NegateFloat,
            destination: map.home(dst)?,
            source: map.home(src)?,
        },
        Instruction::Not(dst, src) => Operation::Unary {
            operation: UnaryOp::Not,
            destination: map.home(dst)?,
            source: map.home(src)?,
        },
        Instruction::IntegerToFloat(dst, src) => Operation::Unary {
            operation: UnaryOp::IntegerToFloat,
            destination: map.home(dst)?,
            source: map.home(src)?,
        },
        Instruction::FloatToInteger(dst, src) => Operation::Unary {
            operation: UnaryOp::FloatToInteger,
            destination: map.home(dst)?,
            source: map.home(src)?,
        },
        Instruction::Jump(to) => Operation::Jump(to),
        Instruction::Branch(condition, yes, no) => Operation::Branch {
            condition: map.home(condition)?,
            yes,
            no,
        },
        Instruction::Return(value) => {
            Operation::Return(value.map(|register| map.home(register)).transpose()?)
        }
        Instruction::Trap => Operation::Trap,
        Instruction::Call(_) => Operation::Call(origin.instruction),
        Instruction::Borrow(..)
        | Instruction::ReadLoan(..)
        | Instruction::WriteLoan(..)
        | Instruction::CheckStep(..)
        | Instruction::ObjectNew(..)
        | Instruction::ObjectGet(..)
        | Instruction::ObjectGetKey(..)
        | Instruction::ObjectSet(..)
        | Instruction::ObjectSetKey(..)
        | Instruction::ObjectDelete(..)
        | Instruction::ObjectDeleteKey(..)
        | Instruction::ObjectView(..)
        | Instruction::ObjectFreeze(..)
        | Instruction::ObjectTryView(..)
        | Instruction::ModuleLoad(..)
        | Instruction::FailureFlag(..)
        | Instruction::FailureMessage(..)
        | Instruction::DynamicInvoke(..)
        | Instruction::Erase(..)
        | Instruction::FrozenValue(..)
        | Instruction::NativeCallableConst(..)
        | Instruction::Random(..)
        | Instruction::ServiceContext(..)
        | Instruction::ServiceExtend(..)
        | Instruction::ServiceIsolate(..)
        | Instruction::ServiceKeyConst(..)
        | Instruction::ServiceReference(..)
        | Instruction::ServiceProvide(..)
        | Instruction::ServiceAvailable(..)
        | Instruction::ServiceInject(..)
        | Instruction::ServiceOnDispose(..)
        | Instruction::ServiceDispose(..)
        | Instruction::Count(..)
        | Instruction::AnyErase(..)
        | Instruction::AnyChecked(..)
        | Instruction::DynamicCast(..)
        | Instruction::DynamicBinary(..)
        | Instruction::DynamicUnary(..)
        | Instruction::TaskSpawn(..)
        | Instruction::TaskJoin(..)
        | Instruction::TaskCancel(..)
        | Instruction::TaskIsFinished(..)
        | Instruction::ClockNew(..)
        | Instruction::ClockClone(..)
        | Instruction::ClockSleep(..)
        | Instruction::TextConstant(..)
        | Instruction::TextConcat(..)
        | Instruction::TextCompare(..)
        | Instruction::TextLength(..)
        | Instruction::TextLeft(..)
        | Instruction::TextRight(..)
        | Instruction::TextMid(..)
        | Instruction::Closure(..)
        | Instruction::BorrowGlobal(..)
        | Instruction::GcNew(..)
        | Instruction::Drop(..)
        | Instruction::MoveOut(..)
        | Instruction::RangeNew(..)
        | Instruction::RangeInclusive(..)
        | Instruction::VecFrom(..)
        | Instruction::VecNew(..)
        | Instruction::VecPush(..)
        | Instruction::VecLength(..)
        | Instruction::CellNew(..)
        | Instruction::CellRead(..)
        | Instruction::CellWrite(..)
        | Instruction::CellReplace(..)
        | Instruction::Clone(..)
        | Instruction::TextMap(..)
        | Instruction::TextTest(..)
        | Instruction::TextReplace(..)
        | Instruction::ToText(..)
        | Instruction::TraitBox(..)
        | Instruction::Abs(..)
        | Instruction::CountToInteger(..)
        | Instruction::IntegerToCount(..)
        | Instruction::ParseInteger(..)
        | Instruction::ChannelBounded(..)
        | Instruction::ChannelUnbounded(..)
        | Instruction::ChannelClone(..)
        | Instruction::ChannelClose(..)
        | Instruction::ChannelIsClosed(..)
        | Instruction::ChannelSend(..)
        | Instruction::ChannelTrySend(..)
        | Instruction::ChannelReceive(..)
        | Instruction::ChannelTryReceive(..)
        | Instruction::SendErrorValue(..)
        | Instruction::Yield(..)
        | Instruction::GeneratorStart(..)
        | Instruction::GeneratorNext(..)
        | Instruction::GeneratorResumeUnit(..)
        | Instruction::GeneratorResume(..)
        | Instruction::GeneratorClose(..)
        | Instruction::LayoutForRc(..)
        | Instruction::Allocate(..)
        | Instruction::Deallocate(..)
        | Instruction::RcNewIn(..)
        | Instruction::LocationField(..)
        | Instruction::LocationLoad(..)
        | Instruction::LocationStore(..)
        | Instruction::PathNew(..)
        | Instruction::PathMissing(..)
        | Instruction::PathTarget(..)
        | Instruction::PathField(..)
        | Instruction::PathElement(..)
        | Instruction::Input(..)
        | Instruction::InputLoan(..)
        | Instruction::Extremum(..)
        | Instruction::SliceOf(..)
        | Instruction::SliceLength(..)
        | Instruction::Sum(..)
        | Instruction::RcNew(..)
        | Instruction::RcClone(..)
        | Instruction::RcDowngrade(..)
        | Instruction::RcUpgrade(..)
        | Instruction::RcStrongCount(..)
        | Instruction::EnumTake(..)
        | Instruction::GcBorrow(..)
        | Instruction::LoanField(..)
        | Instruction::LoanPayload(..)
        | Instruction::LoanElement(..)
        | Instruction::ArrayDeclare(..)
        | Instruction::ArrayNew(..)
        | Instruction::ArrayLoad(..)
        | Instruction::ArrayStore(..)
        | Instruction::ArrayLength(..) => {
            return Err(PrepareError::UnsupportedOperation {
                origin,
                instruction,
            });
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scalar_offsets_reject_arithmetic_overflow() {
        assert_eq!(frame_offset(0), Ok(ByteOffset(0)));
        assert_eq!(
            frame_offset(1),
            Ok(ByteOffset(
                u32::try_from(AbiLayout::SCALARS.slot_bytes).unwrap()
            ))
        );
        assert_eq!(frame_offset(usize::MAX), Err(PrepareError::FrameTooLarge));
    }
}
