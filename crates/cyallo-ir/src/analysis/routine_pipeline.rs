//! HIR, MIR and AIR for the first executable slice of modern routine lowering.
mod air;
mod hir;
mod mir;

use super::{Result, Signature, collect, ir, key, routine_function};
use cyallo_ast::indexed::{IndexedAst, NameId};
use cyallo_ast::{Diagnostic, Expr, ExprKind, Function, Param, Span, Stmt, StmtKind, Type};
use cyallo_parser::BodyHandle;
use ir::{Op, Reg, Slot};
use std::{
    collections::{BTreeMap, HashMap},
    fmt,
    sync::{Arc, Mutex, OnceLock},
};

/// A parameter or local slot: parameters first, then locals in declaration order.
#[derive(Clone, Copy, Debug, Hash, PartialEq, Eq)]
struct Variable(usize);

thread_local! {
    static EMITTED: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

/// How many routine bodies this thread has lowered through the pipeline. A test that
/// compiles with one job can read it before and after to see which route a body took.
pub fn bodies_emitted_on_this_thread() -> usize {
    EMITTED.with(std::cell::Cell::get)
}

/// A local's name: written in the source, so interned in the body's unit, or made up by
/// the lowering (a loop's hidden counter and the like).
#[derive(Clone, Copy, Debug)]
pub(super) enum LocalName {
    Source(cyallo_ast::indexed::NameId),
    Hidden(&'static str),
}

/// What the module graph needs to bind one reference site.
pub(crate) struct SiteFacts<'a> {
    pub(crate) path: &'a str,
    pub(crate) arguments: usize,
    pub(crate) discard: bool,
    /// A record literal: the path names a record type.
    pub(crate) record: bool,
    /// A name used as a value that is not a local.
    pub(crate) name_use: bool,
    /// The value's type is fixed by where it stands: a returned value or a call argument.
    pub(crate) contextual: bool,
    pub(crate) span: Span,
}

/// A use of a name whose meaning depends on declarations outside this body.
#[derive(Clone, Debug)]
struct ReferenceSite {
    /// The path as written, interned in the body's unit; resolved when a site is bound.
    path: NameId,
    span: Span,
    /// Number of ordered arguments written at the site.
    arguments: usize,
    /// The call is a statement: the callee is a procedure and no value is used.
    discard: bool,
    /// A record literal: the path names a record type, not a callee.
    record: bool,
    /// A name used as a value that is not a local: acceptable only as a unit variant.
    name_use: bool,
    /// The value's type is fixed by where it stands: a returned value or a call argument.
    contextual: bool,
}

#[derive(Clone, Copy, Debug, Hash)]
struct SiteId(usize);

/// File-relative control flow retained by the module graph in place of an
/// owned AST body. Archive placement is applied when producing diagnostics or IR.
#[derive(Clone, Debug)]
pub struct IndexedRoutineBody {
    /// Reference sites in source order, known as soon as the body is prepared: the
    /// linker binds them. The control-flow graph itself is built on first use.
    sites: Arc<[ReferenceSite]>,
    lowering: Arc<Lowering>,
    /// Admission facts, computed from the syntax so they do not force the graph.
    size: usize,
    loops: bool,
    file_span: Span,
    archived_span: Span,
    offset: u32,
    digest: crate::sdk::Digest,
    /// Linked callee symbol for each reference site, in site order. Present once
    /// the owned body has been dropped in favor of this overlay.
    overlay: Option<Arc<[Box<str>]>>,
    /// The body's syntax, kept so the linker can materialize it as a fallback.
    handle: BodyHandle,
    /// For a generic routine: its linked parameters and result, with the type parameters
    /// as names. The template is checked against them once, with the parameters opaque.
    generic: Option<Arc<GenericHeader>>,
}

pub(super) struct GenericHeader {
    pub(super) params: Vec<Param>,
    pub(super) result: Type,
    /// What each type parameter may be used for, from its declared bounds.
    pub(super) bounds: Vec<(Box<str>, Vec<crate::generics::Bound>)>,
    /// The linked names of the traits the program declares that bound a parameter; a
    /// `Bound::User(i)` is the `i`th.
    pub(super) user_traits: Vec<Box<str>>,
}

impl fmt::Debug for GenericHeader {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("GenericHeader").finish_non_exhaustive()
    }
}

/// HIR waits here until something needs the control-flow graph (typed analysis),
/// so a routine that is never run never pays for it.
struct Lowering {
    hir: Mutex<Option<hir::Routine>>,
    mir: OnceLock<mir::Body>,
    span: Span,
    /// The outcome of checking a generic template with its parameters opaque.
    generic_check: OnceLock<std::result::Result<(), Diagnostic>>,
}

impl fmt::Debug for Lowering {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Lowering")
            .field("built", &self.mir.get().is_some())
            .finish_non_exhaustive()
    }
}

