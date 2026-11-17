//! Source-preserving data enum and pattern syntax, independent of representation.
use super::*;
use cyallo_ast::diagnostic::{EnumSyntaxError as E, invalid_enum_syntax};
use std::collections::HashSet;
impl Parser<'_> {
    pub(super) fn enum_declaration(
        &mut self,
        ast: &mut Ast,
        public: bool,
        start: Span,
    ) -> Result<(), Diagnostic> {
        let open = self.word("open");
        self.expect("enum")?;
        let name = self.name()?;
        let type_params = self.type_params()?;
        self.expect("{")?;
        self.newlines();
        let mut variants = vec![];
        let mut names = HashSet::new();
        while !self.take("}") {
            let span = self.token().span;
            let (attributes, annotations) = self.declaration_attributes()?;
            if !attributes.is_empty() {
                return Err(invalid_enum_syntax(E::VariantRoutineAttributes, span));
            }
            let name = self.name()?;
            if !names.insert(name.clone()) {
                return Err(invalid_enum_syntax(E::DuplicateVariant { name }, span));
            }
            let payload = if self.take(":") {
                Some(self.ty()?)
            } else {
                None
            };
            variants.push(VariantDeclaration {
                resolved: None,
                name,
                payload,
                annotations,
                span: self.finish_span(span),
            });
            self.newlines();
            if self.take("}") {
                break;
            }
            self.expect(",")?;
            self.newlines();
        }
        if public {
            ast.exports.push(format!("$type${name}").into());
        }
        ast.enums.push(EnumDeclaration {
            resolved: None,
            name,
            type_params,
            open,
            annotations: vec![],
            variants,
            span: self.finish_span(start),
        });
        self.eol()
    }
    pub(super) fn anonymous_record_type(&mut self) -> Result<Type, Diagnostic> {
        self.expect("{")?;
        self.newlines();
        let mut fields = vec![];
        let mut names = HashSet::new();
        while !self.take("}") {
            let span = self.token().span;
            let name = self.name()?;
            if !names.insert(name.clone()) {
                return Err(invalid_enum_syntax(E::DuplicateField { name }, span));
            }
            self.expect(":")?;
            fields.push((name, self.ty()?));
            if self.take("}") {
                break;
            }
            if !self.take(",") && self.token().kind != TokenKind::Newline {
                return Err(invalid_enum_syntax(
                    E::ExpectedFieldSeparator,
                    self.token().span,
                ));
            }
            self.newlines();
        }
        fields.sort_by(|a, b| a.0.cmp(&b.0));
        Ok(Type::AnonymousRecord(fields))
    }
    pub(super) fn anonymous_record_literal(&mut self) -> Result<ExprKind, Diagnostic> {
        self.expect("{")?;
        self.nesting += 1;
        self.newlines();
        let mut fields = vec![];
        let mut names = HashSet::new();
        while !self.take("}") {
            let span = self.token().span;
            let name = self.name()?;
            if !names.insert(name.clone()) {
                return Err(invalid_enum_syntax(E::DuplicateField { name }, span));
            }
            self.expect(":")?;
            fields.push((name, self.expr(0)?));
            self.newlines();
            if self.take("}") {
                break;
            }
            self.expect(",")?;
            self.newlines();
        }
        self.nesting -= 1;
        // Evaluation retains source order; only the type/layout is canonical.
        Ok(ExprKind::AnonymousRecord(fields))
    }
    pub(super) fn match_expression(&mut self) -> Result<ExprKind, Diagnostic> {
        let value = self.condition_expr()?;
        self.expect("{")?;
        self.newlines();
        let mut arms = vec![];
        while !self.take("}") {
            let start = self.token().span;
            let pattern = self.pattern()?;
            let guard = if self.take("if") {
                Some(self.expr(0)?)
            } else {
                None
            };
            self.expect("=>")?;
            let body = if self.at("{") {
                self.match_block()?
            } else {
                MatchBody::Expression(self.expr(0)?)
            };
            arms.push(MatchArm {
                package: None,
                pattern,
                guard,
                body,
                span: self.finish_span(start),
            });
            if self.take("}") {
                break;
            }
            if !self.take(",") && self.token().kind != TokenKind::Newline {
                return Err(invalid_enum_syntax(
                    E::ExpectedArmSeparator,
                    self.token().span,
                ));
            }
            self.newlines();
        }
        Ok(ExprKind::Match(Box::new(value), arms))
    }
    fn match_block(&mut self) -> Result<MatchBody, Diagnostic> {
        self.expect("{")?;
        let old_nesting = std::mem::replace(&mut self.nesting, 0);
        let old_condition = std::mem::replace(&mut self.condition, false);
        let result = (|| {
            let mut statements = vec![];
            self.newlines();
            while !self.take("}") {
                let saved = self.pos;
                if let Ok(value) = self.expr(0) {
                    self.newlines();
                    if self.take("}") {
                        return Ok(MatchBody::Block(statements, Some(Box::new(value))));
                    }
                }
                self.pos = saved;
                statements.push(self.statement()?);
                self.newlines();
            }
            Ok(MatchBody::Block(statements, None))
        })();
        self.nesting = old_nesting;
        self.condition = old_condition;
        result
    }
    pub(super) fn pattern(&mut self) -> Result<Pattern, Diagnostic> {
        self.nested(|p| {
            let start = p.token().span;
            let first = p.pattern_atom()?;
            if !p.take("|") {
                return Ok(first);
            }
            let mut alternatives = vec![first];
            loop {
                alternatives.push(p.pattern_atom()?);
                if !p.take("|") {
                    break;
                }
            }
            Ok(Pattern {
                kind: PatternKind::Or(alternatives),
                span: p.finish_span(start),
            })
        })
    }
    fn pattern_atom(&mut self) -> Result<Pattern, Diagnostic> {
        let start = self.token().span;
        let kind = if self.word("_") {
            PatternKind::Wildcard
        } else if self.take("(") {
            let mut fields = vec![];
            self.newlines();
            while !self.take(")") {
                fields.push(self.pattern()?);
                self.newlines();
                if self.take(")") {
                    break;
                }
                self.expect(",")?;
                self.newlines();
            }
            PatternKind::Tuple(fields)
        } else if self.at("{") {
            self.record_pattern()?
        } else if matches!(
            self.token().kind,
            TokenKind::Integer
                | TokenKind::Real
                | TokenKind::Text
                | TokenKind::Char
                | TokenKind::True
                | TokenKind::False
                | TokenKind::Minus
        ) {
            let value = self.expr(UNARY_OPERAND)?;
            let ExprKind::Literal(value) = value.kind else {
                return Err(invalid_enum_syntax(E::ExpectedPattern, value.span));
            };
            PatternKind::Literal(value)
        } else {
            let name = self.qualified_name()?;
            if self.take("(") {
                let payload = self.pattern()?;
                self.expect(")")?;
                PatternKind::Variant(name, Some(Box::new(payload)))
            } else if self.at("{") {
                let span = self.token().span;
                let kind = self.record_pattern()?;
                PatternKind::Variant(
                    name,
                    Some(Box::new(Pattern {
                        kind,
                        span: self.finish_span(span),
                    })),
                )
            } else if name.contains("::") || name.chars().next().is_some_and(char::is_uppercase) {
                PatternKind::Variant(name, None)
            } else {
                PatternKind::Binding(name)
            }
        };
        Ok(Pattern {
            kind,
            span: self.finish_span(start),
        })
    }
    fn record_pattern(&mut self) -> Result<PatternKind, Diagnostic> {
        self.expect("{")?;
        self.newlines();
        let mut fields = vec![];
        let mut names = HashSet::new();
        let mut rest = false;
        while !self.take("}") {
            if self.take("..") {
                rest = true;
                self.newlines();
                self.expect("}")?;
                break;
            }
            let span = self.token().span;
            let name = self.name()?;
            if !names.insert(name.clone()) {
                return Err(invalid_enum_syntax(E::DuplicateField { name }, span));
            }
            let value = if self.take(":") {
                self.pattern()?
            } else {
                Pattern {
                    kind: PatternKind::Binding(name.clone()),
                    span,
                }
            };
            fields.push((name, value));
            self.newlines();
            if self.take("}") {
                break;
            }
            self.expect(",")?;
            self.newlines();
        }
        Ok(PatternKind::Record(fields, rest))
    }
    pub(super) fn if_let_statement(&mut self) -> Result<Option<StmtKind>, Diagnostic> {
        let pattern = self.pattern()?;
        self.expect("=")?;
        let value = self.condition_expr()?;
        let yes = self.braces()?;
        let after = self.pos;
        self.newlines();
        let no = if self.take("else") {
            if self.at("if") {
                let start = self.token().span;
                let kind = self
                    .modern_statement()?
                    .ok_or_else(|| invalid_enum_syntax(E::ExpectedIfBlock, self.token().span))?;
                vec![Stmt {
                    kind,
                    span: self.finish_span(start),
                }]
            } else {
                self.braces()?
            }
        } else {
            self.pos = after;
            vec![]
        };
        Ok(Some(StmtKind::IfLet(pattern, value, yes, no)))
    }
}
