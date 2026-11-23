//! File-local control flow: blocks of statements joined by explicit exits. Expressions
//! stay trees, so typed analysis can infer upward and push expected types downward
//! exactly as the existing route does. Nothing here depends on an imported declaration.
use super::{Variable, hir};
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
    pub(super) name: Box<str>,
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