impl IndexedRoutineBody {
    fn mir(&self) -> &mir::Body {
        self.lowering.mir.get_or_init(|| {
            let hir = self
                .lowering
                .hir
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .take()
                .expect("the HIR is taken once, by the thread that builds the graph");
            mir::lower(hir, self.lowering.span)
        })
    }

    /// Why `prepare` would decline this body, or `None` when it is accepted. For coverage
    /// reports: the reason is the first unsupported form found.
    pub fn decline_reason(handle: &BodyHandle) -> Option<String> {
        match hir::from_indexed(handle.unit().syntax(), handle.function()) {
            Some(_) => None,
            None => Some(hir::last_decline().unwrap_or_else(|| "unrecognized form".into())),
        }
    }

    pub fn prepare(handle: &BodyHandle, offset: u32) -> Result<Option<Self>> {
        let function = handle.function();
        let Some(hir) = hir::from_indexed(handle.unit().syntax(), function) else {
            return Ok(None);
        };
        // The digest is of the body's own text: one call into the hash, and independent
        // of every imported declaration, as the cache key requires.
        let text = handle.unit().source().text();
        let Some(body_text) = text.get(function.span.start as usize..function.span.end as usize)
        else {
            return Ok(None);
        };
        Ok(Some(Self {
            digest: crate::module_cache::fingerprint(&body_text),
            sites: Arc::from(hir.sites.clone()),
            size: hir.size(),
            loops: hir.has_loops(),
            lowering: Arc::new(Lowering {
                hir: Mutex::new(Some(hir)),
                mir: OnceLock::new(),
                span: function.span,
                generic_check: OnceLock::new(),
            }),
            file_span: function.span,
            archived_span: shift(function.span, offset)?,
            offset,
            overlay: None,
            handle: handle.clone(),
            generic: None,
        }))
    }

