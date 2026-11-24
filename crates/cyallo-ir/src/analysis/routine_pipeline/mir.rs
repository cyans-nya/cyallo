//! File-local control flow: blocks of statements joined by explicit exits. Expressions
//! stay trees, so typed analysis can infer upward and push expected types downward
//! exactly as the existing route does. Nothing here depends on an imported declaration.
use super::{LocalName, Variable, hir};
use cyallo_ast::Span;
use hir::Expression;

#[derive(Clone, Copy, Debug)]
pub(super) struct BlockId(pub(super) usize);

#[derive(Clone, Debug)]
pub(super) struct Located<T> {
    pub(super) span: Span,
    pub(super) kind: T,
}

#[derive(Clone, Debug)]
pub(super) struct LocalInfo {
    pub(super) name: LocalName,
    pub(super) span: Span,
    pub(super) constant: bool,
}

#[derive(Debug)]
pub(super) struct Body {
    pub(super) blocks: Vec<Block>,
    /// Locals after the parameters, with hidden loop locals, in slot order.
    pub(super) locals: Vec<LocalInfo>,
}

#[derive(Debug)]
pub(super) struct Block {
    pub(super) statements: Vec<Located<Statement>>,
    pub(super) exit: Located<Exit>,
}

#[derive(Debug)]
pub(super) enum Statement {
    /// A source statement boundary: a logical instruction of its own.
    Marker,
    /// Declare a local with its initial value; checks an annotation.
    Define {
        slot: Variable,
        value: Expression,
        annotation: Option<cyallo_ast::Type>,
    },
    Assign {
        slot: Variable,
        value: Expression,
    },
    /// `slot op= value`.
    Compound {
        slot: Variable,
        operator: cyallo_ast::Operator,
        value: Expression,
    },
    /// `p.a.b = value`.
    AssignProject {
        root: Variable,
        path: Vec<super::hir::Step>,
        value: Expression,
    },
    /// `p.a.b op= value`.
    CompoundProject {
        root: Variable,
        path: Vec<super::hir::Step>,
        operator: cyallo_ast::Operator,
        value: Expression,
    },
    /// `p.* = value`, through a reference parameter.
    AssignDeref {
        variable: Variable,
        value: Expression,
    },
    /// `p.* op= value`.
    CompoundDeref {
        variable: Variable,
        operator: cyallo_ast::Operator,
        value: Expression,
    },
    /// A call to a procedure for its effects.
    CallStatement(Expression),
    /// Evaluate both bounds of a range loop into the counter and `end`.
    RangeStart {
        variable: Variable,
        end: Variable,
        start: Expression,
        finish: Expression,
        iterable_span: Span,
    },
    /// Fix the last value to visit: `end`, or one less for an exclusive range.
    RangeBound {
        end: Variable,
        bound: Variable,
        inclusive: bool,
    },
    /// Advance the counter by one.
    RangeStep {
        variable: Variable,
    },
    /// What a `match` statement's arms are: checked once, before any test, for unknown
    /// variants, payload shapes and exhaustiveness.
    MatchCover {
        held: Variable,
        arms: Vec<Cover>,
    },
    /// Bind the payload of variant `name` held in `held` to `slot`.
    MatchBind {
        held: Variable,
        name: cyallo_ast::indexed::NameId,
        slot: Variable,
        stage: Stage,
    },
    /// Read one tuple element or record field of `root` into `slot`.
    MatchProject {
        root: Variable,
        step: hir::Step,
        slot: Variable,
    },
    /// The arm has been chosen and its bindings made from parts of `held`: when one of
    /// them is not `Copy` the matched value is spent.
    MatchSpend {
        held: Variable,
        slots: Vec<Variable>,
    },
}

/// One arm of a `match` statement, for the coverage check.
#[derive(Debug)]
pub(super) struct Cover {
    pub(super) pattern: hir::Pattern,
    pub(super) guarded: bool,
    pub(super) span: Span,
}

/// When a binding is made, relative to the arm's guard.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Stage {
    /// No guard: bind, moving a payload that is not `Copy`.
    Direct,
    /// Before a guard: read the payload where it lies.
    Alias,
    /// After a guard chose the arm: move a payload that is not `Copy` into the binding.
    Take,
}

