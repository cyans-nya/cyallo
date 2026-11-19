use super::{
    CallTarget, FORMAT_VERSION, Instruction, IntegerOp, LoanOrigin, Register, RegisterType, Scalar,
    ScalarType, Shape, TextStyle, ValueType, VerifiedModule,
};
use std::fmt;

impl VerifiedModule {
    /// Print logical operands, interpreter slot placement, and archive spans.
    pub fn disassemble(&self) -> String {
        self.to_string()
    }
}

impl fmt::Display for VerifiedModule {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        writeln!(
            f,
            "verified bytecode v{FORMAT_VERSION}, entry f{}",
            self.entry.0
        )?;
        for (id, shape) in self.shapes.iter().enumerate() {
            match shape {
                Shape::Tuple(fields) => {
                    let fields: Vec<_> = fields.iter().map(|&ty| value_name(ty)).collect();
                    writeln!(f, "shape{id} = tuple({})", fields.join(", "))?;
                }
                Shape::Array { element, bounds } => {
                    let bounds: Vec<_> = bounds
                        .iter()
                        .map(|(lower, upper)| format!("{lower}..={upper}"))
                        .collect();
                    writeln!(
                        f,
                        "shape{id} = array[{}] of {}",
                        bounds.join(", "),
                        value_name(*element)
                    )?;
                }
                Shape::Vec { element } => {
                    writeln!(f, "shape{id} = vec of {}", value_name(*element))?;
                }
                Shape::Cell { element } => {
                    writeln!(f, "shape{id} = cell of {}", value_name(*element))?;
                }
                Shape::Slice { element } => {
                    writeln!(f, "shape{id} = slice of {}", value_name(*element))?;
                }
                Shape::Enum(variants) => {
                    let variants: Vec<_> = variants
                        .iter()
                        .map(|payload| payload.map_or_else(|| "unit".to_owned(), value_name))
                        .collect();
                    writeln!(f, "shape{id} = enum({})", variants.join(" | "))?;
                }
            }
        }
        for (id, function) in self.functions.iter().enumerate() {
            write!(f, "f{id}(")?;
            for (index, parameter) in function.parameters().iter().enumerate() {
                if index != 0 {
                    f.write_str(", ")?;
                }
                write!(f, "r{}", parameter.0)?;
            }
            let result = function
                .result()
                .map_or_else(|| "void".to_owned(), value_name);
            let layout = function.frame_layout();
            writeln!(f, ") -> {result} ({} frame slots)", layout.slot_count())?;
            for (index, &ty) in function.registers().iter().enumerate() {
                match ty {
                    RegisterType::Scalar(ty) => write!(f, "  r{index}: {}", type_name(ty))?,
                    RegisterType::Loan {
                        access,
                        origin: LoanOrigin::Local(owner),
                        pointee,
                    } => write!(
                        f,
                        "  r{index}: {access:?} {} loan of r{}",
                        value_name(pointee),
                        owner.0
                    )?,
                    RegisterType::Loan {
                        access,
                        origin: LoanOrigin::Parameter { register },
                        pointee,
                    } => write!(
                        f,
                        "  r{index}: {access:?} {} loan from parameter r{}",
                        value_name(pointee),
                        register.0
                    )?,
                    RegisterType::Loan {
                        access,
                        origin: LoanOrigin::Heap(managed),
                        pointee,
                    } => write!(
                        f,
                        "  r{index}: {access:?} {} loan of heap m{}",
                        value_name(pointee),
                        managed.0
                    )?,
                    RegisterType::Loan {
                        access,
                        origin: LoanOrigin::Global(global),
                        pointee,
                    } => write!(
                        f,
                        "  r{index}: {access:?} {} loan of global g{}",
                        value_name(pointee),
                        global.0
                    )?,
                    RegisterType::BorrowingFunction {
                        signature,
                        access,
                        origin,
                    } => write!(
                        f,
                        "  r{index}: {access:?} borrowing fn sig{} from {origin:?}",
                        signature.0
                    )?,
                    RegisterType::Gc(managed) => write!(f, "  r{index}: gc m{}", managed.0)?,
                    RegisterType::Object(id) => write!(f, "  r{index}: dyn trait{}", id.0)?,
                    RegisterType::Path => write!(f, "  r{index}: path")?,
                    RegisterType::Allocation => write!(f, "  r{index}: allocation")?,
                    RegisterType::Sender(id) => write!(f, "  r{index}: sender managed{}", id.0)?,
                    RegisterType::Receiver(id) => {
                        write!(f, "  r{index}: receiver managed{}", id.0)?
                    }
                    RegisterType::Generator(id) => write!(f, "  r{index}: generator sig{}", id.0)?,
                    RegisterType::Location(id) => {
                        write!(f, "  r{index}: location managed{}", id.0)?
                    }
                    RegisterType::Rc(managed) => write!(f, "  r{index}: rc m{}", managed.0)?,
                    RegisterType::Weak(managed) => write!(f, "  r{index}: weak m{}", managed.0)?,
                    RegisterType::Text => write!(f, "  r{index}: text")?,
                    RegisterType::Function(signature) => {
                        write!(f, "  r{index}: fn sig{}", signature.0)?
                    }
                    RegisterType::Composite(shape) => write!(f, "  r{index}: shape{}", shape.0)?,
                    RegisterType::Unused => continue,
                }
                if let Ok(index) = u32::try_from(index)
                    && let Some(slot) = layout.slot(Register(index))
                {
                    write!(f, " = slot{}", slot.index())?;
                }
                writeln!(f)?;
            }
            for (pc, (&instruction, span)) in function
                .instructions()
                .iter()
                .zip(function.spans())
                .enumerate()
            {
                write!(f, "  {pc:04} ")?;
                match instruction {
                    Instruction::StorageLive(reg) => write!(f, "storage.live r{}", reg.0)?,
                    Instruction::StorageDead(reg) => write!(f, "storage.dead r{}", reg.0)?,
                    Instruction::Constant(reg, Scalar::I64(value)) => {
                        write!(f, "r{} = const.i64 {value}", reg.0)?;
                    }
                    Instruction::Constant(reg, Scalar::U64(value)) => {
                        write!(f, "r{} = const.u64 {value}", reg.0)?;
                    }
                    Instruction::Constant(reg, Scalar::F64(bits)) => {
                        write!(f, "r{} = const.f64 {}", reg.0, f64::from_bits(bits))?;
                    }
                    Instruction::Constant(reg, Scalar::Bool(value)) => {
                        write!(f, "r{} = const.bool {value}", reg.0)?;
                    }
                    Instruction::Copy(dst, src) => write!(f, "r{} = copy r{}", dst.0, src.0)?,
                    Instruction::Borrow(dst, owner) => {
                        write!(f, "r{} = borrow r{}", dst.0, owner.0)?
                    }
                    Instruction::ReadLoan(dst, loan) => {
                        write!(f, "r{} = loan.read r{}", dst.0, loan.0)?
                    }
                    Instruction::WriteLoan(loan, value) => {
                        write!(f, "loan.write r{}, r{}", loan.0, value.0)?
                    }
                    Instruction::Integer(op, dst, lhs, rhs) => {
                        write!(
                            f,
                            "r{} = {} r{}, r{}",
                            dst.0,
                            integer_name(op),
                            lhs.0,
                            rhs.0
                        )?;
                    }
                    Instruction::Jump(to) => write!(f, "jump {}", to.0)?,
                    Instruction::Branch(condition, yes, no) => {
                        write!(f, "branch r{}, {}, {}", condition.0, yes.0, no.0)?;
                    }
                    Instruction::Call(site) => {
                        let call = &function.calls()[site.index()];
                        if let Some(dst) = call.destination {
                            write!(f, "r{} = ", dst.0)?;
                        }
                        match call.target {
                            CallTarget::Direct(id) => write!(f, "call.direct f{}(", id.0)?,
                            CallTarget::Bound(id) => write!(f, "call.bound host{}(", id.0)?,
                            CallTarget::Closure(callee) => {
                                write!(f, "call.closure r{}(", callee.0)?
                            }
                            CallTarget::Mapped(callee) => write!(f, "call.mapped r{}(", callee.0)?,
                            CallTarget::Method { object, method } => {
                                write!(f, "call.method r{}#{method}(", object.0)?;
                            }
                            CallTarget::Output(id) => write!(
                                f,
                                "output.{} out{}(",
                                match function.outputs()[id.index()].style {
                                    TextStyle::Plain => "plain",
                                    TextStyle::Caie => "caie",
                                },
                                id.0
                            )?,
                        }
                        for (index, register) in call.arguments.iter().enumerate() {
                            if index != 0 {
                                f.write_str(", ")?;
                            }
                            write!(f, "r{}", register.0)?;
                        }
                        write!(f, ") @call{}", site.0)?;
                    }
                    Instruction::Return(Some(reg)) => write!(f, "return r{}", reg.0)?,
                    Instruction::Return(None) => f.write_str("return")?,
                    Instruction::Float(op, dst, lhs, rhs) => {
                        write!(f, "r{} = f64.{op:?} r{}, r{}", dst.0, lhs.0, rhs.0)?;
                    }
                    Instruction::RealDivide(dst, lhs, rhs) => {
                        write!(f, "r{} = f64.div.i64 r{}, r{}", dst.0, lhs.0, rhs.0)?;
                    }
                    Instruction::Boolean(op, dst, lhs, rhs) => {
                        write!(f, "r{} = bool.{op:?} r{}, r{}", dst.0, lhs.0, rhs.0)?;
                    }
                    Instruction::NegateInteger(dst, src) => {
                        write!(f, "r{} = i64.checked.neg r{}", dst.0, src.0)?;
                    }
                    Instruction::NegateFloat(dst, src) => {
                        write!(f, "r{} = f64.neg r{}", dst.0, src.0)?
                    }
                    Instruction::Not(dst, src) => write!(f, "r{} = bool.not r{}", dst.0, src.0)?,
                    Instruction::IntegerToFloat(dst, src) => {
                        write!(f, "r{} = f64.from.i64 r{}", dst.0, src.0)?;
                    }
                    Instruction::FloatToInteger(dst, src) => {
                        write!(f, "r{} = i64.from.f64 r{}", dst.0, src.0)?;
                    }
                    Instruction::TextConstant(dst, text) => {
                        write!(
                            f,
                            "r{} = text.const {:?}",
                            dst.0,
                            function.texts()[text.index()]
                        )?;
                    }
                    Instruction::TextConcat(dst, lhs, rhs) => {
                        write!(f, "r{} = text.concat r{}, r{}", dst.0, lhs.0, rhs.0)?;
                    }
                    Instruction::TextCompare(op, dst, lhs, rhs) => {
                        write!(f, "r{} = text.{op:?} r{}, r{}", dst.0, lhs.0, rhs.0)?;
                    }
                    Instruction::TextLength(dst, text) => {
                        write!(f, "r{} = text.len r{}", dst.0, text.0)?
                    }
                    Instruction::TextLeft(dst, text, count) => {
                        write!(f, "r{} = text.left r{}, r{}", dst.0, text.0, count.0)?;
                    }
                    Instruction::TextRight(dst, text, count) => {
                        write!(f, "r{} = text.right r{}, r{}", dst.0, text.0, count.0)?;
                    }
                    Instruction::TextMid(dst, text, list) => {
                        write!(f, "r{} = text.mid r{}", dst.0, text.0)?;
                        write_list(f, &function.lists()[list.index()])?;
                    }
                    Instruction::Move(dst, src) => write!(f, "r{} = move r{}", dst.0, src.0)?,
                    Instruction::RangeNew(dst, start, end) => {
                        write!(f, "r{} = range r{}, r{}", dst.0, start.0, end.0)?;
                    }
                    Instruction::RangeInclusive(dst, start, end) => {
                        write!(f, "r{} = range.inclusive r{}, r{}", dst.0, start.0, end.0)?;
                    }
                    Instruction::VecFrom(dst, range) => {
                        write!(f, "r{} = vec.from r{}", dst.0, range.0)?
                    }
                    Instruction::VecNew(dst, list) => {
                        write!(f, "r{} = vec.new", dst.0)?;
                        write_list(f, &function.lists()[list.index()])?;
                    }
                    Instruction::VecPush(target, value) => {
                        write!(f, "vec.push r{}, r{}", target.0, value.0)?;
                    }
                    Instruction::VecLength(dst, vector) => {
                        write!(f, "r{} = vec.len r{}", dst.0, vector.0)?;
                    }
                    Instruction::CellNew(dst, value) => {
                        write!(f, "r{} = cell.new r{}", dst.0, value.0)?
                    }
                    Instruction::CellRead(dst, cell) => {
                        write!(f, "r{} = cell.read r{}", dst.0, cell.0)?
                    }
                    Instruction::CellWrite(cell, value) => {
                        write!(f, "cell.write r{}, r{}", cell.0, value.0)?;
                    }
                    Instruction::CellReplace(dst, cell, value) => {
                        write!(f, "r{} = cell.replace r{}, r{}", dst.0, cell.0, value.0)?;
                    }
                    Instruction::Clone(dst, src) => write!(f, "r{} = clone r{}", dst.0, src.0)?,
                    Instruction::TextMap(op, dst, src) => {
                        write!(f, "r{} = text.{op:?} r{}", dst.0, src.0)?;
                    }
                    Instruction::TextTest(op, dst, text, other) => {
                        write!(f, "r{} = text.{op:?} r{}, r{}", dst.0, text.0, other.0)?;
                    }
                    Instruction::TextReplace(dst, text, list) => {
                        write!(f, "r{} = text.replace r{}", dst.0, text.0)?;
                        write_list(f, &function.lists()[list.index()])?;
                    }
                    Instruction::ToText(dst, src) => write!(f, "r{} = to.text r{}", dst.0, src.0)?,
                    Instruction::ChannelBounded(dst, capacity) => {
                        write!(f, "r{} = channel.bounded r{}", dst.0, capacity.0)?
                    }
                    Instruction::ChannelUnbounded(dst) => {
                        write!(f, "r{} = channel.unbounded", dst.0)?
                    }
                    Instruction::ChannelClone(dst, end) => {
                        write!(f, "r{} = channel.clone r{}", dst.0, end.0)?
                    }
                    Instruction::ChannelClose(end) => write!(f, "channel.close r{}", end.0)?,
                    Instruction::ChannelIsClosed(dst, end) => {
                        write!(f, "r{} = channel.is_closed r{}", dst.0, end.0)?
                    }
                    Instruction::ChannelSend(dst, list, _) => {
                        write!(f, "r{} = channel.send", dst.0)?;
                        write_list(f, &function.lists()[list.index()])?;
                    }
                    Instruction::ChannelTrySend(dst, list, _) => {
                        write!(f, "r{} = channel.try_send", dst.0)?;
                        write_list(f, &function.lists()[list.index()])?;
                    }
                    Instruction::ChannelReceive(dst, end, _) => {
                        write!(f, "r{} = channel.recv r{}", dst.0, end.0)?
                    }
                    Instruction::ChannelTryReceive(dst, end, _) => {
                        write!(f, "r{} = channel.try_recv r{}", dst.0, end.0)?
                    }
                    Instruction::SendErrorValue(dst, error) => {
                        write!(f, "r{} = send_error.value r{}", dst.0, error.0)?
                    }
                    Instruction::Yield(dst, value) => write!(f, "r{} = yield r{}", dst.0, value.0)?,
                    Instruction::GeneratorStart(dst, generator) => {
                        write!(f, "r{} = generator.start r{}", dst.0, generator.0)?
                    }
                    Instruction::GeneratorNext(dst, generator) => {
                        write!(f, "r{} = generator.next r{}", dst.0, generator.0)?
                    }
                    Instruction::GeneratorResumeUnit(dst, generator) => {
                        write!(f, "r{} = generator.resume r{}", dst.0, generator.0)?
                    }
                    Instruction::GeneratorResume(dst, generator, value) => write!(
                        f,
                        "r{} = generator.resume r{}, r{}",
                        dst.0, generator.0, value.0
                    )?,
                    Instruction::GeneratorClose(generator) => {
                        write!(f, "generator.close r{}", generator.0)?
                    }
                    Instruction::LayoutForRc(dst) => write!(f, "r{} = layout.rc", dst.0)?,
                    Instruction::Allocate(dst, layout) => {
                        write!(f, "r{} = allocate r{}", dst.0, layout.0)?
                    }
                    Instruction::Deallocate(allocation) => {
                        write!(f, "deallocate r{}", allocation.0)?
                    }
                    Instruction::RcNewIn(dst, list, callee) => {
                        write!(f, "r{} = rc.new_in f{}", dst.0, callee.0)?;
                        write_list(f, &function.lists()[list.index()])?;
                    }
                    Instruction::LocationField(dst, owner, field) => {
                        write!(f, "r{} = location.field r{}, {field}", dst.0, owner.0)?
                    }
                    Instruction::LocationLoad(dst, location) => {
                        write!(f, "r{} = location.load r{}", dst.0, location.0)?
                    }
                    Instruction::LocationStore(location, value) => {
                        write!(f, "location.store r{}, r{}", location.0, value.0)?
                    }
                    Instruction::PathNew(dst, root) => {
                        write!(f, "r{} = path.new r{}", dst.0, root.0)?
                    }
                    Instruction::PathMissing(dst) => write!(f, "r{} = path.missing", dst.0)?,
                    Instruction::PathTarget(dst, src) => {
                        write!(f, "r{} = path.target r{}", dst.0, src.0)?
                    }
                    Instruction::PathField(dst, src, field) => {
                        write!(f, "r{} = path.field r{}, {field}", dst.0, src.0)?
                    }
                    Instruction::PathElement(dst, src, index) => {
                        write!(f, "r{} = path.element r{}, r{}", dst.0, src.0, index.0)?
                    }
                    Instruction::Input(dst, kind) => write!(f, "r{} = input.{kind:?}", dst.0)?,
                    Instruction::InputLoan(loan, kind) => {
                        write!(f, "loan.input.{kind:?} r{}", loan.0)?
                    }
                    Instruction::ParseInteger(dst, text) => {
                        write!(f, "r{} = parse.i64 r{}", dst.0, text.0)?;
                    }
                    Instruction::CountToInteger(dst, src) => {
                        write!(f, "r{} = i64.from.u64 r{}", dst.0, src.0)?;
                    }
                    Instruction::IntegerToCount(dst, src) => {
                        write!(f, "r{} = u64.from.i64 r{}", dst.0, src.0)?;
                    }
                    Instruction::TraitBox(dst, value, implementation) => {
                        write!(
                            f,
                            "r{} = trait.box r{}, impl{}",
                            dst.0, value.0, implementation.0
                        )?;
                    }
                    Instruction::Abs(dst, src) => {
                        write!(f, "r{} = i64.checked.abs r{}", dst.0, src.0)?
                    }
                    Instruction::Extremum(op, dst, lhs, rhs) => {
                        write!(f, "r{} = i64.{op:?} r{}, r{}", dst.0, lhs.0, rhs.0)?;
                    }
                    Instruction::SliceOf(dst, src) => {
                        write!(f, "r{} = slice.of r{}", dst.0, src.0)?
                    }
                    Instruction::SliceLength(dst, src) => {
                        write!(f, "r{} = slice.len r{}", dst.0, src.0)?;
                    }
                    Instruction::Sum(dst, src) => write!(f, "r{} = sum r{}", dst.0, src.0)?,
                    Instruction::MoveOut(dst, loan) => {
                        write!(f, "r{} = move.out r{}", dst.0, loan.0)?;
                    }
                    Instruction::Drop(reg) => write!(f, "drop r{}", reg.0)?,
                    Instruction::RcNew(dst, value) => {
                        write!(f, "r{} = rc.new r{}", dst.0, value.0)?
                    }
                    Instruction::RcClone(dst, src) => {
                        write!(f, "r{} = rc.clone r{}", dst.0, src.0)?
                    }
                    Instruction::RcDowngrade(dst, src) => {
                        write!(f, "r{} = rc.downgrade r{}", dst.0, src.0)?;
                    }
                    Instruction::RcUpgrade(dst, src) => {
                        write!(f, "r{} = rc.upgrade r{}", dst.0, src.0)?;
                    }
                    Instruction::RcStrongCount(dst, src) => {
                        write!(f, "r{} = rc.strong r{}", dst.0, src.0)?;
                    }
                    Instruction::EnumTake(dst, src, variant) => {
                        write!(f, "r{} = enum.take r{}, #{variant}", dst.0, src.0)?;
                    }
                    Instruction::GcNew(dst, value) => {
                        write!(f, "r{} = gc.new r{}", dst.0, value.0)?
                    }
                    Instruction::GcBorrow(dst, handle) => {
                        write!(f, "r{} = gc.borrow r{}", dst.0, handle.0)?;
                    }
                    Instruction::BorrowGlobal(dst, global) => {
                        write!(f, "r{} = borrow.global g{}", dst.0, global.0)?;
                    }
                    Instruction::LoanField(dst, src, field) => {
                        write!(f, "r{} = loan.field r{}.{field}", dst.0, src.0)?;
                    }
                    Instruction::LoanPayload(dst, src, variant) => {
                        write!(f, "r{} = loan.payload r{}, #{variant}", dst.0, src.0)?;
                    }
                    Instruction::LoanElement(dst, src, list) => {
                        write!(f, "r{} = loan.element r{}", dst.0, src.0)?;
                        write_list(f, &function.lists()[list.index()])?;
                    }
                    Instruction::Closure(dst, target, list) => {
                        write!(f, "r{} = closure f{}", dst.0, target.0)?;
                        write_list(f, &function.lists()[list.index()])?;
                    }
                    Instruction::ArrayDeclare(dst) => write!(f, "r{} = array.declare", dst.0)?,
                    Instruction::ArrayNew(dst, list) => {
                        write!(f, "r{} = array.new", dst.0)?;
                        write_list(f, &function.lists()[list.index()])?;
                    }
                    Instruction::ArrayLoad(dst, array, list) => {
                        write!(f, "r{} = array.load r{}", dst.0, array.0)?;
                        write_list(f, &function.lists()[list.index()])?;
                    }
                    Instruction::ArrayStore(array, list, value) => {
                        write!(f, "array.store r{}", array.0)?;
                        write_list(f, &function.lists()[list.index()])?;
                        write!(f, ", r{}", value.0)?;
                    }
                    Instruction::ArrayLength(dst, array) => {
                        write!(f, "r{} = array.len r{}", dst.0, array.0)?;
                    }
                    Instruction::CheckStep(step) => write!(f, "check.step r{}", step.0)?,
                    Instruction::Tuple(dst, list) => {
                        write!(f, "r{} = tuple", dst.0)?;
                        for (index, field) in function.lists()[list.index()].iter().enumerate() {
                            write!(f, "{} r{}", if index == 0 { "" } else { "," }, field.0)?;
                        }
                    }
                    Instruction::Extract(dst, src, field) => {
                        write!(f, "r{} = extract r{}.{field}", dst.0, src.0)?;
                    }
                    Instruction::EnumUnit(dst, variant) => {
                        write!(f, "r{} = enum.unit #{variant}", dst.0)?;
                    }
                    Instruction::EnumWrap(dst, variant, payload) => {
                        write!(f, "r{} = enum.wrap #{variant}, r{}", dst.0, payload.0)?;
                    }
                    Instruction::EnumIs(dst, src, variant) => {
                        write!(f, "r{} = enum.is r{}, #{variant}", dst.0, src.0)?;
                    }
                    Instruction::EnumPayload(dst, src, variant) => {
                        write!(f, "r{} = enum.payload r{}, #{variant}", dst.0, src.0)?;
                    }
                    Instruction::EnumExpect(src, variant) => {
                        write!(f, "enum.expect r{}, #{variant}", src.0)?;
                    }
                    Instruction::Trap => f.write_str("trap")?,
                }
                writeln!(f, "  [{}..{}]", span.start, span.end)?;
            }
            for marker in function.statements() {
                write!(
                    f,
                    "  .statement {:04} [{}..{}]",
                    marker.instruction.0, marker.span.start, marker.span.end
                )?;
                if let Some(arrival) = marker.arrival {
                    write!(f, " from {arrival:?}")?;
                }
                writeln!(f)?;
            }
        }
        Ok(())
    }
}

