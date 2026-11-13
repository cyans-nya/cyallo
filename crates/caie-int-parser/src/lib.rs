//! Cambridge compatibility parser. It emits the common source AST.
mod lexer;
mod token;
use cyallo_ast::*;
pub use lexer::{Lexed, lex};
pub use token::{Token, TokenKind};
pub fn parse(source: &str) -> Result<Ast, Diagnostic> {
    parse_mode(source, false)
}
pub fn parse_repl(source: &str) -> Result<Ast, Diagnostic> {
    parse_mode(source, true)
}
fn parse_mode(source: &str, expression_statements: bool) -> Result<Ast, Diagnostic> {
    Parser {
        tokens: lex(source)?,
        pos: 0,
        expression_statements,
        source_names: Default::default(),
        depth: 0,
    }
    .program()
}
struct Parser<'a> {
    tokens: Lexed<'a>,
    pos: usize,
    expression_statements: bool,
    source_names: std::collections::HashMap<Box<str>, Box<str>>,
    /// Syntactic recursion depth, bounded by [`MAX_DEPTH`].
    depth: usize,
}
/// Deeper nesting is a diagnostic rather than an exhausted stack.
const MAX_DEPTH: usize = 256;
impl Parser<'_> {
    fn nested<T>(
        &mut self,
        parse: impl FnOnce(&mut Self) -> Result<T, Diagnostic>,
    ) -> Result<T, Diagnostic> {
        if self.depth >= MAX_DEPTH {
            return Err(Diagnostic::new(
                "E002",
                format!("nesting exceeds the limit of {MAX_DEPTH} levels"),
                self.token().span,
            ));
        }
        self.depth += 1;
        let result = cyallo_ast::with_stack(|| parse(self));
        self.depth -= 1;
        result
    }
    fn token(&self) -> &Token {
        &self.tokens[self.pos]
    }
    fn token_text(&self) -> &str {
        self.tokens.text(*self.token())
    }
    fn finish_span(&self, start: Span) -> Span {
        Span::new(
            start.start,
            self.tokens[self.pos.saturating_sub(1)].span.end,
        )
    }
    fn magnitude(&self) -> Result<u64, Diagnostic> {
        self.token_text()
            .parse()
            .map_err(|_| self.error("INTEGER literal out of range"))
    }
    fn at(&self, s: &str) -> bool {
        TokenKind::from_spelling(s) == Some(self.token().kind)
    }
    fn take(&mut self, s: &str) -> bool {
        if self.at(s) {
            self.pos += 1;
            true
        } else {
            false
        }
    }
    fn error(&self, s: impl Into<String>) -> Diagnostic {
        Diagnostic::new("E002", s, self.token().span)
    }
    fn expect(&mut self, s: &str) -> Result<(), Diagnostic> {
        if self.take(s) {
            Ok(())
        } else {
            Err(self.error(format!("expected {s}")))
        }
    }
    fn name(&mut self) -> Result<Box<str>, Diagnostic> {
        if self.token().kind != TokenKind::Identifier && !self.token().kind.modern_keyword() {
            return Err(self.error("expected identifier"));
        }
        let n = self.remember_identifier();
        self.pos += 1;
        Ok(n)
    }
    fn remember_identifier(&mut self) -> Box<str> {
        let original = self.token_text().to_owned();
        let key: Box<str> = original.to_ascii_uppercase().into();
        self.source_names
            .entry(key.clone())
            .or_insert_with(|| original.into());
        key
    }
    fn newlines(&mut self) {
        while self.token().kind == TokenKind::Newline {
            self.pos += 1;
        }
    }
    fn eol(&mut self) -> Result<(), Diagnostic> {
        if self.token().kind == TokenKind::Eof {
            return Ok(());
        }
        if self.token().kind != TokenKind::Newline {
            return Err(self.error("expected end of line"));
        }
        self.newlines();
        Ok(())
    }
    fn program(mut self) -> Result<Ast, Diagnostic> {
        let mut ast = Ast::default();
        self.newlines();
        while self.token().kind != TokenKind::Eof {
            if self.at("PROCEDURE") || self.at("FUNCTION") {
                ast.functions.push(self.function()?);
            } else {
                ast.body.push(self.statement()?);
            }
        }
        ast.source_names = self.source_names;
        Ok(ast)
    }
    fn ty(&mut self) -> Result<Type, Diagnostic> {
        for (word, ty) in [
            ("INTEGER", Type::Integer),
            ("REAL", Type::Real),
            ("BOOLEAN", Type::Boolean),
            ("CHAR", Type::Char),
            ("STRING", Type::String),
        ] {
            if self.take(word) {
                return Ok(ty);
            }
        }
        if self.take("ARRAY") {
            self.expect("[")?;
            let mut bounds = vec![];
            loop {
                let low = self.signed_int()?;
                self.expect(":")?;
                let high = self.signed_int()?;
                if high < low {
                    return Err(self.error("array upper bound precedes lower bound"));
                }
                bounds.push((low, high));
                if !self.take(",") {
                    break;
                }
            }
            self.expect("]")?;
            self.expect("OF")?;
            return Ok(Type::Array(bounds, Box::new(self.ty()?)));
        }
        Err(self.error("unsupported or unknown type"))
    }
    fn signed_int(&mut self) -> Result<i64, Diagnostic> {
        let span = self.token().span;
        let neg = self.take("-");
        if self.token().kind == TokenKind::Integer {
            let n = self.magnitude()?;
            self.pos += 1;
            let value = if neg { -(n as i128) } else { n as i128 };
            i64::try_from(value).map_err(|_| {
                Diagnostic::new(
                    "E001",
                    "INTEGER literal out of range",
                    self.finish_span(span),
                )
            })
        } else {
            Err(self.error("expected constant integer bound"))
        }
    }
    fn function(&mut self) -> Result<Function, Diagnostic> {
        let span = self.token().span;
        let is_function = self.take("FUNCTION");
        if !is_function {
            self.expect("PROCEDURE")?;
        }
        let name = self.name()?;
        self.expect("(")?;
        let mut params = vec![];
        let mut by_ref = false;
        if !self.at(")") {
            loop {
                let pspan = self.token().span;
                if self.take("BYREF") {
                    by_ref = true;
                } else if self.take("BYVAL") {
                    by_ref = false;
                }
                let pname = self.name()?;
                self.expect(":")?;
                let ty = self.ty()?;
                params.push(Param {
                    receiver: false,
                    name: pname,
                    ty,
                    by_ref,
                    span: self.finish_span(pspan),
                });
                if !self.take(",") {
                    break;
                }
            }
        }
        self.expect(")")?;
        let result = if is_function {
            self.expect("RETURNS")?;
            self.ty()?
        } else {
            Type::Void
        };
        self.eol()?;
        let end = if is_function {
            "ENDFUNCTION"
        } else {
            "ENDPROCEDURE"
        };
        let body = self.block(&[end])?;
        self.expect(end)?;
        let span = self.finish_span(span);
        self.eol()?;
        Ok(Function {
            generator: false,
            origin: RoutineOrigin::Source,
            attributes: Default::default(),
            type_params: vec![],
            kind: if is_function {
                RoutineKind::Function
            } else {
                RoutineKind::Procedure
            },
            source_name: self
                .source_names
                .get(&name)
                .cloned()
                .unwrap_or_else(|| name.clone()),
            name,
            params,
            result,
            body,
            span,
        })
    }
    fn block(&mut self, ends: &[&str]) -> Result<Vec<Stmt>, Diagnostic> {
        self.nested(|parser| parser.block_inner(ends))
    }
    fn block_inner(&mut self, ends: &[&str]) -> Result<Vec<Stmt>, Diagnostic> {
        let mut b = vec![];
        self.newlines();
        while !ends.iter().any(|e| self.at(e)) {
            if self.token().kind == TokenKind::Eof {
                return Err(self.error(format!("expected {}", ends.join(" or "))));
            }
            b.push(self.statement()?);
        }
        Ok(b)
    }
    fn statement(&mut self) -> Result<Stmt, Diagnostic> {
        let span = self.token().span;
        let kind = if self.take("DECLARE") {
            let n = self.name()?;
            self.expect(":")?;
            StmtKind::Declare(n, self.ty()?)
        } else if self.take("CONSTANT") {
            let n = self.name()?;
            self.expect("=")?;
            StmtKind::Constant(n, self.expr(0)?)
        } else if self.take("OUTPUT") {
            StmtKind::Output(self.expr_list()?)
        } else if self.take("INPUT") {
            StmtKind::Input(self.expr(0)?)
        } else if self.take("IF") {
            let e = self.expr(0)?;
            if self.token().kind == TokenKind::Newline {
                self.newlines();
            }
            self.expect("THEN")?;
            self.eol()?;
            let a = self.block(&["ELSE", "ENDIF"])?;
            let b = if self.take("ELSE") {
                self.eol()?;
                self.block(&["ENDIF"])?
            } else {
                vec![]
            };
            self.expect("ENDIF")?;
            StmtKind::If(e, a, b)
        } else if self.take("WHILE") {
            let e = self.expr(0)?;
            self.eol()?;
            let b = self.block(&["ENDWHILE"])?;
            self.expect("ENDWHILE")?;
            StmtKind::While(e, b)
        } else if self.take("REPEAT") {
            self.eol()?;
            let b = self.block(&["UNTIL"])?;
            self.expect("UNTIL")?;
            StmtKind::Repeat(b, self.expr(0)?)
        } else if self.take("FOR") {
            let n = self.name()?;
            self.expect("<-")?;
            let a = self.expr(0)?;
            self.expect("TO")?;
            let z = self.expr(0)?;
            let step = if self.take("STEP") {
                Some(self.expr(0)?)
            } else {
                None
            };
            self.eol()?;
            let b = self.block(&["NEXT"])?;
            self.expect("NEXT")?;
            if self.token().kind == TokenKind::Identifier {
                let end_name = self.name()?;
                if !end_name.eq_ignore_ascii_case(&n) {
                    return Err(self.error("NEXT variable must match FOR"));
                }
            }
            StmtKind::For(n, a, z, step, b)
        } else if self.take("CASE") {
            self.expect("OF")?;
            let e = self.expr(0)?;
            self.eol()?;
            let mut arms = vec![];
            let mut fallback = vec![];
            while !self.at("ENDCASE") {
                if self.take("OTHERWISE") {
                    self.take(":");
                    self.newlines();
                    fallback = self.block(&["ENDCASE"])?;
                    break;
                }
                let label = self.expr(0)?;
                let upper = if self.take("TO") {
                    Some(self.expr(0)?)
                } else {
                    None
                };
                self.expect(":")?;
                let mut body = vec![];
                self.newlines();
                while !self.at("ENDCASE") && !self.at("OTHERWISE") && !self.case_label() {
                    body.push(self.statement()?);
                }
                arms.push((
                    CaseLabel {
                        lower: label,
                        upper,
                    },
                    body,
                ));
            }
            self.expect("ENDCASE")?;
            StmtKind::Case(e, arms, fallback)
        } else if self.take("CALL") {
            let n = self.name()?;
            self.expect("(")?;
            let args = self.args()?;
            StmtKind::Call(n, args)
        } else if self.take("RETURN") {
            StmtKind::Return(self.expr(0)?)
        } else {
            let start = self.pos;
            let target = self.expr(0)?;
            if self.take("<-") {
                StmtKind::Assign(target, self.expr(0)?)
            } else if self.expression_statements {
                self.pos = start;
                StmtKind::Output(vec![self.expr(0)?])
            } else {
                return Err(self.error("expected <-"));
            }
        };
        let span = self.finish_span(span);
        self.eol()?;
        Ok(Stmt { kind, span })
    }
    fn case_label(&self) -> bool {
        if self.token().kind.is_keyword()
            && !matches!(
                self.token().kind,
                TokenKind::True | TokenKind::False | TokenKind::Not
            )
        {
            return false;
        }
        for token in &self.tokens[self.pos..] {
            match token.kind {
                TokenKind::Colon => return true,
                TokenKind::Newline | TokenKind::Eof => return false,
                _ => {}
            }
        }
        false
    }
    fn expr_list(&mut self) -> Result<Vec<Expr>, Diagnostic> {
        let mut v = vec![self.expr(0)?];
        while self.take(",") {
            v.push(self.expr(0)?);
        }
        Ok(v)
    }
    fn args(&mut self) -> Result<Vec<Expr>, Diagnostic> {
        if self.take(")") {
            return Ok(vec![]);
        }
        let v = self.expr_list()?;
        self.expect(")")?;
        Ok(v)
    }
    fn expr(&mut self, min: u8) -> Result<Expr, Diagnostic> {
        self.nested(|parser| parser.expr_inner(min))
    }
    fn expr_inner(&mut self, min: u8) -> Result<Expr, Diagnostic> {
        let span = self.token().span;
        let mut lhs = if self.take("-") {
            if self.token().kind == TokenKind::Integer {
                let n = self.magnitude()?;
                self.pos += 1;
                Expr {
                    kind: ExprKind::Literal(Literal::Integer(
                        i64::try_from(-(n as i128))
                            .map_err(|_| self.error("INTEGER literal out of range"))?,
                    )),
                    span: self.finish_span(span),
                }
            } else {
                Expr {
                    kind: ExprKind::Unary(Operator::Subtract, Box::new(self.expr(7)?)),
                    span,
                }
            }
        } else if self.take("+") {
            Expr {
                kind: ExprKind::Unary(Operator::Add, Box::new(self.expr(7)?)),
                span,
            }
        } else if self.take("NOT") {
            Expr {
                kind: ExprKind::Unary(Operator::Not, Box::new(self.expr(7)?)),
                span,
            }
        } else if self.take("(") {
            let e = self.expr(0)?;
            self.expect(")")?;
            Expr {
                kind: ExprKind::Parenthesized(Box::new(e)),
                span: self.finish_span(span),
            }
        } else {
            let kind = match self.token().kind {
                TokenKind::Integer => ExprKind::Literal(Literal::Integer(
                    i64::try_from(self.magnitude()?)
                        .map_err(|_| self.error("INTEGER literal out of range"))?,
                )),
                TokenKind::Real => ExprKind::Literal(Literal::Real(
                    self.token_text()
                        .parse::<f64>()
                        .ok()
                        .filter(|n| n.is_finite())
                        .ok_or_else(|| self.error("REAL literal out of range"))?,
                )),
                TokenKind::Text => ExprKind::Literal(Literal::String(
                    self.token_text()[1..self.token_text().len() - 1].into(),
                )),
                TokenKind::Char => ExprKind::Literal(Literal::Char(
                    self.token_text()[1..self.token_text().len() - 1]
                        .chars()
                        .next()
                        .ok_or_else(|| self.error("empty CHAR"))?,
                )),
                TokenKind::True | TokenKind::False => {
                    ExprKind::Literal(Literal::Boolean(self.token().kind == TokenKind::True))
                }
                k if k == TokenKind::Identifier || k.modern_keyword() => {
                    ExprKind::Name(self.remember_identifier())
                }
                _ => return Err(self.error("expected expression")),
            };
            self.pos += 1;
            Expr {
                kind,
                span: self.finish_span(span),
            }
        };
        loop {
            if self.take("[") {
                let indices = self.expr_list()?;
                self.expect("]")?;
                lhs = Expr {
                    kind: ExprKind::Index(Box::new(lhs), indices),
                    span: self.finish_span(span),
                };
                continue;
            }
            if self.take("(") {
                let ExprKind::Name(name) = lhs.kind else {
                    return Err(self.error("only named functions can be called"));
                };
                let args = self.args()?;
                lhs = Expr {
                    kind: ExprKind::Call(name, args),
                    span: self.finish_span(span),
                };
                continue;
            }
            if self.token().kind.modern_operator() {
                return Err(self.error("operator requires modern source"));
            }
            let Some((op, prec)) = self.token().kind.binary_op() else {
                break;
            };
            if prec < min {
                break;
            }
            self.pos += 1;
            lhs = Expr {
                kind: ExprKind::Binary(op, Box::new(lhs), Box::new(self.expr(prec + 1)?)),
                span: self.finish_span(span),
            };
        }
        Ok(lhs)
    }
}