    /// Reference sites in source order as `(path, argument count, is a statement, archived span)`.
    pub(crate) fn reference_sites(&self) -> impl Iterator<Item = SiteFacts<'_>> {
        self.sites.iter().map(|site| SiteFacts {
            path: self
                .handle
                .unit()
                .syntax()
                .name(site.path)
                .unwrap_or_default(),
            arguments: site.arguments,
            discard: site.discard,
            record: site.record,
            name_use: site.name_use,
            contextual: site.contextual,
            span: shift(site.span, self.offset).unwrap_or(site.span),
        })
    }

    /// Attach the linker's resolved callee symbol for each site, in site order.
    /// The overlay is part of the digest, so a retargeted call changes cache keys.
    pub(crate) fn with_targets(mut self, targets: Vec<Box<str>>) -> Self {
        debug_assert_eq!(targets.len(), self.sites.len());
        self.digest = crate::module_cache::fingerprint(&(self.digest, &targets));
        self.overlay = Some(targets.into());
        self
    }

    /// Record a generic routine's linked signature, whose type parameters are names.
    pub(crate) fn set_generic_header(
        &mut self,
        params: &[Param],
        result: &Type,
        bounds: Vec<(Box<str>, Vec<crate::generics::Bound>)>,
        user_traits: Vec<Box<str>>,
    ) {
        self.generic = Some(Arc::new(GenericHeader {
            params: params.to_vec(),
            result: result.clone(),
            bounds,
            user_traits,
        }));
    }

    /// The linked names of the program's traits that bound a type parameter.
    pub fn generic_user_traits(&self) -> &[Box<str>] {
        self.generic
            .as_ref()
            .map_or(&[], |header| &header.user_traits)
    }

    /// The bounds each type parameter of a generic routine was declared with.
    pub fn generic_bounds(&self) -> &[(Box<str>, Vec<crate::generics::Bound>)] {
        self.generic.as_ref().map_or(&[], |header| &header.bounds)
    }

    /// Whether this body is a generic template or an instance of one.
    pub fn is_generic(&self) -> bool {
        self.generic.is_some()
    }

    /// The archive-placed span of the routine, to order diagnostics.
    pub fn placed_span(&self) -> Span {
        self.archived_span
    }

    /// Check a generic template once, with its type parameters opaque: a value of such a
    /// type can be moved and returned and nothing else, whatever the instance turns out to
    /// be. The outcome is shared by the template and all its instances.
    pub(crate) fn check_generic(
        &self,
        signatures: &HashMap<String, Signature>,
        source_names: &BTreeMap<Box<str>, Box<str>>,
        definitions: &HashMap<String, Type>,
        services: &crate::services::Metadata,
    ) -> Result<()> {
        let Some(header) = &self.generic else {
            return Ok(());
        };
        self.lowering
            .generic_check
            .get_or_init(|| {
                let Some(callees) = self
                    .bind_callees(None, signatures, source_names)
                    .map_err(|error| error)?
                else {
                    return Ok(());
                };
                air::analyze(
                    self.mir(),
                    &header.params,
                    &header.result,
                    &callees,
                    definitions,
                    self.handle.unit().syntax(),
                    self.file_span,
                    &header.bounds,
                    Some(header),
                    Some(services),
                )
                .map(|_| ())
                .map_err(|mut error| {
                    match cyallo_ast::spans::shift_diagnostic(&mut error, self.offset) {
                        Ok(()) => error,
                        Err(shift_error) => shift_error,
                    }
                })
            })
            .clone()
    }

    /// The owned statements with archive-placed spans, for a routine the indexed
    /// pipeline cannot take and the linker must resolve as a tree.
    pub(crate) fn materialize_owned(&self) -> Result<Vec<Stmt>> {
        let mut tree = cyallo_ast::Ast {
            body: self.handle.materialize_body(),
            ..Default::default()
        };
        if self.offset != 0 {
            cyallo_ast::spans::shift_ast(&mut tree, self.offset)?;
        }
        Ok(tree.body)
    }

    pub fn archived_span(&self) -> Span {
        self.archived_span
    }

    pub(crate) fn digest(&self) -> crate::sdk::Digest {
        self.digest
    }

    pub(super) fn estimate(&self) -> crate::admission::Estimate {
        crate::admission::Estimate {
            nodes: self.size,
            // Direct-call edges come from the overlay, as they do from an owned body.
            calls: self
                .overlay
                .iter()
                .flat_map(|o| o.iter().cloned())
                .collect(),
            shape: ir::RoutineShape {
                calls: !self.sites.is_empty(),
                loops: self.loops,
            },
        }
    }

    /// `Ok(None)` leaves the routine to the existing route: a reference site
    /// that does not bind to a plain `i64` function contract is unsupported.
    #[cfg(test)]
    pub(super) fn emit(
        &self,
        function: &Function,
        source_names: &BTreeMap<Box<str>, Box<str>>,
        signatures: &HashMap<String, Signature>,
        definitions: &HashMap<String, Type>,
    ) -> Result<Option<ir::Function>> {
        self.emit_with(
            function,
            source_names,
            signatures,
            definitions,
            &crate::services::Metadata::default(),
        )
    }

    /// As `emit`, with the program's traits and implementations, which a method call on a
    /// generic routine's parameter is resolved against.
    pub(super) fn emit_with(
        &self,
        function: &Function,
        source_names: &BTreeMap<Box<str>, Box<str>>,
        signatures: &HashMap<String, Signature>,
        definitions: &HashMap<String, Type>,
        services: &crate::services::Metadata,
    ) -> Result<Option<ir::Function>> {
        let Some(callees) = self.bind_callees(Some(function), signatures, source_names)? else {
            return Ok(None);
        };
        lower_mir(
            self.mir(),
            self.file_span,
            self.offset,
            function,
            source_names,
            &callees,
            definitions,
            self.handle.unit().syntax(),
            self.generic.as_deref(),
            services,
        )
    }

    /// Bind each reference site to a callee contract. The overlay is read from
    /// the linker's resolved call targets, keyed by archive-placed site span.
    /// The MIR itself stays independent of every imported declaration.
    fn bind_callees(
        &self,
        function: Option<&Function>,
        signatures: &HashMap<String, Signature>,
        source_names: &BTreeMap<Box<str>, Box<str>>,
    ) -> Result<Option<Vec<air::Site>>> {
        if self.sites.is_empty() {
            return Ok(Some(Vec::new()));
        }
        let mut targets = HashMap::new();
        if self.overlay.is_none()
            && let Some(function) = function
        {
            call_targets(&function.body, &mut targets);
        }
        let mut callees = Vec::with_capacity(self.sites.len());
        for (index, site) in self.sites.iter().enumerate() {
            if site.record {
                // The record literal's type, which the linker already checked.
                let Some(target) = self.overlay.as_ref().and_then(|overlay| overlay.get(index))
                else {
                    return Ok(None);
                };
                callees.push(air::Site::Record(target.clone()));
                continue;
            }
            let target = match &self.overlay {
                Some(overlay) => overlay.get(index).map(|target| &**target),
                None => targets.get(&shift(site.span, self.offset)?).copied(),
            };
            if target == Some("$print") {
                if !site.discard {
                    return Ok(None);
                }
                callees.push(air::Site::Print);
                continue;
            }
            if let Some(id) = target
                .and_then(|target| target.strip_prefix("$builtin:"))
                .and_then(|number| number.parse::<u16>().ok())
                .and_then(|number| {
                    crate::intrinsic::Intrinsic::from_id(cyallo_ast::IntrinsicId(number, None))
                })
            {
                callees.push(air::Site::Builtin(id));
                continue;
            }
            let signature = target.and_then(|target| signatures.get(&key(target)));
            if let Some(target) = target
                && target.starts_with("$variant")
            {
                // A variant constructor is built in place: its tag and payload, no call.
                let Some(name) = source_names.get(target) else {
                    return Ok(None);
                };
                let concrete = signature.and_then(|signature| match &signature.result {
                    Type::Enum(contract) => Some((signature, contract)),
                    _ => None,
                });
                let Some((signature, contract)) = concrete else {
                    // A variant of a generic enum: its instance comes from the expected type.
                    let owner = target
                        .strip_prefix("$variant")
                        .and_then(|rest| rest.split_once('$'))
                        .and_then(|(id, _)| id.parse::<u32>().ok());
                    let Some(owner) = owner else {
                        return Ok(None);
                    };
                    if site.arguments > 1 || site.discard {
                        return Ok(None);
                    }
                    callees.push(air::Site::GenericVariant {
                        owner: cyallo_ast::DeclarationId(owner),
                        name: name.clone(),
                        payload: site.arguments == 1,
                    });
                    continue;
                };
                let Some(variant) = contract.variants.iter().find(|v| *v.name == **name) else {
                    return Ok(None);
                };
                if signature.params.len() != site.arguments || site.discard {
                    return Ok(None);
                }
                callees.push(air::Site::Variant {
                    variant: variant.operation(),
                    payload: variant.payload.clone(),
                    result: signature.result.clone(),
                });
                continue;
            }
            let Some(signature) = signature else {
                return Ok(None);
            };
            // A statement may discard any result; a value must come from a callee that has one.
            // The linker has already checked the callee's types.
            if signature.params.len() != site.arguments
                || (signature.result == Type::Void && !site.discard)
            {
                return Ok(None);
            }
            callees.push(air::Site::Call(air::Callee {
                id: signature.id,
                parameters: signature.params.iter().map(|p| p.ty.clone()).collect(),
                result: signature.result.clone(),
            }));
        }
        Ok(Some(callees))
    }
}

