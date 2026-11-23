//! Independent modern declarations/statements sharing one immutable token index.
use super::*;
use cyallo_ast::indexed::{FunctionNode, Id, IndexedAst, index_diagnostic};
use cyallo_compile_jobs::Jobs;
use cyallo_startup_profile::{Counter, Stage, count, stage};
use std::ops::Range;
use std::sync::Arc;

const MIN_PARALLEL_FILES: usize = 2;
const MIN_PARALLEL_UNITS: usize = 4;

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
        let _parse = stage(Stage::Parse);
        count(Counter::ParseUnits, 1);
        let tree = Parser {
            tokens: self.tokens.fragment(unit.tokens.clone()),
            pos: 0,
            expression_statements: repl,
            nesting: 0,
            depth: 0,
            condition: false,
        }
        .program()?;
        count(Counter::BodiesParsed, tree.functions.len());
        Ok(tree)
    }
    pub fn parse(&self, repl: bool, jobs: Jobs) -> Result<Ast, Diagnostic> {
        self.assemble(self.parse_units(repl, jobs))
    }
    fn freeze_unit(&self, index: usize, repl: bool) -> Result<IndexedAst, Diagnostic> {
        let tree = self.parse_unit(index, repl)?;
        let _freeze = stage(Stage::Freeze);
        count(Counter::BodiesFrozen, tree.functions.len());
        IndexedAst::from_source_tree(tree)
            .map_err(|error| index_diagnostic(error, self.units[index].span))
    }
    fn parse_units(&self, repl: bool, jobs: Jobs) -> Vec<Result<Ast, Diagnostic>> {
        jobs.map(&self.units, MIN_PARALLEL_UNITS, |_, i, _| {
            self.parse_unit(i, repl)
        })
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
            append_unit(&mut out, ast?);
        }
        Ok(out)
    }
}

fn append_unit(out: &mut Ast, mut unit: Ast) {
    out.enums.append(&mut unit.enums);
    out.body.append(&mut unit.body);
    out.functions.append(&mut unit.functions);
    out.type_aliases.append(&mut unit.type_aliases);
    out.records.append(&mut unit.records);
    out.imports.append(&mut unit.imports);
    out.exports.append(&mut unit.exports);
    out.traits.append(&mut unit.traits);
    out.implementations.append(&mut unit.implementations);
    out.properties.append(&mut unit.properties);
    out.modules.append(&mut unit.modules);
    out.worlds.append(&mut unit.worlds);
    out.interface_implementations
        .append(&mut unit.interface_implementations);
    out.inherent_implementations
        .append(&mut unit.inherent_implementations);
}
pub fn index_modern(source: &str) -> Result<IndexedSource<'_>, Diagnostic> {
    let _lex = stage(Stage::Lex);
    let tokens = lex_modern(source)?;
    count(Counter::Files, 1);
    count(Counter::SourceBytes, source.len());
    count(Counter::Tokens, tokens.len());
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

/// The original source and file-wide token stream shared by a parsed file's units.
#[derive(Debug)]
pub struct SourceSnapshot {
    text: Arc<str>,
    tokens: Box<[Token]>,
    line_starts: Box<[u32]>,
}

impl SourceSnapshot {
    pub fn text(&self) -> &str {
        &self.text
    }

    pub fn tokens(&self) -> &[Token] {
        &self.tokens
    }

    pub fn line_starts(&self) -> &[u32] {
        &self.line_starts
    }
}

/// A fully parsed modern file with units in source order.
#[derive(Debug)]
pub struct ParsedFile {
    source: Arc<SourceSnapshot>,
    units: Box<[Arc<ParsedUnit>]>,
}

impl ParsedFile {
    pub fn source(&self) -> &SourceSnapshot {
        &self.source
    }

    pub fn units(&self) -> &[Arc<ParsedUnit>] {
        &self.units
    }

    /// Materialize the ordered, file-relative tree at the legacy resolver boundary.
    pub fn to_source_tree(&self) -> Ast {
        self.to_source_tree_with_bodies(&|_| true)
    }

    /// Apply a file-relative body selection to every root parse unit.
    pub fn to_source_tree_with_bodies(&self, include_body: &impl Fn(Span) -> bool) -> Ast {
        let mut tree = Ast::default();
        for unit in &self.units {
            append_unit(
                &mut tree,
                unit.syntax.to_source_tree_with_bodies(include_body),
            );
        }
        tree
    }
}

/// A published, read-only parse unit using typed, unit-local syntax tables.
#[derive(Debug)]
pub struct ParsedUnit {
    source: Arc<SourceSnapshot>,
    range: ParseUnit,
    syntax: IndexedAst,
}

impl ParsedUnit {
    pub fn source(&self) -> &SourceSnapshot {
        &self.source
    }

    pub fn span(&self) -> Span {
        self.range.span
    }

    /// The actual file tokens belonging to this unit, excluding synthetic EOF.
    pub fn tokens(&self) -> &[Token] {
        &self.source.tokens[self.range.tokens.clone()]
    }

