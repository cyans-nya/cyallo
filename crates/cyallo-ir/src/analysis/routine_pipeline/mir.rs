use super::{Scalar, ScalarType, Variable, hir};
use cyallo_ast::{Operator, Span};

#[derive(Clone, Copy, Debug, Hash)]
pub(super) struct Value(pub(super) usize);

#[derive(Clone, Copy, Debug, Hash)]
pub(super) struct BlockId(pub(super) usize);

#[derive(Clone, Copy, Debug, Hash)]
pub(super) struct Located<T> {
    pub(super) span: Span,
    pub(super) kind: T,
}

#[derive(Debug, Hash)]
pub(super) struct Body {
    pub(super) blocks: Vec<Block>,
    pub(super) value_count: usize,
    /// Locals after the parameters, with hidden loop locals, in slot order.
    pub(super) locals: Vec<LocalInfo>,
    /// Reference sites in source order. MIR never stores a resolved target.
    pub(super) sites: Vec<ReferenceSite>,
    /// Ordered call arguments, addressed by `Operands`.
    pub(super) operands: Vec<Value>,
    /// The source span of each operand, parallel to `operands`.
    pub(super) operand_spans: Vec<Span>,
}

#[derive(Clone, Debug, Hash)]
pub(super) struct LocalInfo {
    pub(super) name: Box<str>,
    pub(super) span: Span,
    pub(super) constant: bool,
}

/// A use of a name whose meaning depends on declarations outside this body.
#[derive(Clone, Debug, Hash)]
pub(super) struct ReferenceSite {
    pub(super) path: Box<str>,
    pub(super) span: Span,
    /// Number of ordered arguments written at the site.
    pub(super) arguments: usize,
    /// The call is a statement: the callee is a procedure and no value is used.
    pub(super) discard: bool,
}

#[derive(Clone, Copy, Debug, Hash)]
pub(super) struct SiteId(pub(super) usize);

/// Half-open range into `Body::operands`.
#[derive(Clone, Copy, Debug, Hash)]
pub(super) struct Operands {
    pub(super) start: usize,
    pub(super) len: usize,
}

#[derive(Debug, Hash)]
pub(super) struct Block {
    pub(super) operations: Vec<Located<Operation>>,
    pub(super) exit: Located<Exit>,
}

#[derive(Clone, Copy, Debug, Hash)]
pub(super) enum Operation {
    Statement,
    Constant(Value, Scalar),
    Read(Value, Variable),
    Binary(Value, Operator, Value, Value),
    Call(Value, SiteId, Operands),
    Unary(Value, Operator, Value),
    /// Check that an operand of `&&` or `||` is boolean. Produces no value.
    Condition(Value),
    /// A call to a procedure for its effects.
    CallStatement(SiteId, Operands),
    /// Declare a local with its initial value; checks an annotation.
    Define(Variable, Value, Span, Option<ScalarType>),
    /// Store into a declared local; the span is the stored expression's.
    Assign(Variable, Value, Span),
    /// Check that two range bounds are integers of one type. Produces no value.
    RangeBounds(Value, Value, Span, Span),
}

#[derive(Clone, Copy, Debug, Hash)]
pub(super) enum Exit {
    Branch {
        condition: Value,
        condition_span: Span,
        yes: BlockId,
        no: BlockId,
    },
    Jump(BlockId),
    Next(BlockId),
    ReturnVoid,
    Return {
        value: Value,
        value_span: Span,
    },
    End,
}

pub(super) fn lower(routine: &hir::Routine, span: Span) -> Body {
    let statements = &routine.statements;
    let mut builder = Builder {
        loops: Vec::new(),
        blocks: Vec::new(),
        operations: Vec::new(),
        value_count: 0,
        sites: Vec::new(),
        operands: Vec::new(),
        operand_spans: Vec::new(),
        last_span: span,
    };
    builder.statements(statements);
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
        value_count: builder.value_count,
        locals: routine
            .locals
            .iter()
            .map(|local| LocalInfo {
                name: local.name.clone(),
                span: local.span,
                constant: local.constant,
            })
            .collect(),
        sites: builder.sites,
        operands: builder.operands,
        operand_spans: builder.operand_spans,
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
    operations: Vec<Located<Operation>>,
    value_count: usize,
    sites: Vec<ReferenceSite>,
    operands: Vec<Value>,
    operand_spans: Vec<Span>,
    last_span: Span,
}