fn call_targets<'a>(statements: &'a [Stmt], targets: &mut HashMap<Span, &'a str>) {
    for statement in statements {
        match &statement.kind {
            StmtKind::If(condition, yes, no) => {
                call_expression_targets(condition, targets);
                call_targets(yes, targets);
                call_targets(no, targets);
            }
            StmtKind::Return(value) => call_expression_targets(value, targets),
            _ => {}
        }
    }
}

fn call_expression_targets<'a>(expression: &'a Expr, targets: &mut HashMap<Span, &'a str>) {
    match &expression.kind {
        ExprKind::Call(target, arguments) => {
            targets.insert(expression.span, target);
            for argument in arguments {
                call_expression_targets(argument, targets);
            }
        }
        ExprKind::Parenthesized(inner) => call_expression_targets(inner, targets),
        ExprKind::Binary(_, left, right) => {
            call_expression_targets(left, targets);
            call_expression_targets(right, targets);
        }
        _ => {}
    }
}

fn shift(span: Span, offset: u32) -> Result<Span> {
    cyallo_ast::spans::shift_span_checked(span, offset)
        .map_err(cyallo_ast::spans::invalid_span_shift)
}

fn lower_mir(
    mir: &mir::Body,
    file_span: Span,
    offset: u32,
    function: &Function,
    source_names: &BTreeMap<Box<str>, Box<str>>,
    callees: &[air::Site],
    definitions: &HashMap<String, Type>,
    syntax: &IndexedAst,
    generic: Option<&GenericHeader>,
    services: &crate::services::Metadata,
) -> Result<Option<ir::Function>> {
    let mut locals = collect(&[], &function.params)?;
    for local in &mut locals {
        if let Some(name) = source_names.get(&local.name) {
            local.display_name = Some(name.clone());
        }
    }
    let air = match air::analyze(
        mir,
        &function.params,
        &function.result,
        callees,
        definitions,
        syntax,
        file_span,
        &[],
        generic,
        Some(services),
    ) {
        Ok(body) => body,
        Err(mut error) => {
            cyallo_ast::spans::shift_diagnostic(&mut error, offset)?;
            return Err(error);
        }
    };
    for (info, ty) in mir.locals.iter().zip(&air.locals) {
        // The one place a local's name becomes owned text: the IR keeps it.
        let name: Box<str> = match info.name {
            LocalName::Source(id) => syntax.name(id).unwrap_or_default().into(),
            LocalName::Hidden(text) => text.into(),
        };
        locals.push(ir::Local {
            display_name: Some(name.clone()),
            name,
            ty: ty.clone(),
            constant: info.constant,
            span: shift(info.span, offset)?,
        });
    }
    let output = routine_function(function, locals, true);
    let mut emitted = emit(air, output);
    if offset != 0 {
        for instruction in &mut emitted.code {
            instruction.span = shift(instruction.span, offset)?;
        }
    }
    cyallo_startup_profile::count(cyallo_startup_profile::Counter::BodiesEmittedFromAir, 1);
    EMITTED.with(|count| count.set(count.get() + 1));
    Ok(Some(emitted))
}