    pub fn syntax(&self) -> &IndexedAst {
        &self.syntax
    }

    /// Handles for source bodies declared at the unit root, in parser order.
    pub fn bodies(self: &Arc<Self>) -> impl Iterator<Item = BodyHandle> + '_ {
        self.syntax
            .get_list(self.syntax.root().functions)
            .expect("root function list belongs to this unit")
            .iter()
            .copied()
            .filter_map(move |id| {
                let function = self.syntax.get(id).expect("function belongs to this unit");
                (function.body_kind == FunctionBodyKind::Source).then(|| BodyHandle {
                    unit: Arc::clone(self),
                    id,
                })
            })
    }
}

/// A routine body and its owning unit. Cloning a handle only retains the unit.
#[derive(Clone, Debug)]
pub struct BodyHandle {
    unit: Arc<ParsedUnit>,
    id: Id<FunctionNode>,
}

impl BodyHandle {
    pub fn unit(&self) -> &ParsedUnit {
        &self.unit
    }

    pub fn function(&self) -> &FunctionNode {
        self.unit
            .syntax
            .get(self.id)
            .expect("function belongs to this unit")
    }

    /// The owned statements of this body, with file-relative spans.
    pub fn materialize_body(&self) -> Vec<cyallo_ast::Stmt> {
        self.unit.syntax.thaw_body(self.id)
    }
}

/// Parse once and freeze each unit into its own indexed tables before publication.
/// The file is published only when all units succeed.
pub fn parse_modern_snapshot(
    source: Arc<str>,
    repl: bool,
    jobs: Jobs,
) -> Result<ParsedFile, Diagnostic> {
    let indexed = index_modern(&source)?;
    let parsed = jobs.map(indexed.units(), MIN_PARALLEL_UNITS, |_, i, _| {
        indexed.freeze_unit(i, repl)
    });
    let IndexedSource { tokens, units } = indexed;
    let tokens = tokens.into_tokens();
    publish_snapshot(source, tokens, units, parsed)
}

/// Parse a source graph with the same bounded job pool across all its units.
/// Return results in file order so callers can report the earliest error.
pub fn parse_modern_snapshots(sources: &[&str], jobs: Jobs) -> Vec<Result<ParsedFile, Diagnostic>> {
    let indexed = jobs.map(sources, MIN_PARALLEL_FILES, |_, _, &source| {
        index_modern(source)
    });
    let work: Vec<_> = indexed
        .iter()
        .enumerate()
        .flat_map(|(file, index)| {
            (0..index.as_ref().map_or(0, |index| index.units.len())).map(move |unit| (file, unit))
        })
        .collect();
    let parsed = jobs.map(&work, MIN_PARALLEL_UNITS, |_, _, &(file, unit)| {
        indexed[file]
            .as_ref()
            .expect("work belongs to an indexed file")
            .freeze_unit(unit, false)
    });
    let mut files: Vec<Vec<_>> = (0..sources.len()).map(|_| Vec::new()).collect();
    for ((file, _), unit) in work.into_iter().zip(parsed) {
        files[file].push(unit);
    }
    indexed
        .into_iter()
        .zip(sources)
        .zip(files)
        .map(|((index, &source), parsed)| {
            let IndexedSource { tokens, units } = index?;
            publish_snapshot(Arc::from(source), tokens.into_tokens(), units, parsed)
        })
        .collect()
}