#[derive(Debug)]
pub(super) enum Exit {
    Branch {
        condition: Expression,
        yes: BlockId,
        no: BlockId,
    },
    Jump(BlockId),
    Next(BlockId),
    Return(Expression),
    ReturnVoid,
    End,
    /// The range has no values: `variable > end` (inclusive) or `variable >= end`.
    RangeEmpty {
        variable: Variable,
        end: Variable,
        inclusive: bool,
        yes: BlockId,
        no: BlockId,
    },
    /// Whether `held` is variant `name`.
    MatchTest {
        held: Variable,
        name: cyallo_ast::indexed::NameId,
        yes: BlockId,
        no: BlockId,
    },
    /// A guard: like a branch, but the alias binding of the arm is read, not moved.
    Guard {
        condition: Expression,
        alias: Vec<Variable>,
        yes: BlockId,
        no: BlockId,
    },
    /// No arm applied.
    Unreachable,
    /// The counter reached the last value to visit.
    RangeDone {
        variable: Variable,
        bound: Variable,
        yes: BlockId,
        no: BlockId,
    },
}

/// Consumes the routine: HIR is transient, so its expression trees move into MIR.
pub(super) fn lower(routine: hir::Routine, span: Span) -> Body {
    let mut builder = Builder {
        loops: Vec::new(),
        blocks: Vec::new(),
        statements: Vec::new(),
        last_span: span,
    };
    builder.statements(routine.statements);
    let end_span = builder.last_span;
    builder
        .take_block()
        .finish(&mut builder, Exit::End, end_span);
    Body {
        blocks: builder
            .blocks
            .into_iter()
            .map(|block| block.expect("each reserved block has an explicit exit"))
            .collect(),
        locals: routine
            .locals
            .into_iter()
            .map(|local| LocalInfo {
                name: local.name,
                span: local.span,
                constant: local.constant,
            })
            .collect(),
    }
}

/// Jumps out of the loop being lowered, finished once its blocks are known.
#[derive(Default)]
struct LoopFrame {
    breaks: Vec<(OpenBlock, Span)>,
    continues: Vec<(OpenBlock, Span)>,
}

struct Builder {
    loops: Vec<LoopFrame>,
    blocks: Vec<Option<Block>>,
    statements: Vec<Located<Statement>>,
    last_span: Span,
}

struct OpenBlock {
    id: BlockId,
    statements: Vec<Located<Statement>>,
}

impl OpenBlock {
    fn finish(self, builder: &mut Builder, exit: Exit, span: Span) {
        builder.blocks[self.id.0] = Some(Block {
            statements: self.statements,
            exit: Located { kind: exit, span },
        });
    }
}

impl Builder {
    fn next_block(&self) -> BlockId {
        BlockId(self.blocks.len())
    }

    fn take_block(&mut self) -> OpenBlock {
        let id = self.next_block();
        self.blocks.push(None);
        OpenBlock {
            id,
            statements: std::mem::take(&mut self.statements),
        }
    }

    fn emit(&mut self, kind: Statement, span: Span) {
        self.last_span = span;
        self.statements.push(Located { kind, span });
    }