/// How many instructions an operation expands to.
fn instruction_count(operation: &air::Operation) -> usize {
    match operation {
        air::Operation::Consume(..) => 0,
        air::Operation::EnumTake(..) => 3,
        air::Operation::ReadVariable(..)
        | air::Operation::EnumTest(..)
        | air::Operation::EnumPayload(..)
        | air::Operation::TakeVariable(..)
        | air::Operation::Store(..)
        | air::Operation::Borrow(..) => 2,
        // The root's address, a read through it when the root is a reference, one
        // projection per field, and the final load or store.
        air::Operation::FieldRead(_, _, place) | air::Operation::FieldWrite(_, place, _) => {
            1 + usize::from(place.via_reference) + place.steps.len() + 1
        }
        // The array's address, the element's address, and the load or store.
        air::Operation::IndexRead(_, _, _, through)
        | air::Operation::IndexWrite(_, _, _, through) => 3 + usize::from(*through),
        _ => 1,
    }
}

/// The address of a field: the root local's address (and the reference stored there,
/// when the root is one), followed through each field index.
fn field_address(
    function: &mut ir::Function,
    root: Variable,
    place: &air::Place,
    span: Span,
) -> Reg {
    let address = register(function, &place.root_type);
    push(
        function,
        Op::Address(
            address,
            Slot {
                global: false,
                index: root.0,
            },
        ),
        span,
    );
    let mut current = address;
    if place.via_reference {
        let reference = register(function, &place.root_type);
        push(function, Op::Load(reference, address), span);
        current = reference;
    }
    for (index, ty) in &place.steps {
        let next = register(function, ty);
        push(function, Op::Field(next, current, *index), span);
        current = next;
    }
    current
}

