//! Stackless lexical generators: saved activation fields, never a captured helper stack.
use super::{Frame, RunResult, Session, Value, gc, native::NativeSlot};
use crate::{ir::{LocalStorage, Reg}, metrics::FrameMetrics};

enum State { New, Suspended(Reg), Running, Complete, Closed }

struct Activation {
    // Registered pointers refer to stable Slots backing. Drop roots first.
    roots: Option<gc::FrameRoots>,
    function: usize,
    pc: usize,
    locals: gc::Slots<usize>,
    registers: gc::Slots<Value>,
    owner_id: Option<u64>,
    owned_cells: Vec<usize>,
    service_scope: Option<usize>,
    service_lease: Option<usize>,
    memory_finalizer: Option<usize>,
    metrics: Option<FrameMetrics>,
}
impl Activation {
    fn save(frame: Frame) -> Self {
        Self { roots: frame.roots, function: frame.function, pc: frame.pc, locals: frame.locals,
            registers: frame.registers, owner_id: frame.owner_id, owned_cells: frame.owned_cells,
            service_scope: frame.service_scope, service_lease: frame.service_lease,
            memory_finalizer: frame.memory_finalizer, metrics: frame.metrics }
    }
    fn restore(self, id: usize, result: Reg) -> Frame {
        let native_registers = self.registers.iter().map(NativeSlot::from_value).collect();
        Frame { generator: Some(id), roots: self.roots, function: self.function, pc: self.pc,
            locals: self.locals, registers: self.registers, native_registers, owner_id: self.owner_id,
            owned_cells: self.owned_cells, service_scope: self.service_scope, service_lease: self.service_lease,
            memory_finalizer: self.memory_finalizer, metrics: self.metrics, speed_version: 0, return_to: Some(result) }
    }
}
struct Generator {
    state: State,
    activation: Option<Activation>,
    unit_input: bool,
}
pub(super) struct Generators {
    next: usize,
    entries: std::collections::BTreeMap<usize, Generator>,
}
impl Default for Generators {
    fn default() -> Self { Self { next: 1, entries: Default::default() } }
}
impl Session {
    pub(super) fn construct_generator(&mut self, function: usize, args: Vec<Value>, result: Option<Reg>) -> RunResult<()> {
        if self.generators.entries.len() >= self.context.max_cells { return Err("generator limit exceeded".into()); }
        let ty = self.program.functions[function].generator.as_ref().unwrap();
        let (_, input, _) = cyallo_ir::generator::parts(ty).unwrap();
        let unit_input = cyallo_ir::generator::unit(input);
        self.push_frame(function, args, None)?;
        let activation = Activation::save(self.frames.pop().unwrap());
        let id = self.generators.next;
        self.generators.next = id.checked_add(1).ok_or("generator identity space exhausted")?;
        self.generators.entries.insert(id, Generator { state: State::New, activation: Some(activation), unit_input });
        self.publish_generator_roots();
        if let Some(result) = result { self.set(result, Value::Generator(id)); }
        else { self.close_generator(id)?; }
        Ok(())
    }
    pub(super) fn generator_id(&self, value: &Value) -> RunResult<usize> {
        match value {
            Value::Generator(id) => Ok(*id),
            Value::Address(cell) => self.generator_id(&self.heap[*cell].value),
            _ => Err("expected a generator".into()),
        }
    }
    pub(super) fn resume_generator(&mut self, id: usize, input: Option<Value>, result: Reg) -> RunResult<()> {
        if self.frames.len() >= self.context.max_frames { return Err("call depth limit exceeded".into()); }
        let generator = self.generators.entries.get_mut(&id).ok_or("cannot resume a completed or closed generator")?;
        match generator.state {
            State::New if input.is_some() => return Err("start a generator with resume(); arguments belong to its factory".into()),
            State::Running => return Err("generator is already running".into()),
            State::Complete | State::Closed => return Err("cannot resume a completed or closed generator".into()),
            State::Suspended(_) if input.is_none() && !generator.unit_input => return Err("resume requires the value expected by yield".into()),
            _ => {}
        }
        let mut activation = generator.activation.take().ok_or("generator has no continuation")?;
        if let State::Suspended(destination) = generator.state {
            activation.registers[destination] = input.unwrap_or_else(|| Value::ArraySnapshot(vec![]));
        }
        generator.state = State::Running;
        self.frames.push(activation.restore(id, result));
        Ok(())
    }
    pub(super) fn yield_generator(&mut self, input: Reg, value: Reg) -> RunResult<()> {
        let yielded = self.register(value);
        let frame = self.frames.pop().ok_or("yield without a frame")?;
        let id = frame.generator.ok_or("yield outside its lexical generator")?;
        let result = frame.return_to.ok_or("generator has no resumer")?;
        let generator = self.generators.entries.get_mut(&id).unwrap();
        generator.activation = Some(Activation::save(frame));
        generator.state = State::Suspended(input);
        self.publish_generator_roots();
        self.set(result, Value::GeneratorStep(cyallo_values::GeneratorStep::Yielded(Box::new(yielded))));
        Ok(())
    }
    pub(super) fn complete_generator(&mut self, value: Option<Reg>) -> RunResult<()> {
        let returned = value.map(|r| self.register(r)).unwrap_or_else(|| Value::ArraySnapshot(vec![]));
        let roots = self.root_scope();
        let returned = roots.value(returned);
        let frame = self.frames.pop().unwrap();
        let id = frame.generator.unwrap();
        let result = frame.return_to.ok_or("generator has no resumer")?;
        let generator = self.generators.entries.get_mut(&id).unwrap();
        generator.activation = Some(Activation::save(frame));
        generator.state = State::Complete;
        self.drop_generator_activation(id)?;
        self.generators.entries.remove(&id);
        self.set(result, Value::GeneratorStep(cyallo_values::GeneratorStep::Complete(Box::new(roots.value_copy(returned)))));
        Ok(())
    }
    pub(super) fn close_generator(&mut self, id: usize) -> RunResult<()> {
        let Some(generator) = self.generators.entries.get_mut(&id) else {
            return if id > 0 && id < self.generators.next { Ok(()) } else { Err("invalid generator".into()) };
        };
        if matches!(generator.state, State::Running) { return Err("cannot close a running generator".into()); }
        generator.state = State::Closed;
        self.drop_generator_activation(id)?;
        self.generators.entries.remove(&id);
        Ok(())
    }
    fn drop_generator_activation(&mut self, id: usize) -> RunResult<()> {
        let Some(mut activation) = self.generators.entries.get_mut(&id).and_then(|generator| generator.activation.take()) else { return Ok(()); };
        let function = &self.program.functions[activation.function];
        let locals: Vec<_> = function.locals.iter().zip(function.local_storage.iter()).filter_map(|(local, storage)| {
            if cyallo_ir::ownership::needs_drop(&local.ty) && let LocalStorage::Cell(slot) = storage { Some(activation.locals[*slot]) } else { None }
        }).collect();
        let mut live = Vec::new();
        function.resume.live_at(function, activation.pc, &mut live);
        let values: Vec<_> = function.registers.iter().enumerate().filter_map(|(r, ty)| {
            if cyallo_ir::ownership::needs_drop(ty) && live.get(r / 64).is_some_and(|bits| bits & (1 << (r % 64)) != 0) {
                Some(std::mem::replace(&mut activation.registers[r], Value::Uninitialized))
            } else { None }
        }).collect();
        for cell in locals { self.drop_cell(cell)?; }
        for value in values { self.drop_value(value)?; }
        let cells = std::mem::take(&mut activation.owned_cells);
        let lease = activation.service_lease;
        drop(activation);
        self.release_cells(cells);
        if let Some(lease) = lease { self.release_service_lease(lease); }
        Ok(())
    }
    pub(super) fn publish_generator_roots(&self) {
        let mut live = Vec::new();
        for generator in self.generators.entries.values() {
            if let Some(activation) = &generator.activation && let Some(roots) = &activation.roots {
                let function = &self.program.functions[activation.function];
                function.resume.live_at(function, activation.pc, &mut live);
                roots.publish(&live);
            }
        }
    }
}
