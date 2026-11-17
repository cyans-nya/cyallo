use super::*;
#[derive(Default)]
struct GenericParameters {
    names: Vec<Box<str>>,
    constraints: Vec<GenericConstraint>,
    packs: Vec<Box<str>>,
}
pub(super) struct ManagedPrefix {
    pub kind: MemoryKind,
    pub locality: Locality,
}
impl Parser<'_> {
    /// Parses storage syntax independently of its optional locality modifier.
    pub(super) fn managed_prefix(&mut self) -> Result<Option<ManagedPrefix>, Diagnostic> {
        use cyallo_ast::diagnostic::{ManagedSyntaxError as E, invalid_managed_syntax};
        let start = self.token().span;
        let mut locality = Locality::Local;
        let qualified = self.take("@");
        if qualified {
            locality = match self.token_text() {
                "local" => Locality::Local,
                "shared" => Locality::Shared,
                "gc" => {
                    return Err(invalid_managed_syntax(
                        E::LegacySigil(MemoryKind::Gc),
                        Span::new(start.start, self.token().span.end),
                    ));
                }
                "rc" => {
                    return Err(invalid_managed_syntax(
                        E::LegacySigil(MemoryKind::Rc),
                        Span::new(start.start, self.token().span.end),
                    ));
                }
                name => {
                    return Err(invalid_managed_syntax(
                        E::UnknownLocality(name.into()),
                        Span::new(start.start, self.token().span.end),
                    ));
                }
            };
            self.pos += 1;
            if self.at("@") {
                return Err(invalid_managed_syntax(
                    E::RepeatedLocality,
                    Span::new(start.start, self.token().span.end),
                ));
            }
        }
        // A storage keyword can also name an ordinary module in a path.
        if !qualified
            && self
                .tokens
                .get(self.pos + 1)
                .is_some_and(|token| token.kind == TokenKind::Colon)
        {
            return Ok(None);
        }
        let kind = match self.token().kind {
            TokenKind::Gc => MemoryKind::Gc,
            TokenKind::Rc => MemoryKind::Rc,
            _ if qualified => {
                return Err(invalid_managed_syntax(
                    E::ExpectedStorage,
                    Span::new(start.start, self.token().span.end),
                ));
            }
            _ => return Ok(None),
        };
        self.pos += 1;
        Ok(Some(ManagedPrefix { kind, locality }))
    }
    pub(super) fn declaration_attributes(
        &mut self,
    ) -> Result<(RoutineAttributes, Vec<DeclarationAnnotation>), Diagnostic> {
        let mut attributes = RoutineAttributes::default();
        let mut annotations = vec![];
        while self.token().kind == TokenKind::Hash {
            let start = self.token().span;
            self.pos += 1;
            self.expect("[")?;
            let name = self.name()?.to_string();
            if name == "cyallo_internal" || name == "since" {
                self.expect("(")?;
                if self.token().kind != TokenKind::Text {
                    if name == "cyallo_internal" {
                        return Err(self.error("cyallo_internal requires a string literal ABI key"));
                    }
                    return Err(cyallo_ast::diagnostic::invalid_enum_syntax(
                        cyallo_ast::diagnostic::EnumSyntaxError::AnnotationRequiresString {
                            name: name.into(),
                        },
                        self.token().span,
                    ));
                }
                let value = self
                    .literal_text(self.token_text(), self.token().span)?
                    .into_boxed_str();
                self.pos += 1;
                self.expect(")")?;
                self.expect("]")?;
                annotations.push(DeclarationAnnotation {
                    name: name.into(),
                    value,
                    span: self.finish_span(start),
                });
                self.newlines();
                continue;
            }
            let value = if self.take("(") {
                let value = self.name()?.to_string();
                self.expect(")")?;
                value
            } else if name == "inline" {
                "preferred".into()
            } else {
                return Err(self.error("attribute requires a value"));
            };
            self.expect("]")?;
            let span = self.finish_span(start);
            let previous = match name.as_str() {
                "hint" => {
                    attributes.heat = match value.as_str() {
                        "auto" => HeatHint::Auto,
                        "hot" => HeatHint::Hot,
                        "cold" => HeatHint::Cold,
                        _ => {
                            return Err(Diagnostic::new(
                                "E154",
                                "expected hint(auto), hint(hot) or hint(cold)",
                                span,
                            ));
                        }
                    };
                    attributes.heat_span.replace(span)
                }
                "opt" => {
                    attributes.compilation = match value.as_str() {
                        "auto" => CompileHint::Auto,
                        "eager" => CompileHint::Eager,
                        "lazy" => CompileHint::Lazy,
                        "always_eager" => CompileHint::AlwaysEager,
                        "always_lazy" => CompileHint::AlwaysLazy,
                        _ => {
                            return Err(Diagnostic::new(
                                "E154",
                                "unknown opt attribute value",
                                span,
                            ));
                        }
                    };
                    attributes.compilation_span.replace(span)
                }
                "inline" => {
                    attributes.inlining = match value.as_str() {
                        "auto" => InlineHint::Auto,
                        "preferred" => InlineHint::Preferred,
                        "never" => InlineHint::Never,
                        "always" => InlineHint::Always,
                        _ => {
                            return Err(Diagnostic::new(
                                "E154",
                                "unknown inline attribute value",
                                span,
                            ));
                        }
                    };
                    attributes.inline_span.replace(span)
                }
                _ => return Err(Diagnostic::new("E154", "unknown routine attribute", span)),
            };
            if let Some(previous) = previous {
                return Err(Diagnostic::new("E154", "duplicate routine attribute", span)
                    .with_related(previous, "first attribute"));
            }
            self.newlines();
        }
        if attributes.compilation == CompileHint::AlwaysLazy
            && attributes.inlining == InlineHint::Always
        {
            return Err(Diagnostic::new(
                "E154",
                "opt(always_lazy) conflicts with inline(always)",
                attributes.inline_span.unwrap(),
            )
            .with_related(
                attributes.compilation_span.unwrap(),
                "native compilation forbidden here",
            ));
        }
        Ok((attributes, annotations))
    }
    pub(super) fn condition_expr(&mut self) -> Result<Expr, Diagnostic> {
        let old = self.condition;
        self.condition = true;
        let result = self.expr(0);
        self.condition = old;
        result
    }
    pub(super) fn type_params(&mut self) -> Result<Vec<Box<str>>, Diagnostic> {
        let GenericParameters {
            names,
            constraints,
            packs,
        } = self.generic_parameters()?;
        if !constraints.is_empty() || !packs.is_empty() {
            return Err(self.error("bounds and packs require a callable declaration"));
        }
        Ok(names)
    }
    fn pack_marker(&mut self) -> Result<bool, Diagnostic> {
        if !self.take("..") {
            return Ok(false);
        }
        self.expect(".")?;
        Ok(true)
    }
    fn generic_parameters(&mut self) -> Result<GenericParameters, Diagnostic> {
        if !self.take("<") {
            return Ok(GenericParameters::default());
        }
        let mut params: Vec<Box<str>> = vec![];
        let mut constraints = vec![];
        let mut packs = vec![];
        loop {
            let pack = self.pack_marker()?;
            let name = self.name()?;
            if params.iter().any(|p| p == &name) {
                return Err(self.error("duplicate type parameter"));
            }
            if pack {
                packs.push(name.clone());
            }
            if self.take(":") {
                let mut bounds = vec![self.ty()?];
                while self.take("+") {
                    bounds.push(self.ty()?);
                }
                constraints.push(GenericConstraint {
                    parameter: name.clone(),
                    bounds,
                });
            }
            params.push(name);
            if self.take(">") {
                break;
            }
            self.expect(",")?;
        }
        Ok(GenericParameters {
            names: params,
            constraints,
            packs,
        })
    }
    pub(super) fn type_arguments(&mut self) -> Result<Vec<Type>, Diagnostic> {
        let mut ts = vec![];
        loop {
            ts.push(self.ty()?);
            if self.take(">") {
                break;
            }
            self.expect(",")?;
        }
        Ok(ts)
    }
    pub(super) fn record(&mut self) -> Result<Record, Diagnostic> {
        let start = self.token().span;
        self.expect("record")?;
        let name = self.name()?;
        let type_params = self.type_params()?;
        self.expect("{")?;
        self.newlines();
        let mut fields = vec![];
        while !self.take("}") {
            let field = self.name()?;
            self.expect(":")?;
            fields.push((field, self.ty()?));
            if self.take("}") {
                break;
            }
            if self.take(",") {
                self.newlines();
            } else {
                self.eol()?;
            }
        }
        let span = self.finish_span(start);
        self.eol()?;
        Ok(Record {
            opaque: false,
            annotations: vec![],
            type_params,
            name,
            fields,
            span,
        })
    }
    pub(super) fn closure(&mut self) -> Result<(Function, Vec<ReferenceCapture>), Diagnostic> {
        let start = self.token().span;
        let kind = if self.take("fn") {
            RoutineKind::Function
        } else {
            self.expect("proc")?;
            RoutineKind::Procedure
        };
        let mut refs = vec![];
        if self.take("[") {
            self.newlines();
            while !self.take("]") {
                self.expect("&")?;
                let mutable = self.word("mut");
                refs.push(ReferenceCapture {
                    name: self.name()?,
                    mutable,
                });
                self.newlines();
                if self.take("]") {
                    break;
                }
                self.expect(",")?;
                self.newlines();
            }
        }
        self.expect("(")?;
        let mut params = vec![];
        self.newlines();
        while !self.take(")") {
            let span = self.token().span;
            let by_ref = false;
            let name = self.name()?;
            self.expect(":")?;
            let ty = self.ty()?;
            params.push(Param {
                arity: ParameterArity::One,
                passing: ParameterPassing::Value,
                receiver: false,
                name,
                ty,
                by_ref,
                span,
            });
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
        let body = self.braces()?;
        Ok((
            Function {
                annotations: vec![],
                body_kind: FunctionBodyKind::Source,
                constraints: vec![],
                type_packs: vec![],
                generator: false,
                origin: RoutineOrigin::Source,
                attributes: Default::default(),
                type_params: vec![],
                kind,
                source_name: "<closure>".into(),
                name: "<closure>".into(),
                params,
                result,
                body,
                span: self.finish_span(start),
            },
            refs,
        ))
    }
    pub(super) fn literal_text(&self, raw: &str, span: Span) -> Result<String, Diagnostic> {
        let raw = &raw[1..raw.len() - 1];
        let mut out = String::with_capacity(raw.len());
        let mut chars = raw.chars();
        while let Some(c) = chars.next() {
            if c != '\\' {
                out.push(c);
                continue;
            }
            out.push(match chars.next() {
                Some('n') => '\n',
                Some('r') => '\r',
                Some('t') => '\t',
                Some('0') => '\0',
                Some('\\') => '\\',
                Some('"') => '"',
                Some('\'') => '\'',
                _ => {
                    return Err(Diagnostic::new(
                        "E001",
                        "unknown or incomplete escape sequence",
                        span,
                    ));
                }
            });
        }
        Ok(out)
    }
    pub(super) fn modern_list(&mut self, end: &str) -> Result<Vec<Expr>, Diagnostic> {
        self.nesting += 1;
        self.newlines();
        let mut values = vec![];
        while !self.take(end) {
            values.push(self.expr(0)?);
            self.newlines();
            if self.take(end) {
                break;
            }
            self.expect(",")?;
            self.newlines();
        }
        self.nesting -= 1;
        Ok(values)
    }
    pub(super) fn braces(&mut self) -> Result<Vec<Stmt>, Diagnostic> {
        self.nested(Self::braces_inner)
    }
    fn braces_inner(&mut self) -> Result<Vec<Stmt>, Diagnostic> {
        let outer_nesting = self.nesting;
        let outer_condition = self.condition;
        self.condition = false;
        self.nesting = 0;
        self.expect("{")?;
        self.newlines();
        let mut body = vec![];
        while !self.take("}") {
            if self.token().kind == TokenKind::Eof {
                return Err(self.error("expected }"));
            }
            body.push(self.statement()?);
        }
        self.nesting = outer_nesting;
        self.condition = outer_condition;
        Ok(body)
    }
    pub(super) fn modern_function(&mut self) -> Result<Function, Diagnostic> {
        self.modern_routine(false)
    }
    pub(super) fn modern_routine(&mut self, declaration: bool) -> Result<Function, Diagnostic> {
        let span = self.token().span;
        let (attributes, annotations) = self.declaration_attributes()?;
        if declaration && !attributes.is_empty() {
            return Err(self.error("put routine attributes on the implementation body"));
        }
        let function = self.take("fn");
        if !function {
            self.expect("proc")?;
        }
        let name = self.name()?;
        let GenericParameters {
            names: type_params,
            constraints,
            packs: type_packs,
        } = self.generic_parameters()?;
        self.expect("(")?;
        let mut params = vec![];
        self.newlines();
        while !self.take(")") {
            let start = self.token().span;
            let arity = if self.pack_marker()? {
                ParameterArity::Variadic
            } else {
                ParameterArity::One
            };
            let passing = if self.token_text() == "read"
                && self
                    .tokens
                    .get(self.pos + 1)
                    .is_some_and(|token| token.kind == TokenKind::Identifier)
            {
                self.pos += 1;
                ParameterPassing::Read
            } else {
                ParameterPassing::Value
            };
            let by_ref = false;
            let name = self.name()?;
            let ty = if name.as_ref() == "self" && !self.at(":") {
                Type::SelfType
            } else {
                self.expect(":")?;
                self.ty()?
            };
            params.push(Param {
                arity,
                passing,
                receiver: name.as_ref() == "self",
                name,
                ty,
                by_ref,
                span: self.finish_span(start),
            });
            self.newlines();
            if self.take(")") {
                break;
            }
            self.expect(",")?;
            self.newlines();
        }
        let result = if function {
            self.expect("->")?;
            self.ty()?
        } else if self.take("->") {
            self.ty()?
        } else {
            Type::Void
        };
        let body_kind = if declaration || self.at(";") {
            FunctionBodyKind::Declaration
        } else {
            FunctionBodyKind::Source
        };
        let body = if body_kind == FunctionBodyKind::Declaration {
            vec![]
        } else {
            self.braces()?
        };
        let span = self.finish_span(span);
        self.eol()?;
        Ok(Function {
            annotations,
            body_kind,
            constraints,
            type_packs,
            generator: false,
            origin: RoutineOrigin::Source,
            attributes,
            type_params,
            kind: if function {
                RoutineKind::Function
            } else {
                RoutineKind::Procedure
            },
            source_name: name.clone(),
            name,
            params,
            result,
            body,
            span,
        })
    }
    pub(super) fn modern_statement(&mut self) -> Result<Option<StmtKind>, Diagnostic> {
        if self.at("let") || self.at("const") {
            let constant = self.take("const");
            if !constant {
                self.expect("let")?;
            }
            let mutable = !constant && self.word("mut");
            let name = self.name()?;
            let ty = if self.take(":") {
                Some(self.ty()?)
            } else {
                None
            };
            self.expect("=")?;
            return Ok(Some(StmtKind::Binding(
                name,
                ty,
                constant,
                mutable,
                self.expr(0)?,
            )));
        }
        if self.take("break") {
            return Ok(Some(StmtKind::Break));
        }
        if self.take("continue") {
            return Ok(Some(StmtKind::Continue));
        }
        if self.at("return")
            && self.tokens.get(self.pos + 1).is_some_and(|t| {
                matches!(
                    t.kind,
                    TokenKind::RightBrace | TokenKind::Newline | TokenKind::Semicolon
                )
            })
        {
            self.pos += 1;
            return Ok(Some(StmtKind::ReturnVoid));
        }
        if self.take("if") {
            if self.take("let") {
                return self.if_let_statement();
            }
            let condition = self.condition_expr()?;
            let yes = self.braces()?;
            let after = self.pos;
            self.newlines();
            let no = if self.take("else") {
                if self.at("if") {
                    let span = self.token().span;
                    let kind = self
                        .modern_statement()?
                        .ok_or_else(|| self.error("expected if block"))?;
                    vec![Stmt {
                        kind,
                        span: self.finish_span(span),
                    }]
                } else {
                    self.braces()?
                }
            } else {
                self.pos = after;
                vec![]
            };
            return Ok(Some(StmtKind::If(condition, yes, no)));
        }
        if self.take("while") {
            let condition = self.condition_expr()?;
            return Ok(Some(StmtKind::While(condition, self.braces()?)));
        }
        if self.take("for") {
            let name = self.name()?;
            self.expect("in")?;
            let iterable = self.condition_expr()?;
            let body = self.braces()?;
            return Ok(Some(StmtKind::ForIn(name, iterable, body)));
        }
        Ok(None)
    }
}