struct OpenBlock {
    id: BlockId,
    operations: Vec<Located<Operation>>,
}

impl OpenBlock {
    fn finish(self, builder: &mut Builder, exit: Exit, span: Span) {
        builder.blocks[self.id.0] = Some(Block {
            operations: self.operations,
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
            operations: std::mem::take(&mut self.operations),
        }
    }

    fn emit(&mut self, kind: Operation, span: Span) {
        self.last_span = span;
        self.operations.push(Located { kind, span });
    }

    fn expression(&mut self, expression: &hir::Expression) -> Value {
        cyallo_ast::with_stack(|| {
            // Keep the left-to-right evaluation order even when an operation can trap.
            let operation = match &expression.kind {
                hir::ExpressionKind::Group(inner) => return self.expression(inner),
                hir::ExpressionKind::Constant(value) => {
                    Operation::Constant(self.new_value(), *value)
                }
                hir::ExpressionKind::Read(parameter) => {
                    Operation::Read(self.new_value(), *parameter)
                }
                hir::ExpressionKind::Binary(operator, left, right) => {
                    let left = self.expression(left);
                    let right = self.expression(right);
                    Operation::Binary(self.new_value(), *operator, left, right)
                }
                hir::ExpressionKind::Unary(operator, operand) => {
                    let operand = self.expression(operand);
                    Operation::Unary(self.new_value(), *operator, operand)
                }
                hir::ExpressionKind::ShortCircuit {
                    and,
                    left,
                    right,
                    slot,
                } => return self.short_circuit(*and, left, right, *slot, expression.span),
                hir::ExpressionKind::Call(path, arguments) => {
                    // Arguments run left to right before the call.
                    let values: Vec<Value> = arguments
                        .iter()
                        .map(|argument| self.expression(argument))
                        .collect();
                    let argument_count = values.len();
                    let operands = Operands {
                        start: self.operands.len(),
                        len: values.len(),
                    };
                    self.operands.extend(values);
                    self.operand_spans
                        .extend(arguments.iter().map(|argument| argument.span));
                    let site = SiteId(self.sites.len());
                    self.sites.push(ReferenceSite {
                        path: path.clone(),
                        span: expression.span,
                        arguments: argument_count,
                        discard: false,
                    });
                    Operation::Call(self.new_value(), site, operands)
                }
            };
            let value = match operation {
                Operation::Constant(value, _)
                | Operation::Read(value, _)
                | Operation::Binary(value, ..)
                | Operation::Unary(value, ..)
                | Operation::Call(value, ..) => value,
                _ => unreachable!("expressions produce values"),
            };
            self.emit(operation, expression.span);
            value
        })
    }

    /// An empty range skips the body. An exclusive bound becomes inclusive by
    /// stepping back once, after the emptiness test so the step cannot overflow.
    /// The counter stops on equality and never steps past the bound.
    fn range(&mut self, range: &hir::ForRange, span: Span) {
        let start = self.expression(&range.start);
        let finish = self.expression(&range.finish);
        self.emit(
            Operation::RangeBounds(start, finish, range.iterable_span, range.finish.span),
            span,
        );
        self.emit(Operation::Define(range.variable, start, span, None), span);
        self.emit(Operation::Define(range.end, finish, span, None), span);
        let empty = self.new_value();
        let empty_test = if range.inclusive {
            Operator::Greater
        } else {
            Operator::GreaterEqual
        };
        self.emit(Operation::Binary(empty, empty_test, start, finish), span);
        let skip = self.take_block();
        // Non-empty: fix the last value to visit.
        let end = self.new_value();
        self.emit(Operation::Read(end, range.end), span);
        let bound = if range.inclusive {
            end
        } else {
            let one = self.constant_one(span);
            let bound = self.new_value();
            self.emit(Operation::Binary(bound, Operator::Subtract, end, one), span);
            bound
        };
        self.emit(Operation::Define(range.bound, bound, span, None), span);
        let enter = self.take_block();
        let head = self.next_block();
        self.loops.push(LoopFrame::default());
        self.statements(&range.body);
        // A `continue` lands on the test, which therefore starts its own block.
        let body_end_span = self.last_span;
        let body_end = self.take_block();
        let test_start = self.next_block();
        body_end.finish(self, Exit::Next(test_start), body_end_span);
        // The test after the body: stop on the last value, else step and repeat.
        let current = self.new_value();
        self.emit(Operation::Read(current, range.variable), span);
        let last = self.new_value();
        self.emit(Operation::Read(last, range.bound), span);
        let finished = self.new_value();
        self.emit(
            Operation::Binary(finished, Operator::Equal, current, last),
            span,
        );
        let test_span = self.last_span;
        let test = self.take_block();
        let advance = self.next_block();
        let again = self.new_value();
        self.emit(Operation::Read(again, range.variable), span);
        let one = self.constant_one(span);
        let next = self.new_value();
        self.emit(Operation::Binary(next, Operator::Add, again, one), span);
        self.emit(Operation::Assign(range.variable, next, span), span);
        let jump_span = self.last_span;
        let back = self.take_block();
        let exit = self.next_block();
        self.finish_loop(test_start, exit);
        skip.finish(
            self,
            Exit::Branch {
                condition: empty,
                condition_span: range.finish.span,
                yes: exit,
                no: BlockId(enter.id.0),
            },
            span,
        );
        enter.finish(self, Exit::Next(head), span);
        test.finish(
            self,
            Exit::Branch {
                condition: finished,
                condition_span: span,
                yes: exit,
                no: advance,
            },
            test_span,
        );
        back.finish(self, Exit::Jump(head), jump_span);
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

    /// `left && right` / `left || right`. The result lives in a hidden local,
    /// because the right side runs in its own block and values do not cross blocks.
    fn short_circuit(
        &mut self,
        and: bool,
        left: &hir::Expression,
        right: &hir::Expression,
        slot: Variable,
        span: Span,
    ) -> Value {
        let decided = self.expression(left);
        self.emit(Operation::Condition(decided), span);
        self.emit(Operation::Define(slot, decided, left.span, None), span);
        let branch_span = self.last_span;
        let test = self.take_block();
        let rhs = self.next_block();
        let other = self.expression(right);
        self.emit(Operation::Condition(other), span);
        self.emit(Operation::Assign(slot, other, right.span), span);
        let rhs_span = self.last_span;
        let rhs_end = self.take_block();
        let end = self.next_block();
        let (yes, no) = if and { (rhs, end) } else { (end, rhs) };
        test.finish(
            self,
            Exit::Branch {
                condition: decided,
                condition_span: left.span,
                yes,
                no,
            },
            branch_span,
        );
        rhs_end.finish(self, Exit::Next(end), rhs_span);
        let result = self.new_value();
        self.emit(Operation::Read(result, slot), span);
        result
    }

    fn constant_one(&mut self, span: Span) -> Value {
        let one = self.new_value();
        self.emit(Operation::Constant(one, Scalar::Integer(1)), span);
        one
    }

    fn new_value(&mut self) -> Value {
        let value = Value(self.value_count);
        self.value_count += 1;
        value
    }

    fn statements(&mut self, statements: &[hir::Statement]) {
        cyallo_ast::with_stack(|| {
            for statement in statements {
                // A loop header runs its marker on every iteration instead.
                if !matches!(statement.kind, hir::StatementKind::While(..)) {
                    self.emit(Operation::Statement, statement.span);
                }
                match &statement.kind {
                    hir::StatementKind::Let {
                        slot,
                        value,
                        annotation,
                    } => {
                        let stored = self.expression(value);
                        self.emit(
                            Operation::Define(*slot, stored, value.span, *annotation),
                            statement.span,
                        );
                    }
                    hir::StatementKind::Assign { slot, value } => {
                        let stored = self.expression(value);
                        self.emit(Operation::Assign(*slot, stored, value.span), statement.span);
                    }
                    hir::StatementKind::Compound {
                        slot,
                        operator,
                        value,
                    } => {
                        let old = self.new_value();
                        self.emit(Operation::Read(old, *slot), statement.span);
                        let operand = self.expression(value);
                        let result = self.new_value();
                        self.emit(
                            Operation::Binary(result, *operator, old, operand),
                            statement.span,
                        );
                        self.emit(
                            Operation::Assign(*slot, result, statement.span),
                            statement.span,
                        );
                    }
                    hir::StatementKind::While(condition, body) => {
                        let before = self.take_block();
                        let header = self.next_block();
                        before.finish(self, Exit::Next(header), statement.span);
                        self.emit(Operation::Statement, statement.span);
                        let value = self.expression(condition);
                        let branch_span = self.last_span;
                        let test = self.take_block();
                        let yes = self.next_block();
                        self.loops.push(LoopFrame::default());
                        self.statements(body);
                        let back_span = self.last_span;
                        let back = self.take_block();
                        let no = self.next_block();
                        self.finish_loop(header, no);
                        test.finish(
                            self,
                            Exit::Branch {
                                condition: value,
                                condition_span: condition.span,
                                yes,
                                no,
                            },
                            branch_span,
                        );
                        back.finish(self, Exit::Jump(header), back_span);
                    }
                    hir::StatementKind::ForRange(range) => self.range(range, statement.span),
                    hir::StatementKind::ReturnVoid => {
                        let span = statement.span;
                        self.take_block().finish(self, Exit::ReturnVoid, span);
                    }
                    hir::StatementKind::CallStatement(call) => {
                        let hir::ExpressionKind::Call(path, arguments) = &call.kind else {
                            unreachable!("a call statement holds a call");
                        };
                        let values: Vec<Value> = arguments
                            .iter()
                            .map(|argument| self.expression(argument))
                            .collect();
                        let operands = Operands {
                            start: self.operands.len(),
                            len: values.len(),
                        };
                        let count = values.len();
                        self.operands.extend(values);
                        self.operand_spans
                            .extend(arguments.iter().map(|argument| argument.span));
                        let site = SiteId(self.sites.len());
                        self.sites.push(ReferenceSite {
                            path: path.clone(),
                            span: call.span,
                            arguments: count,
                            discard: true,
                        });
                        self.emit(Operation::CallStatement(site, operands), call.span);
                    }
                    hir::StatementKind::Break => {
                        let open = self.take_block();
                        let span = self.last_span;
                        if let Some(frame) = self.loops.last_mut() {
                            frame.breaks.push((open, span));
                        }
                    }
                    hir::StatementKind::Continue => {
                        let open = self.take_block();
                        let span = self.last_span;
                        if let Some(frame) = self.loops.last_mut() {
                            frame.continues.push((open, span));
                        }
                    }
                    hir::StatementKind::Return(expression) => {
                        let value = self.expression(expression);
                        let span = self.last_span;
                        self.take_block().finish(
                            self,
                            Exit::Return {
                                value,
                                value_span: expression.span,
                            },
                            span,
                        );
                    }
                    hir::StatementKind::If(expression, yes_arm, no_arm) => {
                        let condition = self.expression(expression);
                        let branch_span = self.last_span;
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
                        branch.finish(
                            self,
                            Exit::Branch {
                                condition,
                                condition_span: expression.span,
                                yes,
                                no,
                            },
                            branch_span,
                        );
                        skip.finish(self, Exit::Jump(join), skip_span);
                        no_exit.finish(self, Exit::Next(join), no_exit_span);
                    }
                }
            }
        });
    }
}