    fn statements(&mut self, statements: Vec<hir::Statement>) {
        cyallo_ast::with_stack(|| {
            for statement in statements {
                let span = statement.span;
                // A loop header runs its marker on every iteration instead.
                if !matches!(statement.kind, hir::StatementKind::While(..)) {
                    self.emit(Statement::Marker, span);
                }
                match statement.kind {
                    hir::StatementKind::Let {
                        slot,
                        value,
                        annotation,
                    } => self.emit(
                        Statement::Define {
                            slot,
                            value,
                            annotation,
                        },
                        span,
                    ),
                    hir::StatementKind::Assign { slot, value } => {
                        self.emit(Statement::Assign { slot, value }, span)
                    }
                    hir::StatementKind::Compound {
                        slot,
                        operator,
                        value,
                    } => self.emit(
                        Statement::Compound {
                            slot,
                            operator,
                            value,
                        },
                        span,
                    ),
                    hir::StatementKind::AssignProject { root, path, value } => {
                        self.emit(Statement::AssignProject { root, path, value }, span)
                    }
                    hir::StatementKind::CompoundProject {
                        root,
                        path,
                        operator,
                        value,
                    } => self.emit(
                        Statement::CompoundProject {
                            root,
                            path,
                            operator,
                            value,
                        },
                        span,
                    ),
                    hir::StatementKind::AssignDeref { variable, value } => {
                        self.emit(Statement::AssignDeref { variable, value }, span)
                    }
                    hir::StatementKind::CompoundDeref {
                        variable,
                        operator,
                        value,
                    } => self.emit(
                        Statement::CompoundDeref {
                            variable,
                            operator,
                            value,
                        },
                        span,
                    ),
                    hir::StatementKind::CallStatement(call) => {
                        self.emit(Statement::CallStatement(call), span)
                    }
                    hir::StatementKind::Return(expression) => {
                        self.take_block()
                            .finish(self, Exit::Return(expression), span);
                    }
                    hir::StatementKind::ReturnVoid => {
                        self.take_block().finish(self, Exit::ReturnVoid, span);
                    }
                    hir::StatementKind::If(condition, yes_arm, no_arm) => {
                        let condition_span = condition.span;
                        let branch = self.take_block();
                        let yes = self.next_block();
                        self.statements(yes_arm);
                        let skip_span = self.last_span;
                        let skip = self.take_block();
                        let no = self.next_block();
                        self.statements(no_arm);
                        let no_exit_span = self.last_span;
                        let no_exit = self.take_block();
                        let join = self.next_block();
                        branch.finish(self, Exit::Branch { condition, yes, no }, condition_span);
                        skip.finish(self, Exit::Jump(join), skip_span);
                        no_exit.finish(self, Exit::Next(join), no_exit_span);
                    }
                    hir::StatementKind::While(condition, body) => {
                        let condition_span = condition.span;
                        let before = self.take_block();
                        let header = self.next_block();
                        before.finish(self, Exit::Next(header), span);
                        self.emit(Statement::Marker, span);
                        let test = self.take_block();
                        let yes = self.next_block();
                        self.loops.push(LoopFrame::default());
                        self.statements(body);
                        let back_span = self.last_span;
                        let back = self.take_block();
                        let no = self.next_block();
                        self.finish_loop(header, no);
                        test.finish(self, Exit::Branch { condition, yes, no }, condition_span);
                        back.finish(self, Exit::Jump(header), back_span);
                    }
                    hir::StatementKind::ForRange(range) => self.range(*range, span),
                    hir::StatementKind::Match(matched) => self.matching(*matched, span),
                    hir::StatementKind::Break => {
                        let open = self.take_block();
                        if let Some(frame) = self.loops.last_mut() {
                            frame.breaks.push((open, span));
                        }
                    }
                    hir::StatementKind::Continue => {
                        let open = self.take_block();
                        if let Some(frame) = self.loops.last_mut() {
                            frame.continues.push((open, span));
                        }
                    }
                }
            }
        });
    }

