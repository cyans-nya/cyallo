//! Independent modern declarations/statements sharing one immutable token index.
use super::*;
use cyallo_compile_jobs::Jobs;
use std::ops::Range;
#[derive(Debug, Clone)]
pub struct ParseUnit {
    pub tokens: Range<usize>,
    pub span: Span,
}
#[derive(Debug)]
pub struct IndexedSource<'src> {
    tokens: Lexed<'src>,
    units: Vec<ParseUnit>,
}
impl<'src> IndexedSource<'src> {
    pub fn units(&self) -> &[ParseUnit] {
        &self.units
    }
    /// A unit borrows the original source. Spans stay in file coordinates.
    pub fn parse_unit(&self, index: usize, repl: bool) -> Result<Ast, Diagnostic> {
        let unit = self
            .units
            .get(index)
            .ok_or_else(|| Diagnostic::new("E002", "invalid parse unit", Span::default()))?;
        Parser {
            tokens: self.tokens.fragment(unit.tokens.clone()),
            pos: 0,
            expression_statements: repl,
            nesting: 0,
            depth: 0,
            condition: false,
        }
        .program()
    }
    pub fn parse(&self, repl: bool, jobs: Jobs) -> Result<Ast, Diagnostic> {
        let parsed = jobs.map(&self.units, 4, |_, i, _| self.parse_unit(i, repl));
        self.assemble(parsed)
    }
    /// Merge results in unit order, independent of their execution order.
    pub fn assemble(&self, parsed: Vec<Result<Ast, Diagnostic>>) -> Result<Ast, Diagnostic> {
        if parsed.len() != self.units.len() {
            return Err(Diagnostic::new(
                "E002",
                "parse unit count mismatch",
                Span::default(),
            ));
        }
        let mut out = Ast::default();
        for ast in parsed {
            let mut ast = ast?;
            out.enums.append(&mut ast.enums);
            out.body.append(&mut ast.body);
            out.functions.append(&mut ast.functions);
            out.type_aliases.append(&mut ast.type_aliases);
            out.records.append(&mut ast.records);
            out.imports.append(&mut ast.imports);
            out.exports.append(&mut ast.exports);
            out.traits.append(&mut ast.traits);
            out.implementations.append(&mut ast.implementations);
            out.properties.append(&mut ast.properties);
            out.modules.append(&mut ast.modules);
            out.worlds.append(&mut ast.worlds);
            out.interface_implementations
                .append(&mut ast.interface_implementations);
            out.inherent_implementations
                .append(&mut ast.inherent_implementations);
        }
        Ok(out)
    }
}
pub fn index_modern(source: &str) -> Result<IndexedSource<'_>, Diagnostic> {
    let tokens = lex_modern(source)?;
    let whole = || {
        vec![ParseUnit {
            tokens: 0..tokens.len() - 1,
            span: Span::new(0, source.len() as u32),
        }]
    };
    let mut units = vec![];
    let mut stack = vec![];
    let mut start = 0;
    let mut balanced = true;
    for (i, t) in tokens.iter().enumerate() {
        match t.kind {
            TokenKind::LeftParen => stack.push(TokenKind::RightParen),
            TokenKind::LeftBracket => stack.push(TokenKind::RightBracket),
            TokenKind::LeftBrace => stack.push(TokenKind::RightBrace),
            TokenKind::RightParen | TokenKind::RightBracket | TokenKind::RightBrace => {
                if stack.pop() != Some(t.kind) {
                    balanced = false;
                    break;
                }
            }
            _ => {}
        }
        if stack.is_empty()
            && matches!(
                t.kind,
                TokenKind::Newline | TokenKind::Semicolon | TokenKind::Eof
            )
        {
            if t.kind != TokenKind::Eof
                && tokens[start..i]
                    .iter()
                    .find(|t| !matches!(t.kind, TokenKind::Newline | TokenKind::Semicolon))
                    .is_some_and(|t| t.kind == TokenKind::Hash)
                && !tokens[start..i]
                    .iter()
                    .any(|t| matches!(t.kind, TokenKind::Fn | TokenKind::Proc))
            {
                continue;
            }
            let next = tokens[i + 1..]
                .iter()
                .find(|t| !matches!(t.kind, TokenKind::Newline | TokenKind::Semicolon));
            if next.is_some_and(|t| t.kind == TokenKind::Else) {
                continue;
            }
            let end = if t.kind == TokenKind::Eof { i } else { i + 1 };
            if tokens[start..end]
                .iter()
                .any(|t| !matches!(t.kind, TokenKind::Newline | TokenKind::Semicolon))
            {
                units.push(ParseUnit {
                    tokens: start..end,
                    span: Span::new(tokens[start].span.start, t.span.end),
                });
            }
            start = end;
        }
    }
    // Unbalanced input uses one unit to retain the parser's original diagnostics.
    if !balanced || !stack.is_empty() {
        units = whole()
    }
    Ok(IndexedSource { tokens, units })
}
pub fn parse_modern_with_jobs(source: &str, repl: bool, jobs: Jobs) -> Result<Ast, Diagnostic> {
    index_modern(source)?.parse(repl, jobs)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn indexed_units_match_the_original_parser_and_spans() {
        let source = r#"
mod example {}
pub record Pair<T> { item: T }
fn identity<T>(item: T) -> T { return item }
let value = identity<i64>(2)
if value > 0 {
    std::io::print("brace } and ; in a string")
}
else if value < 0 { std::io::print(-1) }
else { std::io::print(0) }
proc closures() {
    let f = fn(n: i64) -> i64 { return n + 1 }
    std::io::print(f(2))
}
"#;
        let indexed = index_modern(source).unwrap();
        assert!(indexed.units().len() > 4);
        let original = super::super::parse_whole(source, false).unwrap();
        for jobs in [Jobs(1), Jobs(4)] {
            assert_eq!(
                format!("{original:?}"),
                format!("{:?}", indexed.parse(false, jobs).unwrap())
            );
        }
    }
    #[test]
    fn caie_and_incomplete_blocks_report_errors() {
        for source in [
            "IF TRUE THEN\nOUTPUT 1\nENDIF\n",
            "fn test() -> i64 {\nreturn 2\n",
            "std::io::print(\"unterminated\n",
        ] {
            let original = super::super::parse_whole(source, false);
            let parallel = parse_modern_with_jobs(source, false, Jobs(4));
            assert!(original.is_err());
            assert!(parallel.is_err());
            assert_eq!(format!("{original:?}"), format!("{parallel:?}"));
        }
    }
}