fn write_list(f: &mut fmt::Formatter<'_>, list: &[Register]) -> fmt::Result {
    f.write_str(" [")?;
    for (index, register) in list.iter().enumerate() {
        if index != 0 {
            f.write_str(", ")?;
        }
        write!(f, "r{}", register.0)?;
    }
    f.write_str("]")
}

fn value_name(ty: ValueType) -> String {
    match ty {
        ValueType::Scalar(ty) => type_name(ty).to_owned(),
        ValueType::Text => "text".to_owned(),
        ValueType::Gc(managed) => format!("gc m{}", managed.0),
        ValueType::Object(id) => format!("dyn trait{}", id.0),
        ValueType::Path => "path".to_owned(),
        ValueType::Allocation => "allocation".to_owned(),
        ValueType::Generator(id) => format!("generator sig{}", id.0),
        ValueType::Sender(id) => format!("sender managed{}", id.0),
        ValueType::Receiver(id) => format!("receiver managed{}", id.0),
        ValueType::Location(id) => format!("location managed{}", id.0),
        ValueType::Rc(managed) => format!("rc m{}", managed.0),
        ValueType::Weak(managed) => format!("weak m{}", managed.0),
        ValueType::Function(signature) => format!("fn sig{}", signature.0),
        ValueType::Composite(shape) => format!("shape{}", shape.0),
    }
}

fn type_name(ty: ScalarType) -> &'static str {
    match ty {
        ScalarType::I64 => "i64",
        ScalarType::U64 => "u64",
        ScalarType::F64 => "f64",
        ScalarType::Bool => "bool",
    }
}

fn integer_name(op: IntegerOp) -> &'static str {
    match op {
        IntegerOp::Add => "i64.checked.add",
        IntegerOp::Subtract => "i64.checked.sub",
        IntegerOp::Multiply => "i64.checked.mul",
        IntegerOp::Divide => "i64.checked.div",
        IntegerOp::Remainder => "i64.checked.rem",
        IntegerOp::Less => "i64.lt",
        IntegerOp::LessEqual => "i64.le",
        IntegerOp::Greater => "i64.gt",
        IntegerOp::GreaterEqual => "i64.ge",
        IntegerOp::Equal => "i64.eq",
        IntegerOp::NotEqual => "i64.ne",
    }
}