    /// A `match` statement: for each arm a test block per alternative, an optional guard
    /// block, and the body, which ends by jumping past the last arm. A test or guard that
    /// fails goes to the next alternative or arm, and falling off the last one is
    /// unreachable. The alternatives of an or-pattern each bind in their own block and
    /// meet before the guard and body.
    fn matching(&mut self, matched: hir::MatchStatement, span: Span) {
        let mut covers = Vec::new();
        for arm in &matched.arms {
            for pattern in &arm.patterns {
                covers.push(Cover {
                    pattern: pattern.clone(),
                    guarded: arm.guard.is_some(),
                    span: arm.span,
                });
            }
        }
        if !matched.in_place {
            self.emit(
                Statement::Assign {
                    slot: matched.held,
                    value: matched.scrutinee,
                },
                span,
            );
        }
        let held = matched.held;
        self.emit(Statement::MatchCover { held, arms: covers }, span);
        let mut fails: Vec<(OpenBlock, Exit, Span)> = Vec::new();
        let mut ends: Vec<(OpenBlock, Span)> = Vec::new();
        // Tests and guards of the arm being lowered, which continue with the next arm.
        let mut pending_no: Vec<usize> = Vec::new();
        for arm in matched.arms {
            let alternatives = arm.patterns.len();
            let mut to_shared: Vec<(OpenBlock, Span)> = Vec::new();
            // The tests of the previous alternative, which continue with the next one.
            let mut previous: Vec<usize> = Vec::new();
            // The bindings that take a payload, and every binding, of the last alternative.
            let mut takes: Vec<(Variable, cyallo_ast::indexed::NameId, Variable)> = Vec::new();
            let mut leaves: Vec<Variable> = Vec::new();
            let mut nested = false;
            for (index, pattern) in arm.patterns.into_iter().enumerate() {
                let last = index + 1 == alternatives;
                let steps = flatten(&pattern);
                takes.clear();
                leaves.clear();
                nested = false;
                for step in steps {
                    match step {
                        Part::Test { held, name } => {
                            let test = self.take_block();
                            let yes = self.next_block();
                            let at = fails.len();
                            fails.push((
                                test,
                                Exit::MatchTest {
                                    held,
                                    name,
                                    yes,
                                    no: yes,
                                },
                                arm.span,
                            ));
                            if last {
                                pending_no.push(at);
                            } else {
                                previous.push(at);
                            }
                        }
                        Part::Bind {
                            held,
                            name,
                            slot,
                            leaf,
                        } => {
                            // With a guard a payload is read where it lies, and taken
                            // afterwards; a part that is taken apart further is only read.
                            let stage = if !leaf || arm.guard.is_some() {
                                Stage::Alias
                            } else {
                                Stage::Direct
                            };
                            if leaf {
                                takes.push((held, name, slot));
                                leaves.push(slot);
                            }
                            nested |= held != matched.held || !leaf;
                            self.emit(
                                Statement::MatchBind {
                                    held,
                                    name,
                                    slot,
                                    stage,
                                },
                                arm.span,
                            );
                        }
                        Part::Project { root, step, slot, leaf } => {
                            if leaf {
                                leaves.push(slot);
                            }
                            nested = true;
                            self.emit(Statement::MatchProject { root, step, slot }, arm.span);
                        }
                    }
                }
                if nested && arm.guard.is_none() && !leaves.is_empty() {
                    self.emit(
                        Statement::MatchSpend {
                            held,
                            slots: leaves.clone(),
                        },
                        arm.span,
                    );
                }
                if alternatives > 1 {
                    let open = self.take_block();
                    if last {
                        let shared = self.next_block();
                        for (block, span) in to_shared.drain(..) {
                            block.finish(self, Exit::Jump(shared), span);
                        }
                        open.finish(self, Exit::Next(shared), arm.span);
                    } else {
                        to_shared.push((open, arm.span));
                        let next = self.next_block();
                        for at in previous.drain(..) {
                            if let Exit::MatchTest { no, .. } = &mut fails[at].1 {
                                *no = next;
                            }
                        }
                    }
                }
            }
            if let Some(guard) = arm.guard {
                let guard_span = guard.span;
                let block = self.take_block();
                let yes = self.next_block();
                pending_no.push(fails.len());
                fails.push((
                    block,
                    Exit::Guard {
                        condition: guard,
                        alias: leaves.clone(),
                        yes,
                        no: yes,
                    },
                    guard_span,
                ));
                for &(source, name, slot) in &takes {
                    self.emit(
                        Statement::MatchBind {
                            held: source,
                            name,
                            slot,
                            stage: Stage::Take,
                        },
                        arm.span,
                    );
                }
                if nested && !leaves.is_empty() {
                    self.emit(
                        Statement::MatchSpend {
                            held,
                            slots: leaves.clone(),
                        },
                        arm.span,
                    );
                }
            }
            self.statements(arm.body);
            let end_span = self.last_span;
            ends.push((self.take_block(), end_span));
            // Everything that failed in this arm continues with the next one.
            let next = self.next_block();
            for index in pending_no.drain(..) {
                match &mut fails[index].1 {
                    Exit::MatchTest { no, .. } | Exit::Guard { no, .. } => *no = next,
                    _ => unreachable!("only tests and guards are pending"),
                }
            }
        }
        let fall_off = self.take_block();
        let end = self.next_block();
        fall_off.finish(self, Exit::Unreachable, span);
        for (block, exit, span) in fails {
            block.finish(self, exit, span);
        }
        for (block, end_span) in ends {
            block.finish(self, Exit::Jump(end), end_span);
        }
    }