fn emit(checked: air::Body, mut function: ir::Function) -> ir::Function {
    // The adapter expands a parameter read into Address and Load. Explicit
    // fallthrough blocks consume no logical instruction.
    let mut starts = Vec::with_capacity(checked.blocks.len());
    let mut pc = 0;
    for block in &checked.blocks {
        starts.push(pc);
        pc += block
            .operations
            .iter()
            .map(|operation| instruction_count(&operation.kind))
            .sum::<usize>();
        if !matches!(block.exit.kind, air::Exit::Next) {
            pc += 1;
        }
    }
    function.code.reserve(pc);
    let mut registers = vec![0; checked.types.len()];
    for block in checked.blocks {
        for mir::Located { kind, span } in block.operations {
            let operation = match kind {
                air::Operation::Statement => Op::Statement,
                air::Operation::Constant(value, literal) => {
                    let dst = register(&mut function, &checked.types[value.0]);
                    registers[value.0] = dst;
                    Op::Literal(dst, literal)
                }
                air::Operation::Text(value, text) => {
                    let dst = register(&mut function, &checked.types[value.0]);
                    registers[value.0] = dst;
                    // The interned text is shared, not copied.
                    Op::LiteralString(dst, text)
                }
                air::Operation::TakeVariable(value, variable) => {
                    // Moving a value out empties the local it came from.
                    let ty = &checked.types[value.0];
                    let address = register(&mut function, ty);
                    push(
                        &mut function,
                        Op::Address(
                            address,
                            Slot {
                                global: false,
                                index: variable.0,
                            },
                        ),
                        span,
                    );
                    let dst = register(&mut function, ty);
                    registers[value.0] = dst;
                    Op::Take(dst, address)
                }
                air::Operation::Store(variable, value) => {
                    let ty = &checked.types[value.0];
                    let address = register(&mut function, ty);
                    push(
                        &mut function,
                        Op::Address(
                            address,
                            Slot {
                                global: false,
                                index: variable.0,
                            },
                        ),
                        span,
                    );
                    Op::Store(address, registers[value.0])
                }
                air::Operation::ReadVariable(value, parameter) => {
                    let ty = &checked.types[value.0];
                    let address = register(&mut function, ty);
                    push(
                        &mut function,
                        Op::Address(
                            address,
                            Slot {
                                global: false,
                                index: parameter.0,
                            },
                        ),
                        span,
                    );
                    let dst = register(&mut function, ty);
                    registers[value.0] = dst;
                    Op::Load(dst, address)
                }
                air::Operation::Call(value, target, range) => {
                    let arguments = checked.operands[range.start..range.start + range.len]
                        .iter()
                        .map(|argument| registers[argument.0])
                        .collect();
                    let dst = register(&mut function, &checked.types[value.0]);
                    registers[value.0] = dst;
                    Op::Call(Some(dst), target, arguments)
                }
                air::Operation::Aggregate(value, fields) => {
                    let dst = register(&mut function, &checked.types[value.0]);
                    registers[value.0] = dst;
                    Op::Aggregate(dst, fields.iter().map(|field| registers[field.0]).collect())
                }
                air::Operation::FieldRead(value, root, place) => {
                    let address = field_address(&mut function, root, &place, span);
                    let dst = register(&mut function, &checked.types[value.0]);
                    registers[value.0] = dst;
                    Op::Load(dst, address)
                }
                air::Operation::Slice(value, array, mutable) => {
                    let dst = register(&mut function, &checked.types[value.0]);
                    registers[value.0] = dst;
                    Op::Lookup(
                        dst,
                        crate::lookup::Operation::Slice(mutable),
                        vec![registers[array.0]],
                    )
                }
                air::Operation::ArrayLiteral(value, range) => {
                    let elements = checked.operands[range.start..range.start + range.len]
                        .iter()
                        .map(|element| registers[element.0])
                        .collect();
                    let dst = register(&mut function, &checked.types[value.0]);
                    registers[value.0] = dst;
                    Op::ArrayLiteral(dst, elements)
                }
                air::Operation::IndexRead(value, root, index, _) => {
                    let array = index_base(&mut function, root, span);
                    let element = register(&mut function, &checked.types[value.0]);
                    push(
                        &mut function,
                        Op::Index(element, array, vec![registers[index.0]], false),
                        span,
                    );
                    let dst = register(&mut function, &checked.types[value.0]);
                    registers[value.0] = dst;
                    Op::Load(dst, element)
                }
                air::Operation::IndexWrite(root, index, value, _) => {
                    let array = index_base(&mut function, root, span);
                    let element = register(&mut function, &checked.types[value.0]);
                    push(
                        &mut function,
                        Op::Index(element, array, vec![registers[index.0]], true),
                        span,
                    );
                    Op::Store(element, registers[value.0])
                }
                air::Operation::FieldWrite(root, place, value) => {
                    let address = field_address(&mut function, root, &place, span);
                    Op::Store(address, registers[value.0])
                }
                air::Operation::Load(value, reference) => {
                    let dst = register(&mut function, &checked.types[value.0]);
                    registers[value.0] = dst;
                    Op::Load(dst, registers[reference.0])
                }
                air::Operation::StoreIndirect(reference, value) => {
                    Op::Store(registers[reference.0], registers[value.0])
                }
                air::Operation::Borrow(value, variable) => {
                    // The address of the local, then a reference value that holds it.
                    let Type::Reference { pointee, .. } = &checked.types[value.0] else {
                        unreachable!("a borrow produces a reference");
                    };
                    let address = register(&mut function, pointee);
                    push(
                        &mut function,
                        Op::Address(
                            address,
                            Slot {
                                global: false,
                                index: variable.0,
                            },
                        ),
                        span,
                    );
                    let dst = register(&mut function, &checked.types[value.0]);
                    registers[value.0] = dst;
                    Op::Move(dst, address)
                }
                air::Operation::CallStatement(target, range) => {
                    let arguments = checked.operands[range.start..range.start + range.len]
                        .iter()
                        .map(|argument| registers[argument.0])
                        .collect();
                    Op::Call(None, target, arguments)
                }
                air::Operation::EnumConstruct(value, variant, payload) => {
                    let dst = register(&mut function, &checked.types[value.0]);
                    registers[value.0] = dst;
                    Op::EnumConstruct(dst, variant, payload.map(|value| registers[value.0]))
                }
                air::Operation::EnumTest(value, variable, variant) => {
                    let address = enum_address(&mut function, variable, span);
                    let dst = register(&mut function, &checked.types[value.0]);
                    registers[value.0] = dst;
                    Op::EnumTest(dst, address, variant)
                }
                air::Operation::Consume(_) => continue,
                air::Operation::Builtin(value, id, range) => {
                    let arguments = checked.operands[range.start..range.start + range.len]
                        .iter()
                        .map(|argument| registers[argument.0])
                        .collect();
                    let dst = register(&mut function, &checked.types[value.0]);
                    registers[value.0] = dst;
                    Op::Builtin(dst, id, arguments)
                }
                air::Operation::Print(range) => Op::Output(
                    checked.operands[range.start..range.start + range.len]
                        .iter()
                        .map(|argument| registers[argument.0])
                        .collect(),
                    ir::TextStyle::Plain,
                ),
                air::Operation::EnumTake(value, variable, variant, ty) => {
                    let address = enum_address(&mut function, variable, span);
                    let payload = register(&mut function, &ty);
                    push(
                        &mut function,
                        Op::EnumPayload(
                            payload,
                            address,
                            variant,
                            crate::enums::Projection::Address,
                            Box::new(ty.clone()),
                        ),
                        span,
                    );
                    let dst = register(&mut function, &ty);
                    registers[value.0] = dst;
                    Op::Take(dst, payload)
                }
                air::Operation::EnumPayload(value, variable, variant, ty) => {
                    let address = enum_address(&mut function, variable, span);
                    let dst = register(&mut function, &ty);
                    registers[value.0] = dst;
                    Op::EnumPayload(
                        dst,
                        address,
                        variant,
                        crate::enums::Projection::Value,
                        Box::new(ty),
                    )
                }
                air::Operation::Unary(value, operator, operand) => {
                    let dst = register(&mut function, &checked.types[value.0]);
                    registers[value.0] = dst;
                    Op::Unary(dst, operator, registers[operand.0])
                }
                air::Operation::Binary(value, operator, left, right) => {
                    let dst = register(&mut function, &checked.types[value.0]);
                    registers[value.0] = dst;
                    Op::Binary(dst, operator, registers[left.0], registers[right.0])
                }
            };
            push(&mut function, operation, span);
        }
        let operation = match block.exit.kind {
            air::Exit::Branch { condition, yes, no } => {
                Op::Branch(registers[condition.0], starts[yes], starts[no])
            }
            air::Exit::Jump(target) => Op::Jump(starts[target]),
            air::Exit::Next => continue,
            air::Exit::Return(value) => Op::Return(Some(registers[value.0])),
            air::Exit::End | air::Exit::ReturnVoid => Op::Return(None),
            air::Exit::Unreachable => Op::Unreachable,
        };
        push(&mut function, operation, block.exit.span);
    }
    debug_assert_eq!(function.code.len(), pc);
    function
}

