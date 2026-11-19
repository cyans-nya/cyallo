#![feature(portable_simd)]
//! Source-preserving parser for the Cyallo language.
mod lexer;
mod modern;
mod modules;
mod patterns;
mod token;
mod units;
use cyallo_ast::*;
pub use lexer::{Lexed, lex, lex_modern};
#[doc(hidden)]
pub use lexer::{ScanVariant, lex_variant, scan};
pub use token::{Token, TokenKind};
pub use units::{
    BodyHandle, IndexedSource, ParseUnit, ParsedFile, ParsedUnit, SourceSnapshot, index_modern,
    parse_modern_snapshot, parse_modern_snapshots, parse_modern_with_jobs,
};
pub fn parse(source: &str) -> Result<Ast, Diagnostic> {
    parse_modern(source, false)
}
/// REPL entries may use bare expressions; their original source spans are retained.
pub fn parse_repl(source: &str) -> Result<Ast, Diagnostic> {
    parse_modern(source, true)
}
pub fn parse_modern(source: &str, repl: bool) -> Result<Ast, Diagnostic> {
    parse_modern_with_jobs(source, repl, cyallo_compile_jobs::Jobs::default())
}
#[cfg(test)]
fn parse_whole(source: &str, repl: bool) -> Result<Ast, Diagnostic> {
    let tokens = lex_modern(source)?;
    Parser {
        tokens: tokens.fragment(0..tokens.len() - 1),
        pos: 0,
        expression_statements: repl,
        nesting: 0,
        depth: 0,
        condition: false,
    }
    .program()
}
struct Parser<'tokens, 'src> {
    tokens: lexer::TokenWindow<'tokens, 'src>,
    pos: usize,
    expression_statements: bool,
    /// Open parentheses/brackets in the current expression; newlines are insignificant inside.
    nesting: usize,
    /// Syntactic recursion depth, bounded by [`MAX_DEPTH`].
    depth: usize,
    condition: bool,
}
/// Deeper nesting is rejected with a diagnostic instead of exhausting the stack.
/// Later passes recurse over the same tree, so this also bounds them.
const MAX_DEPTH: usize = 256;
/// Unary operands bind tighter than `as`, which binds tighter than every binary operator.
const CAST_PRECEDENCE: u8 = 7;
const UNARY_OPERAND: u8 = CAST_PRECEDENCE + 1;
impl<'src> Parser<'_, 'src> {
    /// Runs `parse` one level deeper, failing past [`MAX_DEPTH`].
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
    fn finish_span(&self, start: Span) -> Span {
        Span::new(
            start.start,
            self.tokens[self.pos.saturating_sub(1)].span.end,
        )
    }
    fn token(&self) -> &Token {
        &self.tokens[self.pos]
    }
    fn token_text(&self) -> &'src str {
        self.tokens.text(*self.token())
    }
    fn magnitude(&self) -> Result<u64, Diagnostic> {
        self.token_text()
            .parse()
            .map_err(|_| Diagnostic::new("E001", "INTEGER literal out of range", self.token().span))
    }
    fn number_literal(&self, negative: bool) -> Result<Literal, Diagnostic> {
        let raw = self.token_text();
        let suffix = [
            "usize", "isize", "u16", "u32", "u64", "i16", "i32", "i64", "f32", "f64", "u8", "i8",
        ]
        .into_iter()
        .find(|s| raw.ends_with(s));
        let kind =
            suffix
                .and_then(NumberKind::parse)
                .unwrap_or(if self.token().kind == TokenKind::Real {
                    NumberKind::F64
                } else {
                    NumberKind::I64
                });
        let digits = suffix.map_or(raw, |s| raw[..raw.len() - s.len()].trim_end_matches('_'));
        let text = if negative {
            format!("-{digits}")
        } else {
            digits.to_string()
        };
        Number::parse(kind, &text)
            .map(|n| {
                if suffix.is_some() {
                    Literal::Number(n)
                } else {
                    Literal::number(n)
                }
            })
            .map_err(|error| {
                cyallo_ast::diagnostic::invalid_numeric_literal(error, self.token().span)
            })
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
    fn error(&self, msg: impl Into<String>) -> Diagnostic {
        Diagnostic::new("E002", msg, self.token().span)
    }
    fn expect(&mut self, s: &str) -> Result<(), Diagnostic> {
        if self.take(s) {
            Ok(())
        } else {
            Err(self.error(format!("expected {s}")))
        }
    }
    fn name(&mut self) -> Result<Box<str>, Diagnostic> {
        if self.token().kind != TokenKind::Identifier {
            return Err(self.error("expected identifier; keywords are reserved"));
        }
        let name = self.token_text().into();
        self.pos += 1;
        Ok(name)
    }
    fn newlines(&mut self) {
        while self.token().kind == TokenKind::Newline || self.token().kind == TokenKind::Semicolon {
            self.pos += 1;
        }
    }
    fn eol(&mut self) -> Result<(), Diagnostic> {
        if self.token().kind == TokenKind::Eof || self.at("}") {
            return Ok(());
        }
        if self.take(";") {
            self.newlines();
            return Ok(());
        }
        if self.token().kind != TokenKind::Newline {
            return Err(self.error("expected end of line"));
        }
        self.newlines();
        Ok(())
    }
    fn program(mut self) -> Result<Ast, Diagnostic> {
        self.declarations(false)
    }
    fn declarations(&mut self, nested: bool) -> Result<Ast, Diagnostic> {
        let mut ast = Ast::default();
        self.newlines();
        while self.token().kind != TokenKind::Eof {
            if nested && self.take("}") {
                return Ok(ast);
            }
            let attribute_start = self.token().span.start;
            let (attributes, annotations) = self.declaration_attributes()?;
            let module_count = ast.modules.len();
            let trait_count = ast.traits.len();
            let enum_count = ast.enums.len();
            let record_count = ast.records.len();
            let world_count = ast.worlds.len();
            if self.module_declaration(&mut ast)? {
                if !attributes.is_empty() {
                    return Err(
                        self.error("routine attributes require a function or procedure body")
                    );
                }
                if !annotations.is_empty() {
                    if ast.enums.len() > enum_count {
                        ast.enums.last_mut().unwrap().annotations = annotations;
                    } else if ast.modules.len() > module_count {
                        ast.modules.last_mut().unwrap().annotations = annotations;
                    } else if ast.traits.len() > trait_count {
                        ast.traits.last_mut().unwrap().annotations = annotations;
                    } else if ast.records.len() > record_count {
                        ast.records.last_mut().unwrap().annotations = annotations;
                    } else if ast.worlds.len() > world_count {
                        ast.worlds.last_mut().unwrap().annotations = annotations;
                    } else {
                        return Err(self.error(
                            "annotation requires a module, type, trait or callable declaration",
                        ));
                    }
                }
                continue;
            }
            if self.at("fn") || self.at("proc") {
                let mut function = self.modern_function()?;
                function.annotations.extend(annotations);
                if !attributes.is_empty() {
                    function.attributes = attributes;
                    function.span.start = attribute_start;
                }
                ast.functions.push(function);
            } else if self.at("record") {
                if !attributes.is_empty() {
                    return Err(
                        self.error("routine attributes require a function or procedure body")
                    );
                }
                let mut record = self.record()?;
                record.annotations = annotations;
                ast.records.push(record);
            } else {
                if !attributes.is_empty() || !annotations.is_empty() {
                    return Err(
                        self.error("routine attributes require a function or procedure body")
                    );
                }
                ast.body.push(self.statement()?);
            }
        }
        if nested {
            Err(self.error("expected }"))
        } else {
            Ok(ast)
        }
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
    fn ty(&mut self) -> Result<Type, Diagnostic> {
        self.nested(Self::ty_inner)
    }
    fn ty_inner(&mut self) -> Result<Type, Diagnostic> {
        if self.token().kind == TokenKind::Lifetime {
            let lifetime = self.token_text()[1..].into();
            self.pos += 1;
            return Ok(Type::Lifetime(lifetime));
        }
        if let Some(modern::ManagedPrefix { kind, locality }) = self.managed_prefix()? {
            let lifetime = if self.take("<") {
                if self.token().kind != TokenKind::Lifetime {
                    return Err(self.error("expected allocator lifetime"));
                }
                let lifetime = self.token_text()[1..].into();
                self.pos += 1;
                self.expect(">")?;
                Some(lifetime)
            } else {
                None
            };
            return Ok(Type::Managed {
                kind,
                locality,
                lifetime,
                pointee: Box::new(self.ty()?),
            });
        }
        if self.take("&") {
            let lifetime = if self.token().kind == TokenKind::Lifetime {
                let name = self.token_text()[1..].into();
                self.pos += 1;
                Some(name)
            } else {
                None
            };
            let mutable = self.word("mut");
            return Ok(Type::Reference {
                mutable,
                lifetime,
                pointee: Box::new(self.ty()?),
            });
        }
        if self.take("[") {
            if self.take("]") {
                return Ok(Type::Slice(Box::new(self.ty()?)));
            }
            let size = self.signed_int()?;
            if size < 0 {
                return Err(self.error("array length must be nonnegative"));
            }
            self.expect("]")?;
            return Ok(Type::Array(vec![(0, size - 1)], Box::new(self.ty()?)));
        }
        self.base_ty()
    }
    fn base_ty(&mut self) -> Result<Type, Diagnostic> {
        if self.take("dynamic") {
            return Ok(Type::Dynamic);
        }
        if self.at("record") {
            self.pos += 1;
            return self.anonymous_record_type();
        }
        if self.take("object") {
            self.expect("{")?;
            self.newlines();
            let mut fields = vec![];
            while !self.take("}") {
                if self.take("..") {
                    self.newlines();
                    self.expect("}")?;
                    break;
                }
                let name = self.name()?;
                self.expect(":")?;
                fields.push((name, self.ty()?));
                if self.take("}") {
                    break;
                }
                if !self.take(",") && self.token().kind != TokenKind::Newline {
                    return Err(self.error("expected comma or newline"));
                }
                self.newlines();
            }
            return Ok(Type::Object(fields));
        }
        if self.word("dyn") {
            return Ok(Type::TraitObject(self.qualified_name()?));
        }
        if self.at("fn") || self.at("proc") {
            let kind = if self.take("fn") {
                RoutineKind::Function
            } else {
                self.expect("proc")?;
                RoutineKind::Procedure
            };
            self.expect("(")?;
            let mut params = vec![];
            self.newlines();
            while !self.take(")") {
                let by_ref = false;
                params.push((self.ty()?, by_ref));
                self.newlines();
                if self.take(")") {
                    break;
                }
                self.expect(",")?;
                self.newlines();
            }
            let result = if self.take("->") {
                self.ty()?
            } else if kind == RoutineKind::Procedure {
                Type::Void
            } else {
                return Err(self.error("expected ->"));
            };
            return Ok(Type::Function(kind, params, Box::new(result)));
        }
        if self.take("(") {
            let mut fields = vec![];
            self.newlines();
            while !self.take(")") {
                fields.push(self.ty()?);
                self.newlines();
                if self.take(")") {
                    break;
                }
                self.expect(",")?;
                self.newlines();
            }
            return Ok(Type::Tuple(fields));
        }
        if matches!(
            self.token().kind,
            TokenKind::Identifier
                | TokenKind::Gc
                | TokenKind::Rc
                | TokenKind::Object
                | TokenKind::Dynamic
        ) {
            if let Some(kind) = NumberKind::parse(self.token_text()) {
                self.pos += 1;
                return Ok(Type::numeric(kind));
            }
            let ty = match self.token_text() {
                "Self" => Some(Type::SelfType),
                "i64" => Some(Type::Integer),
                "f64" => Some(Type::Real),
                "bool" => Some(Type::Boolean),
                _ => None,
            };
            if let Some(ty) = ty {
                self.pos += 1;
                return Ok(ty);
            }
            let name = self.qualified_name()?;
            if self.take("<") {
                let ts = self.type_arguments()?;
                return Ok(Type::Applied(name, ts));
            }
            return Ok(Type::Named(name));
        }
        for (spelling, ty) in [("char", Type::Char), ("string", Type::String)] {
            if self.take(spelling) {
                return Ok(ty);
            }
        }
        Err(Diagnostic::new(
            "E003",
            "unsupported or unknown type",
            self.token().span,
        ))
    }
    fn statement(&mut self) -> Result<Stmt, Diagnostic> {
        let span = self.token().span;
        if let Some(kind) = self.modern_statement()? {
            let span = self.finish_span(span);
            self.eol()?;
            return Ok(Stmt { kind, span });
        }
        let kind = if self.take("return") {
            StmtKind::Return(self.expr(0)?)
        } else {
            // Parse once: the target of an assignment is an ordinary expression,
            // and semantic analysis checks that it is assignable.
            let target = self.expr(0)?;
            if self.take("=") {
                StmtKind::Assign(target, self.expr(0)?)
            } else if matches!(
                self.token().kind,
                TokenKind::PlusAssign
                    | TokenKind::MinusAssign
                    | TokenKind::StarAssign
                    | TokenKind::SlashAssign
            ) {
                let op = match self.token().kind {
                    TokenKind::PlusAssign => Operator::Add,
                    TokenKind::MinusAssign => Operator::Subtract,
                    TokenKind::StarAssign => Operator::Multiply,
                    _ => Operator::Divide,
                };
                self.pos += 1;
                StmtKind::Compound(target, op, self.expr(0)?)
            } else {
                let value = target;
                if self.expression_statements {
                    StmtKind::Output(vec![value])
                } else if matches!(
                    value.kind,
                    ExprKind::Call(..)
                        | ExprKind::GenericCall(..)
                        | ExprKind::Invoke(..)
                        | ExprKind::Yield(..)
                        | ExprKind::DeleteProperty(..)
                        | ExprKind::Match(..)
                ) {
                    StmtKind::Evaluate(value)
                } else {
                    return Err(self.error("expected assignment or routine call"));
                }
            }
        };
        let span = self.finish_span(span);
        self.eol()?;
        Ok(Stmt { kind, span })
    }
    fn expr(&mut self, min: u8) -> Result<Expr, Diagnostic> {
        self.nested(|parser| parser.expr_inner(min))
    }
    fn expr_inner(&mut self, min: u8) -> Result<Expr, Diagnostic> {
        if self.nesting > 0 {
            self.newlines();
        }
        let span = self.token().span;
        let mut lhs = if let Some(modern::ManagedPrefix { kind, locality }) =
            self.managed_prefix()?
        {
            let (allocator, value) = if kind == MemoryKind::Rc && self.take("(") {
                let inner = self.expr(0)?;
                self.expect(")")?;
                if matches!(
                    self.token().kind,
                    TokenKind::Identifier
                        | TokenKind::Integer
                        | TokenKind::Real
                        | TokenKind::Text
                        | TokenKind::Char
                        | TokenKind::True
                        | TokenKind::False
                        | TokenKind::LeftParen
                        | TokenKind::LeftBracket
                        | TokenKind::At
                        | TokenKind::Fn
                        | TokenKind::Proc
                        | TokenKind::Gc
                        | TokenKind::Rc
                        | TokenKind::Ampersand
                        | TokenKind::Bang
                        | TokenKind::Minus
                        | TokenKind::Plus
                ) {
                    (Some(Box::new(inner)), Box::new(self.expr(UNARY_OPERAND)?))
                } else {
                    (None, Box::new(inner))
                }
            } else {
                (None, Box::new(self.expr(UNARY_OPERAND)?))
            };
            Expr {
                kind: ExprKind::ManagedNew(kind, locality, allocator, value),
                span: self.finish_span(span),
            }
        } else if self.word("match") {
            Expr {
                kind: self.match_expression()?,
                span: self.finish_span(span),
            }
        } else if self.take("record") {
            Expr {
                kind: self.anonymous_record_literal()?,
                span: self.finish_span(span),
            }
        } else if (self.at("object") || self.at("dynamic"))
            && self
                .tokens
                .get(self.pos + 1)
                .is_some_and(|token| token.kind == TokenKind::LeftBrace)
        {
            let kind = if self.take("object") {
                ObjectKind::Typed
            } else {
                self.expect("dynamic")?;
                ObjectKind::Dynamic
            };
            self.expect("{")?;
            self.nesting += 1;
            self.newlines();
            let mut fields = vec![];
            while !self.take("}") {
                let name = self.name()?;
                self.expect(":")?;
                fields.push((name, self.expr(0)?));
                if self.take("}") {
                    break;
                }
                if !self.take(",") && self.token().kind != TokenKind::Newline {
                    return Err(self.error("expected comma or newline"));
                }
                self.newlines();
            }
            self.nesting -= 1;
            Expr {
                kind: ExprKind::ObjectLiteral(kind, fields),
                span: self.finish_span(span),
            }
        } else if self.take("delete") {
            let target = self.expr(UNARY_OPERAND)?;
            Expr {
                kind: ExprKind::DeleteProperty(Box::new(target)),
                span: self.finish_span(span),
            }
        } else if self.take("gen") {
            let (mut function, refs) = self.closure()?;
            function.generator = true;
            Expr {
                kind: ExprKind::Closure(Box::new(function), refs),
                span: self.finish_span(span),
            }
        } else if self.take("yield") {
            let delegate = self.word("from");
            let value = self.expr(0)?;
            Expr {
                kind: ExprKind::Yield(Box::new(value), delegate),
                span: self.finish_span(span),
            }
        } else if self.at("fn") || self.at("proc") {
            let (function, refs) = self.closure()?;
            Expr {
                kind: ExprKind::Closure(Box::new(function), refs),
                span: self.finish_span(span),
            }
        } else if self.take("&") {
            let op = if self.word("mut") {
                Operator::BorrowMut
            } else {
                Operator::Borrow
            };
            Expr {
                kind: ExprKind::Unary(op, Box::new(self.expr(UNARY_OPERAND)?)),
                span: self.finish_span(span),
            }
        } else if self.at("*") {
            return Err(cyallo_ast::diagnostic::prefix_dereference_removed(span));
        } else if self.take("-") {
            if matches!(self.token().kind, TokenKind::Integer | TokenKind::Real) {
                let literal = self.number_literal(true)?;
                self.pos += 1;
                Expr {
                    kind: ExprKind::Literal(literal),
                    span: self.finish_span(span),
                }
            } else {
                Expr {
                    kind: ExprKind::Unary(Operator::Subtract, Box::new(self.expr(UNARY_OPERAND)?)),
                    span,
                }
            }
        } else if self.take("+") {
            Expr {
                kind: ExprKind::Unary(Operator::Add, Box::new(self.expr(UNARY_OPERAND)?)),
                span,
            }
        } else if self.take("!") {
            Expr {
                kind: ExprKind::Unary(Operator::Not, Box::new(self.expr(UNARY_OPERAND)?)),
                span,
            }
        } else if self.take("(") {
            self.nesting += 1;
            if self.take(")") {
                self.nesting -= 1;
                return Ok(Expr {
                    kind: ExprKind::Tuple(vec![]),
                    span: self.finish_span(span),
                });
            }
            let e = self.expr(0)?;
            if self.take(",") {
                let mut values = vec![e];
                self.newlines();
                while !self.take(")") {
                    values.push(self.expr(0)?);
                    self.newlines();
                    if self.take(")") {
                        break;
                    }
                    self.expect(",")?;
                    self.newlines();
                }
                self.nesting -= 1;
                Expr {
                    kind: ExprKind::Tuple(values),
                    span: self.finish_span(span),
                }
            } else {
                self.newlines();
                self.nesting -= 1;
                self.expect(")")?;
                Expr {
                    kind: ExprKind::Parenthesized(Box::new(e)),
                    span: self.finish_span(span),
                }
            }
        } else if self.take("[") {
            let values = self.modern_list("]")?;
            Expr {
                kind: ExprKind::ArrayLiteral(values),
                span: self.finish_span(span),
            }
        } else {
            let kind = match self.token().kind {
                TokenKind::Integer | TokenKind::Real => {
                    ExprKind::Literal(self.number_literal(false)?)
                }
                TokenKind::Text => {
                    let text = self.token_text();
                    ExprKind::Literal(Literal::String(
                        self.literal_text(text, span)?.into_boxed_str(),
                    ))
                }
                TokenKind::Char => {
                    let text = self.literal_text(self.token_text(), span)?;
                    let mut chars = text.chars();
                    let c = chars.next().ok_or_else(|| self.error("empty CHAR"))?;
                    if chars.next().is_some() {
                        return Err(self.error("CHAR requires one Unicode scalar"));
                    }
                    ExprKind::Literal(Literal::Char(c))
                }
                TokenKind::True | TokenKind::False => {
                    ExprKind::Literal(Literal::Boolean(self.token().kind == TokenKind::True))
                }
                TokenKind::Identifier
                | TokenKind::Gc
                | TokenKind::Rc
                | TokenKind::Object
                | TokenKind::Dynamic => {
                    let mut name = self.token_text().to_string();
                    while self
                        .tokens
                        .get(self.pos + 1)
                        .is_some_and(|t| t.kind == TokenKind::Colon)
                        && self
                            .tokens
                            .get(self.pos + 2)
                            .is_some_and(|t| t.kind == TokenKind::Colon)
                    {
                        self.pos += 3;
                        if !self.token().kind.is_keyword()
                            && self.token().kind != TokenKind::Identifier
                        {
                            return Err(self.error("expected path segment"));
                        }
                        name.push_str("::");
                        name.push_str(self.token_text());
                    }
                    ExprKind::Name(name.into())
                }
                _ => return Err(self.error("expected expression")),
            };
            self.pos += 1;
            Expr { kind, span }
        };
        lhs.span = self.finish_span(span);
        loop {
            if self.nesting > 0 {
                self.newlines();
            }
            if min <= CAST_PRECEDENCE && self.word("as") {
                let ty = self.ty()?;
                lhs = Expr {
                    kind: ExprKind::Cast(Box::new(lhs), ty),
                    span: self.finish_span(span),
                };
                continue;
            }
            if self.at("<")
                && let ExprKind::Name(name) = &lhs.kind
            {
                let name = name.clone();
                let saved = self.pos;
                self.pos += 1;
                if let Ok(ts) = self.type_arguments() {
                    if self.take("[") {
                        let args = self.modern_list("]")?;
                        lhs = Expr {
                            kind: ExprKind::TypedCollection(name, ts, args),
                            span: self.finish_span(span),
                        };
                        continue;
                    }
                    if self.take("(") {
                        let args = self.modern_list(")")?;
                        lhs = Expr {
                            kind: ExprKind::GenericCall(name, ts, args),
                            span: self.finish_span(span),
                        };
                        continue;
                    }
                    if self.take("{") {
                        let mut fields = vec![];
                        self.nesting += 1;
                        self.newlines();
                        while !self.take("}") {
                            let field = self.name()?;
                            self.expect(":")?;
                            fields.push((field, self.expr(0)?));
                            self.newlines();
                            if self.take("}") {
                                break;
                            }
                            self.expect(",")?;
                            self.newlines();
                        }
                        self.nesting -= 1;
                        lhs = Expr {
                            kind: ExprKind::GenericRecord(name, ts, fields),
                            span: self.finish_span(span),
                        };
                        continue;
                    }
                }
                self.pos = saved;
            }
            if self.take("[") {
                let indices = self.modern_list("]")?;
                lhs = Expr {
                    kind: ExprKind::Index(Box::new(lhs), indices),
                    span: self.finish_span(span),
                };
                continue;
            }
            if self.take(".") {
                let kind = if self.take("*") {
                    ExprKind::Unary(Operator::Dereference, Box::new(lhs))
                } else if self.token().kind == TokenKind::Integer {
                    let index = usize::try_from(self.magnitude()?)
                        .map_err(|_| self.error("tuple index is too large"))?;
                    self.pos += 1;
                    ExprKind::TupleField(Box::new(lhs), index)
                } else {
                    let name = self.name()?;
                    ExprKind::Field(Box::new(lhs), name)
                };
                lhs = Expr {
                    kind,
                    span: self.finish_span(span),
                };
                continue;
            }
            if self.at("{")
                && let ExprKind::Name(name) = &lhs.kind
                && {
                    let mut i = self.pos + 1;
                    while self
                        .tokens
                        .get(i)
                        .is_some_and(|t| t.kind == TokenKind::Newline)
                    {
                        i += 1;
                    }
                    (!self.condition || self.nesting > 0)
                        && self
                            .tokens
                            .get(i)
                            .is_some_and(|t| t.kind == TokenKind::RightBrace)
                        || self
                            .tokens
                            .get(i + 1)
                            .is_some_and(|t| t.kind == TokenKind::Colon)
                            && self
                                .tokens
                                .get(i + 2)
                                .is_none_or(|t| t.kind != TokenKind::Colon)
                }
            {
                let name = name.clone();
                self.pos += 1;
                let mut fields = vec![];
                self.nesting += 1;
                self.newlines();
                while !self.take("}") {
                    let field = self.name()?;
                    self.expect(":")?;
                    fields.push((field, self.expr(0)?));
                    self.newlines();
                    if self.take("}") {
                        break;
                    }
                    self.expect(",")?;
                    self.newlines();
                }
                self.nesting -= 1;
                lhs = Expr {
                    kind: ExprKind::Record(name, fields),
                    span: self.finish_span(span),
                };
                continue;
            }
            if self.at("(") {
                if let ExprKind::Name(n) = lhs.kind {
                    self.pos += 1;
                    lhs = Expr {
                        kind: ExprKind::Call(n, self.modern_list(")")?),
                        span: self.finish_span(span),
                    };
                    continue;
                } else {
                    self.pos += 1;
                    let args = self.modern_list(")")?;
                    lhs = Expr {
                        kind: ExprKind::Invoke(Box::new(lhs), args),
                        span: self.finish_span(span),
                    };
                    continue;
                }
            }
            if self.token().kind == TokenKind::Ampersand {
                return Err(
                    self.error("binary `&` is not an operator; use `+` to concatenate strings")
                );
            }
            let Some((op, prec)) = self.token().kind.binary_op() else {
                break;
            };
            if prec < min {
                break;
            }
            if op.is_comparison()
                && let ExprKind::Binary(previous, ..) = &lhs.kind
                && previous.is_comparison()
            {
                return Err(
                    self.error("comparison operators cannot be chained; use `a < b && b < c`")
                );
            }
            self.pos += 1;
            let rhs = self.expr(prec + 1)?;
            lhs = Expr {
                kind: ExprKind::Binary(op, Box::new(lhs), Box::new(rhs)),
                span: self.finish_span(span),
            };
        }
        lhs.span = self.finish_span(span);
        Ok(lhs)
    }
}