    /// An empty range skips the body. An exclusive bound becomes inclusive by
    /// stepping back once, after the emptiness test so the step cannot overflow.
    /// The counter stops on equality and never steps past the bound.
    fn range(&mut self, range: hir::ForRange, span: Span) {
        self.emit(
            Statement::RangeStart {
                variable: range.variable,
                end: range.end,
                start: range.start,
                finish: range.finish,
                iterable_span: range.iterable_span,
            },
            span,
        );
        let skip = self.take_block();
        self.emit(
            Statement::RangeBound {
                end: range.end,
                bound: range.bound,
                inclusive: range.inclusive,
            },
            span,
        );
        let enter = self.take_block();
        let head = self.next_block();
        self.loops.push(LoopFrame::default());
        self.statements(range.body);
        // A `continue` lands on the test, which therefore starts its own block.
        let body_end_span = self.last_span;
        let body_end = self.take_block();
        let test_start = self.next_block();
        body_end.finish(self, Exit::Next(test_start), body_end_span);
        let test = self.take_block();
        let advance = self.next_block();
        self.emit(
            Statement::RangeStep {
                variable: range.variable,
            },
            span,
        );
        let back = self.take_block();
        let exit = self.next_block();
        self.finish_loop(test_start, exit);
        let enter_id = enter.id;
        skip.finish(
            self,
            Exit::RangeEmpty {
                variable: range.variable,
                end: range.end,
                inclusive: range.inclusive,
                yes: exit,
                no: enter_id,
            },
            span,
        );
        enter.finish(self, Exit::Next(head), span);
        test.finish(
            self,
            Exit::RangeDone {
                variable: range.variable,
                bound: range.bound,
                yes: exit,
                no: advance,
            },
            span,
        );
        back.finish(self, Exit::Jump(head), span);
    }

    /// Finish the pending `break` and `continue` jumps of the innermost loop.
    fn finish_loop(&mut self, next: BlockId, exit: BlockId) {
        let frame = self.loops.pop().expect("a loop frame is open");
        for (open, span) in frame.continues {
            open.finish(self, Exit::Jump(next), span);
        }
        for (open, span) in frame.breaks {
            open.finish(self, Exit::Jump(exit), span);
        }
    }
}

/// One step of matching a pattern, in the order it runs.
enum Part {
    /// Whether `held` is variant `name`.
    Test {
        held: Variable,
        name: cyallo_ast::indexed::NameId,
    },
    /// Read the payload of variant `name` in `held` into `slot`: a name the pattern binds
    /// (`leaf`), or a hidden local for a payload that is taken apart further.
    Bind {
        held: Variable,
        name: cyallo_ast::indexed::NameId,
        slot: Variable,
        leaf: bool,
    },
    /// Read a tuple element or record field of `root` into `slot`, likewise.
    Project {
        root: Variable,
        step: hir::Step,
        slot: Variable,
        leaf: bool,
    },
}

/// A pattern as the tests and reads that match it, parts after the whole they belong to.
fn flatten(pattern: &hir::Pattern) -> Vec<Part> {
    fn walk(pattern: &hir::Pattern, out: &mut Vec<Part>) {
        match pattern {
            hir::Pattern::Wildcard | hir::Pattern::Binding(_) => {}
            hir::Pattern::Variant { name, payload, at } => {
                out.push(Part::Test {
                    held: *at,
                    name: *name,
                });
                match payload.as_deref() {
                    None | Some(hir::Pattern::Wildcard) => {}
                    Some(hir::Pattern::Binding(slot)) => out.push(Part::Bind {
                        held: *at,
                        name: *name,
                        slot: *slot,
                        leaf: true,
                    }),
                    Some(inner) => {
                        let inner_at = inner.at().expect("a pattern taken apart names its local");
                        out.push(Part::Bind {
                            held: *at,
                            name: *name,
                            slot: inner_at,
                            leaf: false,
                        });
                        walk(inner, out);
                    }
                }
            }
            hir::Pattern::Tuple { items, at } => {
                for (index, item) in items.iter().enumerate() {
                    part(*at, hir::Step::Index(index), item, out);
                }
            }
            hir::Pattern::Record { fields, at, .. } => {
                for (name, item) in fields {
                    part(*at, hir::Step::Field(*name), item, out);
                }
            }
        }
    }
    fn part(root: Variable, step: hir::Step, item: &hir::Pattern, out: &mut Vec<Part>) {
        match item {
            hir::Pattern::Wildcard => {}
            hir::Pattern::Binding(slot) => out.push(Part::Project {
                root,
                step,
                slot: *slot,
                leaf: true,
            }),
            inner => {
                let inner_at = inner.at().expect("a pattern taken apart names its local");
                out.push(Part::Project {
                    root,
                    step,
                    slot: inner_at,
                    leaf: false,
                });
                walk(inner, out);
            }
        }
    }
    let mut out = Vec::new();
    walk(pattern, &mut out);
    out
}