/// The address of the local that holds an enum.
/// What `Op::Index` takes: the local array's address, or, for a slice parameter, the
/// reference stored in it.
fn index_base(function: &mut ir::Function, root: Variable, span: Span) -> Reg {
    let address = enum_address(function, root, span);
    if matches!(function.locals[root.0].ty, Type::Reference { .. }) {
        let reference = register(function, &function.locals[root.0].ty.clone());
        push(function, Op::Load(reference, address), span);
        reference
    } else {
        address
    }
}

fn enum_address(function: &mut ir::Function, variable: Variable, span: Span) -> Reg {
    let ty = function.locals[variable.0].ty.clone();
    let address = register(function, &ty);
    push(
        function,
        Op::Address(
            address,
            Slot {
                global: false,
                index: variable.0,
            },
        ),
        span,
    );
    address
}

fn register(function: &mut ir::Function, ty: &Type) -> Reg {
    let index = function.registers.len();
    function.registers.push(ty.clone());
    index
}

fn push(function: &mut ir::Function, op: Op, span: Span) {
    function.code.push(ir::Instruction { op, span });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn prepare(source: &str) -> (Function, IndexedRoutineBody) {
        let file =
            cyallo_parser::parse_modern_snapshot(source.into(), false, crate::Jobs(1)).unwrap();
        let handle = file
            .units()
            .iter()
            .flat_map(|unit| unit.bodies())
            .next()
            .unwrap();
        let body = IndexedRoutineBody::prepare(&handle, 0)
            .unwrap()
            .expect("scalar routine is selected");
        let header = file
            .to_source_tree_with_bodies(&|_| false)
            .functions
            .remove(0);
        (header, body)
    }

    #[test]
    fn indexed_routine_produces_explicit_blocks_and_checked_values() {
        let source = include_str!("../../../../examples/routine-ir.yas");
        let (function, prepared) = prepare(source);
        let mir = prepared.mir();
        let mir::Exit::Branch { yes, no, .. } = mir.blocks[0].exit.kind else {
            panic!("the condition terminates the entry block");
        };
        assert!(matches!(
            mir.blocks[yes.0].exit.kind,
            mir::Exit::Return { .. }
        ));
        assert!(matches!(
            mir.blocks[no.0].exit.kind,
            mir::Exit::Return { .. }
        ));
        let locals = collect(&[], &function.params).unwrap();
        let body = air::analyze(
            mir,
            &function.params,
            &function.result,
            &[],
            &HashMap::new(),
            prepared.handle.unit().syntax(),
            function.span,
            &[],
            None,
            None,
        )
        .unwrap();
        assert!(matches!(
            body.types.as_slice(),
            [Type::Integer, Type::Integer, Type::Boolean, ..]
        ));
        let emitted = emit(body, routine_function(&function, locals, true));
        for instruction in &emitted.code {
            if let Op::Branch(_, yes, no) = instruction.op {
                assert!(matches!(emitted.code[yes].op, Op::Statement));
                assert!(matches!(emitted.code[no].op, Op::Statement));
            }
        }
    }

    #[test]
    fn selected_routine_reports_type_errors_at_source_expressions() {
        for (source, diagnostic_span) in [
            (
                "fn wrong(n: i64) -> i64 { if n { return 1 }\nreturn 0 }",
                "n",
            ),
            ("fn wrong(n: i64) -> i64 { return (n > 0) }", "(n > 0)"),
            ("fn wrong(n: i64) -> i64 { return n + true }", "n + true"),
        ] {
            let (function, body) = prepare(source);
            let diagnostic = body
                .emit(
                    &function,
                    &BTreeMap::new(),
                    &HashMap::new(),
                    &HashMap::new(),
                )
                .unwrap_err();
            assert_eq!(diagnostic.span.source_text(source), diagnostic_span);
            assert!(matches!(diagnostic.code, "E109" | "E105"));
        }
    }

    /// Parse `source`, prepare MIR for body `index`, and materialize every body
    /// so call targets are visible to the overlay.
    fn prepare_nth(source: &str, index: usize) -> (Vec<Function>, IndexedRoutineBody) {
        let file =
            cyallo_parser::parse_modern_snapshot(source.into(), false, crate::Jobs(1)).unwrap();
        let handle = file
            .units()
            .iter()
            .flat_map(|unit| unit.bodies())
            .nth(index)
            .unwrap();
        let body = IndexedRoutineBody::prepare(&handle, 0).unwrap().unwrap();
        (file.to_source_tree_with_bodies(&|_| true).functions, body)
    }

    fn signature(function: &Function, id: usize) -> Signature {
        Signature {
            kind: function.kind,
            params: function.params.clone(),
            result: function.result.clone(),
            id,
        }
    }

    #[test]
    fn direct_calls_keep_sites_in_mir_and_bind_through_the_overlay() {
        let source = "fn bump(n: i64) -> i64 { return n + 1 }\nfn twice(n: i64) -> i64 { return bump(bump(n)) }";
        let (functions, body) = prepare_nth(source, 1);
        // Two sites in source order; MIR holds no resolved target.
        assert_eq!(body.sites.len(), 2);
        let syntax = body.handle.unit().syntax();
        assert!(
            body.sites
                .iter()
                .all(|site| syntax.name(site.path) == Some("bump"))
        );
        let signatures = HashMap::from([("bump".to_owned(), signature(&functions[0], 7))]);
        let emitted = body
            .emit(
                &functions[1],
                &BTreeMap::new(),
                &signatures,
                &HashMap::new(),
            )
            .unwrap()
            .expect("a plain i64 callee is bound");
        let calls: Vec<_> = emitted
            .code
            .iter()
            .filter_map(|instruction| match &instruction.op {
                Op::Call(Some(_), target, arguments) => Some((*target, arguments.len())),
                _ => None,
            })
            .collect();
        assert_eq!(calls, [(7, 1), (7, 1)]);
        // The inner call's result feeds the outer call's only argument.
        let results: Vec<_> = emitted
            .code
            .iter()
            .filter_map(|instruction| match &instruction.op {
                Op::Call(Some(result), _, arguments) => Some((*result, arguments[0])),
                _ => None,
            })
            .collect();
        assert_eq!(results[0].0, results[1].1);
    }

    #[test]
    fn unbound_or_mismatched_callees_leave_the_routine_to_the_existing_route() {
        let source = "fn bump(n: i64) -> i64 { return n + 1 }\nfn twice(n: i64) -> i64 { return bump(n, n) }";
        let (functions, body) = prepare_nth(source, 1);
        let signatures = HashMap::from([("bump".to_owned(), signature(&functions[0], 0))]);
        let emitted = body.emit(
            &functions[1],
            &BTreeMap::new(),
            &signatures,
            &HashMap::new(),
        );
        assert!(emitted.unwrap().is_none(), "arity mismatch is unsupported");
        let emitted = body.emit(
            &functions[1],
            &BTreeMap::new(),
            &HashMap::new(),
            &HashMap::new(),
        );
        assert!(emitted.unwrap().is_none(), "unbound site is unsupported");
    }

    #[test]
    fn mir_is_independent_of_the_callee_declaration() {
        let call = "fn twice(n: i64) -> i64 { return bump(n) }";
        let (_, first) = prepare_nth(
            &format!("fn bump(n: i64) -> i64 {{ return n + 1 }}\n{call}"),
            1,
        );
        let (_, second) = prepare_nth(
            &format!("fn bump(n: u64) -> u64 {{ return n * 9 }}\n{call}"),
            1,
        );
        assert_eq!(first.digest(), second.digest());
    }

    #[test]
    fn short_circuit_operands_must_be_boolean() {
        for source in [
            "fn f(n: i64) -> bool { return n && true }",
            "fn f(n: i64) -> bool { return true || n }",
        ] {
            let (function, body) = prepare(source);
            let error = body
                .emit(
                    &function,
                    &BTreeMap::new(),
                    &HashMap::new(),
                    &HashMap::new(),
                )
                .unwrap_err();
            assert_eq!(error.code, "E109", "{source}");
            // The error is on the whole `&&` or `||` expression.
            assert!(
                error.span.source_text(source).contains("&&")
                    || error.span.source_text(source).contains("||")
            );
        }
        let (function, body) = prepare("fn f(n: i64) -> bool { return n > 0 && n < 9 }");
        assert!(
            body.emit(
                &function,
                &BTreeMap::new(),
                &HashMap::new(),
                &HashMap::new()
            )
            .unwrap()
            .is_some()
        );
    }

    #[test]
    fn reachable_fallthrough_requires_a_return() {
        let (complete, body) =
            prepare("fn choose(n: i64) -> i64 { if n > 1 { return n }\nreturn 1 }");
        assert!(
            body.emit(
                &complete,
                &BTreeMap::new(),
                &HashMap::new(),
                &HashMap::new()
            )
            .is_ok()
        );
        let (incomplete, body) = prepare("fn choose(n: i64) -> i64 { if n > 1 { return n } }");
        let diagnostic = body
            .emit(
                &incomplete,
                &BTreeMap::new(),
                &HashMap::new(),
                &HashMap::new(),
            )
            .unwrap_err();
        assert_eq!(diagnostic.code, "E103");
        assert_eq!(diagnostic.span, incomplete.span);
    }
}