fn publish_snapshot(
    source: Arc<str>,
    tokens: Box<[Token]>,
    units: Vec<ParseUnit>,
    parsed: Vec<Result<IndexedAst, Diagnostic>>,
) -> Result<ParsedFile, Diagnostic> {
    let _publish = stage(Stage::Publish);
    // Lexing rejects sources larger than the u32 span range.
    let line_starts = std::iter::once(0)
        .chain(
            source
                .bytes()
                .enumerate()
                .filter_map(|(i, byte)| (byte == b'\n').then_some((i + 1) as u32)),
        )
        .collect();
    let source = Arc::new(SourceSnapshot {
        text: source,
        tokens,
        line_starts,
    });
    let units = units
        .into_iter()
        .zip(parsed)
        .map(|(range, syntax)| {
            let syntax = syntax?;
            Ok(Arc::new(ParsedUnit {
                source: Arc::clone(&source),
                range,
                syntax,
            }))
        })
        .collect::<Result<_, Diagnostic>>()?;
    Ok(ParsedFile { source, units })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn snapshot_body_retains_its_unit_and_shared_source() {
        let source: Arc<str> = Arc::from("proc first() {}\nproc second() {}\n");
        let original = Arc::clone(&source);
        let snapshot = parse_modern_snapshot(source, false, Jobs(1)).unwrap();
        let [first, second] = snapshot.units() else {
            panic!("expected two parse units");
        };
        assert!(std::ptr::eq(first.source(), second.source()));
        let body = first.bodies().next().unwrap();
        let retained = body.clone();
        let other_unit = Arc::downgrade(second);
        drop(snapshot);

        assert!(other_unit.upgrade().is_none());
        assert!(std::ptr::eq(body.function(), retained.function()));
        assert_eq!(
            body.unit().syntax().name(body.function().name),
            Some("first")
        );
        assert_eq!(
            body.function()
                .span
                .source_text(body.unit().source().text()),
            "proc first() {}",
        );
        assert!(std::ptr::eq(original.as_ref(), body.unit().source().text()));
    }

    #[test]
    fn snapshot_preserves_first_diagnostic_in_source_order() {
        let source = "proc first() {}\nlet = 1\nlet good = 2\nlet = 3\n";
        let error = parse_modern_snapshot(Arc::from(source), false, Jobs(4)).unwrap_err();
        let legacy = parse_modern_with_jobs(source, false, Jobs(1)).unwrap_err();
        let first_invalid_token = source.find("= 1").unwrap() as u32;
        assert_eq!(
            error.span,
            Span::new(first_invalid_token, first_invalid_token + 1)
        );
        assert_eq!(error, legacy);
    }

    #[test]
    fn indexed_snapshots_round_trip_declarations_and_executable_syntax() {
        for source in [
            include_str!("../../../examples/sdk/worlds.yas"),
            include_str!("../../../examples/composites.yas"),
            include_str!("../../../examples/enums/patterns.yas"),
            include_str!("../../../examples/concurrency/generators/delegation.yas"),
            "mod external;\nmod empty {}\npub world W { import empty {}; import external; }\n",
        ] {
            let index = index_modern(source).unwrap();
            let expected = index.parse(false, Jobs(1)).unwrap();
            for jobs in [Jobs(1), Jobs(4)] {
                let indexed = parse_modern_snapshot(Arc::from(source), false, jobs).unwrap();
                let restored = indexed.to_source_tree();
                assert_eq!(format!("{expected:?}"), format!("{restored:?}"));
            }
        }
    }

    #[test]
    fn repeated_names_share_spelling_but_keep_occurrence_spans() {
        use cyallo_ast::indexed::{ExprSyntax, StmtSyntax};
        let source = "fn twice(n: i64) -> i64 {\n return n + n\n}\n";
        let file = parse_modern_snapshot(Arc::from(source), false, Jobs(1)).unwrap();
        let unit = &file.units()[0];
        let routine = unit.bodies().next().unwrap();
        let ast = unit.syntax();
        let [statement] = ast.get_list(routine.function().body).unwrap() else {
            panic!("expected a return statement");
        };
        let StmtSyntax::Return(value) = &ast.get(*statement).unwrap().kind else {
            panic!("expected a return statement");
        };
        let ExprSyntax::Binary(Operator::Add, left, right) = &ast.get(*value).unwrap().kind else {
            panic!("expected an addition");
        };
        let left = ast.get(*left).unwrap();
        let right = ast.get(*right).unwrap();
        let (ExprSyntax::Name(left_name), ExprSyntax::Name(right_name)) = (&left.kind, &right.kind)
        else {
            panic!("expected two name occurrences");
        };
        assert_eq!(left_name, right_name);
        assert_eq!(ast.name(*left_name), Some("n"));
        assert_ne!(left.span, right.span);
        assert_eq!(file.source().line_starts(), [0, 26, 40, 42]);
    }

    #[test]
    fn indexing_a_long_left_associated_chain_preserves_its_depth() {
        use cyallo_ast::indexed::{ExprSyntax, StmtSyntax};
        let terms = 4096;
        let source = format!("let n = {}\n", vec!["1"; terms].join(" + "));
        let file = parse_modern_snapshot(Arc::from(source), false, Jobs(1)).unwrap();
        let ast = file.units()[0].syntax();
        let [statement] = ast.get_list(ast.root().body).unwrap() else {
            panic!("expected a binding");
        };
        let StmtSyntax::Binding(_, _, _, _, value) = &ast.get(*statement).unwrap().kind else {
            panic!("expected a binding");
        };
        let mut at = *value;
        let mut operators = 0;
        while let ExprSyntax::Binary(Operator::Add, left, _) = &ast.get(at).unwrap().kind {
            at = *left;
            operators += 1;
        }
        assert_eq!(operators, terms - 1);
    }

    #[test]
    fn a_unit_borrows_tokens_and_stops_lookahead_at_its_own_eof() {
        let source = "let first = 1\n  let second = 2";
        let indexed = index_modern(source).unwrap();
        let range = indexed.units()[0].tokens.clone();
        let window = indexed.tokens.fragment(range.clone());

        assert!(std::ptr::eq(&window[0], &indexed.tokens[range.start]));
        let end = indexed.tokens[range.end].span.start;
        assert_eq!(end as usize, source.find("let second").unwrap());
        assert_eq!(window[range.len()].kind, TokenKind::Eof);
        assert_eq!(window[range.len()].span, Span::new(end, end));
        assert!(window.get(range.len() + 1).is_none());
    }

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
