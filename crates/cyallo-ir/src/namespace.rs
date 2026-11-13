//! Lexical, case-sensitive resolution shared by single-source and module builds.
use crate::intrinsic::Intrinsic;
use crate::stdlib::{self, BuiltinType, Item};
use cyallo_ast::*;
use std::collections::HashMap;
use std::path::Path;
type Result<T> = std::result::Result<T, Diagnostic>;
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
enum Kind {
    Module,
    Type,
    Value,
    Property,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SymbolOrigin {
    Source,
    Standard,
}
#[derive(Clone, Debug, PartialEq, Eq)]
struct Symbol {
    origin: SymbolOrigin,
    target: Box<str>,
    kind: Kind,
    owner: usize,
    public: bool,
    intrinsic: Option<Intrinsic>,
}
struct Node {
    path: String,
    ast: Ast,
    names: HashMap<String, Vec<Symbol>>,
    bindings: HashMap<u32, Box<str>>,
}
#[derive(Clone)]
struct Local {
    target: Box<str>,
    writable: bool,
}
struct Resolver {
    source_names: HashMap<Box<str>, Box<str>>,
    nodes: Vec<Node>,
    next_binding: usize,
}
fn error(message: impl Into<String>, span: Span) -> Diagnostic {
    Diagnostic::new("E140", message, span)
}
fn standard(path: &str) -> Option<Symbol> {
    let (target, kind) = match stdlib::item(path)? {
        Item::Module(p) => (p.to_owned(), Kind::Module),
        Item::Type(_) | Item::Trait => (path.to_owned(), Kind::Type),
        Item::Function(id) => (format!("$intrinsic${}", id.id().0), Kind::Value),
    };
    Some(Symbol {
        origin: SymbolOrigin::Standard,
        target: target.into(),
        kind,
        owner: usize::MAX,
        public: true,
        intrinsic: match stdlib::item(path) {
            Some(Item::Function(id)) => Some(id),
            _ => None,
        },
    })
}
impl Resolver {
    fn add(
        &mut self,
        path: String,
        mut ast: Ast,
        files: &mut [Option<Ast>],
        paths: &[String],
        dir: &Path,
    ) -> Result<usize> {
        if self.nodes.iter().any(|n| n.path == path) {
            return Err(error("duplicate module", Span::default()));
        }
        let index = self.nodes.len();
        let declarations = std::mem::take(&mut ast.modules);
        self.nodes.push(Node {
            path: path.clone(),
            ast,
            names: HashMap::new(),
            bindings: HashMap::new(),
        });
        for declaration in declarations {
            let child_path = format!("{path}::{}", declaration.name);
            let child_dir = dir.join(declaration.name.as_ref());
            let body = if let Some(body) = declaration.body {
                *body
            } else {
                let candidates = [
                    dir.join(format!("{}.ya", declaration.name)),
                    child_dir.join("mod.ya"),
                ];
                let matches: Vec<_> = paths
                    .iter()
                    .enumerate()
                    .filter(|(_, p)| candidates.iter().any(|c| Path::new(p) == c))
                    .map(|(i, _)| i)
                    .collect();
                if matches.len() != 1 {
                    return Err(error(
                        format!(
                            "expected exactly one source for module {}",
                            declaration.name
                        ),
                        declaration.span,
                    ));
                }
                files[matches[0]].take().ok_or_else(|| {
                    error(
                        "module source is already owned by another module",
                        declaration.span,
                    )
                })?
            };
            self.add(child_path.clone(), body, files, paths, &child_dir)?;
            self.nodes[index]
                .names
                .entry(declaration.name.to_string())
                .or_default()
                .push(Symbol {
                    origin: SymbolOrigin::Source,
                    target: child_path.into(),
                    kind: Kind::Module,
                    owner: index,
                    public: declaration.public,
                    intrinsic: None,
                });
        }
        Ok(index)
    }
    fn declarations(&mut self) -> Result<()> {
        for (index, node) in self.nodes.iter_mut().enumerate() {
            let mut definitions: Vec<(Box<str>, Kind, bool)> = node
                .ast
                .functions
                .iter()
                .map(|f| {
                    (
                        f.name.clone(),
                        Kind::Value,
                        node.ast.exports.contains(&f.name),
                    )
                })
                .collect();
            definitions.extend(node.ast.records.iter().map(|r| {
                (
                    r.name.clone(),
                    Kind::Type,
                    node.ast
                        .exports
                        .iter()
                        .any(|e| e.as_ref() == format!("$type${}", r.name)),
                )
            }));
            definitions.extend(node.ast.traits.iter().map(|t| {
                (
                    t.name.clone(),
                    Kind::Type,
                    node.ast
                        .exports
                        .iter()
                        .any(|e| e.as_ref() == format!("$type${}", t.name)),
                )
            }));
            definitions.extend(node.ast.properties.iter().map(|p| {
                (
                    p.name.clone(),
                    Kind::Property,
                    node.ast
                        .exports
                        .iter()
                        .any(|e| e.as_ref() == format!("$property${}", p.name)),
                )
            }));
            definitions.extend(node.ast.body.iter().filter_map(|s| match &s.kind {
                StmtKind::Constant(n, _) | StmtKind::Declare(n, _) => {
                    Some((n.clone(), Kind::Value, node.ast.exports.contains(n)))
                }
                _ => None,
            }));
            for (name, kind, public) in definitions {
                let entries = node.names.entry(name.to_string()).or_default();
                if entries.iter().any(|s| s.kind == kind) {
                    return Err(error(
                        format!("duplicate declaration {name}"),
                        Span::default(),
                    ));
                }
                let target: Box<str> = format!("$module{index}${kind:?}${name}").into();
                self.source_names.insert(target.clone(), name.clone());
                entries.push(Symbol {
                    origin: SymbolOrigin::Source,
                    target,
                    kind,
                    owner: index,
                    public,
                    intrinsic: None,
                });
            }
            let mut versions: HashMap<Box<str>, (usize, bool)> = HashMap::new();
            for statement in &node.ast.body {
                let StmtKind::Binding(name, _, constant, _, _) = &statement.kind else {
                    continue;
                };
                let version = versions.entry(name.clone()).or_insert((0, *constant));
                let entries = node.names.entry(name.to_string()).or_default();
                if (*constant || version.1 || version.0 == 0)
                    && entries.iter().any(|symbol| symbol.kind == Kind::Value)
                {
                    return Err(error(
                        format!("duplicate declaration {name}"),
                        statement.span,
                    ));
                }
                let target: Box<str> = if version.0 == 0 {
                    format!("$module{index}$Value${name}").into()
                } else {
                    format!("$module{index}$Value${name}$shadow{}", version.0).into()
                };
                version.0 += 1;
                self.source_names.insert(target.clone(), name.clone());
                node.bindings.insert(statement.span.start, target.clone());
                entries.retain(|symbol| symbol.kind != Kind::Value);
                entries.push(Symbol {
                    origin: SymbolOrigin::Source,
                    target,
                    kind: Kind::Value,
                    owner: index,
                    public: node.ast.exports.contains(name),
                    intrinsic: None,
                });
            }
        }
        Ok(())
    }
    fn accessible(&self, from: usize, symbol: &Symbol) -> bool {
        symbol.public
            || symbol.origin == SymbolOrigin::Standard
            || self.nodes[from].path == self.nodes[symbol.owner].path
            || self.nodes[from]
                .path
                .starts_with(&format!("{}::", self.nodes[symbol.owner].path))
    }
    fn path(&self, from: usize, path: &str, kind: Option<Kind>) -> Option<Symbol> {
        let parts: Vec<_> = path.split("::").collect();
        let select = |symbols: &[Symbol], requested: Option<Kind>| {
            let matches: Vec<_> = symbols
                .iter()
                .filter(|s| requested.is_none_or(|k| s.kind == k))
                .collect();
            (matches.len() == 1).then(|| matches[0].clone())
        };
        let mut position = 1;
        let mut symbol = match parts[0] {
            "std" => standard("std")?,
            "crate" => Symbol {
                origin: SymbolOrigin::Source,
                target: "crate".into(),
                kind: Kind::Module,
                owner: 0,
                public: true,
                intrinsic: None,
            },
            "self" => Symbol {
                origin: SymbolOrigin::Source,
                target: self.nodes[from].path.clone().into(),
                kind: Kind::Module,
                owner: from,
                public: true,
                intrinsic: None,
            },
            "super" => {
                let parent = self.nodes[from].path.rsplit_once("::")?.0;
                Symbol {
                    origin: SymbolOrigin::Source,
                    target: parent.into(),
                    kind: Kind::Module,
                    owner: from,
                    public: true,
                    intrinsic: None,
                }
            }
            first => {
                if let Some(symbols) = self.nodes[from].names.get(first) {
                    select(symbols, if parts.len() == 1 { kind } else { None })?
                } else {
                    let (_, target) = stdlib::PRELUDE.iter().find(|(name, _)| *name == first)?;
                    standard(target)?
                }
            }
        };
        while position < parts.len() {
            let part = parts[position];
            position += 1;
            if symbol.origin == SymbolOrigin::Standard {
                symbol = standard(&format!("{}::{part}", symbol.target))?;
            } else if symbol.kind == Kind::Module {
                if part == "super" {
                    symbol.target = symbol.target.rsplit_once("::")?.0.into();
                    continue;
                }
                let owner = self
                    .nodes
                    .iter()
                    .position(|n| n.path == symbol.target.as_ref())?;
                let symbols = self.nodes[owner].names.get(part)?;
                symbol = select(symbols, if position == parts.len() { kind } else { None })?;
                if !self.accessible(from, &symbol) {
                    return None;
                }
            } else {
                return None;
            }
        }
        if kind.is_some_and(|k| k != symbol.kind) {
            None
        } else {
            Some(symbol)
        }
    }
    fn imports(&mut self) -> Result<()> {
        use std::collections::HashSet;
        let imports: Vec<_> = self
            .nodes
            .iter()
            .enumerate()
            .flat_map(|(module, node)| {
                node.ast
                    .imports
                    .iter()
                    .cloned()
                    .map(move |import| (module, import))
            })
            .collect();
        let mut installed: Vec<HashSet<(String, Kind, Box<str>)>> =
            vec![HashSet::new(); imports.len()];
        let mut origins: HashMap<(usize, String, Kind), Vec<usize>> = HashMap::new();
        loop {
            let mut changed = false;
            let mut unresolved = None;
            for (ordinal, (module, import)) in imports.iter().enumerate() {
                let Some(target) = self.path(*module, &import.path, None) else {
                    unresolved.get_or_insert(import);
                    continue;
                };
                let entries = if import.glob {
                    if target.kind != Kind::Module {
                        return Err(error("glob import requires a module", import.span));
                    }
                    if target.origin == SymbolOrigin::Standard {
                        stdlib::exports(&target.target)
                            .into_iter()
                            .map(|(name, path)| (name, standard(&path).expect("standard export")))
                            .collect::<Vec<_>>()
                    } else {
                        let node = self
                            .nodes
                            .iter()
                            .find(|node| node.path == target.target.as_ref())
                            .expect("resolved module");
                        node.names
                            .iter()
                            .flat_map(|(name, symbols)| {
                                symbols
                                    .iter()
                                    .filter(|symbol| symbol.public)
                                    .map(move |symbol| (name.clone(), symbol.clone()))
                            })
                            .collect()
                    }
                } else {
                    vec![(
                        import
                            .alias
                            .as_deref()
                            .unwrap_or_else(|| import.path.rsplit("::").next().unwrap())
                            .to_owned(),
                        target,
                    )]
                };
                for (name, mut symbol) in entries {
                    if !installed[ordinal].insert((
                        name.clone(),
                        symbol.kind,
                        symbol.target.clone(),
                    )) {
                        continue;
                    }
                    if import.public && !symbol.public {
                        return Err(error(
                            "cannot publicly reexport a private declaration",
                            import.span,
                        ));
                    }
                    let key = (*module, name.clone(), symbol.kind);
                    let previous = origins.entry(key).or_default();
                    let existing = self.nodes[*module].names.entry(name.clone()).or_default();
                    if let Some(existing) = existing
                        .iter_mut()
                        .find(|existing| existing.kind == symbol.kind)
                    {
                        if existing.target != symbol.target
                            || (!import.glob
                                && (previous.is_empty()
                                    || previous.iter().any(|previous| !imports[*previous].1.glob)))
                        {
                            return Err(error(format!("ambiguous import {name}"), import.span));
                        }
                        if import.public && !existing.public {
                            existing.public = true;
                            changed = true;
                        }
                    } else {
                        symbol.public = import.public;
                        symbol.owner = *module;
                        existing.push(symbol);
                        changed = true;
                    }
                    previous.push(ordinal);
                }
            }
            if !changed {
                if let Some(import) = unresolved {
                    return Err(error(
                        format!("unresolved or private import {}", import.path),
                        import.span,
                    ));
                }
                return Ok(());
            }
        }
    }
    fn symbol(&self, module: usize, name: &str, kind: Kind, span: Span) -> Result<Box<str>> {
        self.path(module, name, Some(kind))
            .map(|s| s.target)
            .ok_or_else(|| error(format!("unresolved name {name}"), span))
    }
    fn ty(&self, module: usize, ty: &mut Type, params: &[Box<str>], span: Span) -> Result<()> {
        match ty {
            Type::Lifetime(_) => {
                return Err(error("lifetime is only valid as a lifetime argument", span));
            }
            Type::Named(n) if params.contains(n) => {}
            Type::Named(n) | Type::TraitObject(n) => {
                let target = self.symbol(module, n, Kind::Type, span)?;
                if let Some(Item::Type(id)) = stdlib::item(&target) {
                    *ty = match id {
                        BuiltinType::Any => Type::Any,
                        BuiltinType::Symbol => Type::Symbol,
                        BuiltinType::Context => Type::Context,
                        BuiltinType::Registration => Type::Registration,
                        BuiltinType::Storage(
                            kind @ (StorageKind::ParseError
                            | StorageKind::Layout
                            | StorageKind::Allocation
                            | StorageKind::AllocError),
                        ) => Type::Storage(kind, Box::new(Type::Void)),
                        _ => return Err(error("generic type requires element arguments", span)),
                    };
                } else {
                    *n = target;
                }
            }
            Type::Applied(n, arguments) => {
                let target = self.symbol(module, n, Kind::Type, span)?;
                if matches!(
                    stdlib::item(&target),
                    Some(Item::Type(BuiltinType::Storage(StorageKind::Weak)))
                ) {
                    let lifetime = if matches!(arguments.first(), Some(Type::Lifetime(_))) {
                        let Type::Lifetime(lifetime) = arguments.remove(0) else {
                            unreachable!()
                        };
                        Some(lifetime)
                    } else {
                        None
                    };
                    if arguments.len() != 1 {
                        return Err(error(
                            "Weak requires a pointee type and an optional allocator lifetime",
                            span,
                        ));
                    }
                    let mut pointee = arguments.remove(0);
                    self.ty(module, &mut pointee, params, span)?;
                    *ty = Type::Storage(
                        StorageKind::Weak,
                        Box::new(Type::Managed {
                            kind: MemoryKind::Rc,
                            lifetime,
                            pointee: Box::new(pointee),
                        }),
                    );
                    return Ok(());
                }
                for t in arguments.iter_mut() {
                    self.ty(module, t, params, span)?;
                }
                let target = self.symbol(module, n, Kind::Type, span)?;
                if let Some(Item::Type(id)) = stdlib::item(&target) {
                    if id == BuiltinType::Storage(StorageKind::Generator) {
                        if arguments.len() != 3 { return Err(error("Generator requires yield, resume, and return types", span)); }
                        *ty = Type::Storage(StorageKind::Generator, Box::new(Type::Tuple(std::mem::take(arguments))));
                        return Ok(());
                    }
                    if id == BuiltinType::Storage(StorageKind::Result) {
                        let [value, error]: [Type; 2] =
                            std::mem::take(arguments).try_into().map_err(|_| {
                                error("Result requires a value type and an error type", span)
                            })?;
                        *ty = crate::lookup::result_type(value, error);
                        return Ok(());
                    }
                    if arguments.len() != 1 {
                        return Err(error("expected one element type", span));
                    }
                    let inner = Box::new(arguments.remove(0));
                    *ty = match id {
                        BuiltinType::ServiceKey => Type::ServiceKey(inner),
                        BuiltinType::ServiceRef => Type::ServiceRef(inner),
                        BuiltinType::Storage(StorageKind::Weak) => Type::Storage(
                            StorageKind::Weak,
                            Box::new(Type::Managed {
                                kind: MemoryKind::Rc,
                                lifetime: None,
                                pointee: inner,
                            }),
                        ),
                        BuiltinType::Storage(kind) => Type::Storage(kind, inner),
                        _ => return Err(error("type does not accept element arguments", span)),
                    };
                } else {
                    *n = target;
                }
            }
            Type::Array(_, t)
            | Type::SparseArray(t)
            | Type::Borrowed(t)
            | Type::Slice(t)
            | Type::ServiceKey(t)
            | Type::ServiceRef(t)
            | Type::Storage(_, t) => self.ty(module, t, params, span)?,
            Type::Reference { pointee, .. } | Type::Managed { pointee, .. } => {
                self.ty(module, pointee, params, span)?
            }
            Type::Tuple(ts) => {
                for t in ts {
                    self.ty(module, t, params, span)?;
                }
            }
            Type::Function(_, ps, r) => {
                for (t, _) in ps {
                    self.ty(module, t, params, span)?;
                }
                self.ty(module, r, params, span)?;
            }
            Type::Record(_, fs) => {
                for (_, t) in fs {
                    self.ty(module, t, params, span)?;
                }
            }
            _ => {}
        }
        Ok(())
    }
    fn local(scopes: &[HashMap<String, Local>], name: &str) -> Option<Local> {
        scopes.iter().rev().find_map(|s| s.get(name).cloned())
    }
    fn fresh(&mut self, name: &str) -> Box<str> {
        self.next_binding += 1;
        let target: Box<str> = format!("$binding{}${name}", self.next_binding).into();
        self.source_names.insert(target.clone(), name.into());
        target
    }
    fn expr(
        &mut self,
        module: usize,
        e: &mut Expr,
        scopes: &mut Vec<HashMap<String, Local>>,
        params: &[Box<str>],
    ) -> Result<()> {
        let constructor = match &e.kind {
            ExprKind::Index(base, _) => match &base.kind {
                ExprKind::Name(n) if Self::local(scopes, n).is_none() => Some(n.as_ref()),
                _ => None,
            },
            ExprKind::TypedCollection(n, _, _) => Some(n.as_ref()),
            _ => None,
        }
        .and_then(|name| self.path(module, name, Some(Kind::Type)))
        .and_then(|symbol| match stdlib::item(&symbol.target) {
            Some(Item::Type(BuiltinType::Storage(StorageKind::Vec))) => Some(Intrinsic::VecLiteral),
            Some(Item::Type(BuiltinType::Storage(StorageKind::VecDeque))) => {
                Some(Intrinsic::DequeLiteral)
            }
            _ => None,
        });
        if let Some(id) = constructor {
            match &mut e.kind {
                ExprKind::Index(_, args) => {
                    e.kind = ExprKind::IntrinsicCall(id.id(), vec![], std::mem::take(args))
                }
                ExprKind::TypedCollection(_, types, args) => {
                    e.kind = ExprKind::IntrinsicCall(
                        id.id(),
                        std::mem::take(types),
                        std::mem::take(args),
                    )
                }
                _ => {}
            }
        }
        match &mut e.kind {
            ExprKind::Yield(value, _) => self.expr(module, value, scopes, params)?,
            ExprKind::ManagedNew(_, allocator, value) => {
                if let Some(allocator) = allocator {
                    self.expr(module, allocator, scopes, params)?;
                }
                self.expr(module, value, scopes, params)?;
            }
            ExprKind::TypedCollection(..) => {
                return Err(error(
                    "type does not define a collection initializer",
                    e.span,
                ));
            }
            ExprKind::IntrinsicCall(_, types, arguments) => {
                for t in types {
                    self.ty(module, t, params, e.span)?;
                }
                for a in arguments {
                    self.expr(module, a, scopes, params)?;
                }
            }
            ExprKind::Name(n) => {
                *n = if let Some(local) = Self::local(scopes, n) {
                    local.target
                } else if let Some(symbol) = self.path(module, n, Some(Kind::Value)) {
                    symbol.target
                } else {
                    e.kind = ExprKind::Invalid(Diagnostic::new(
                        "E106",
                        format!("undeclared identifier {n}"),
                        e.span,
                    ));
                    return Ok(());
                }
            }
            ExprKind::Call(n, args) | ExprKind::GenericCall(n, _, args) => {
                let symbol = if Self::local(scopes, n).is_none() {
                    self.path(module, n, Some(Kind::Value))
                } else {
                    None
                };
                for a in args.iter_mut() {
                    self.expr(module, a, scopes, params)?;
                }
                let mut types = match &mut e.kind {
                    ExprKind::GenericCall(_, ts, _) => ts.clone(),
                    _ => vec![],
                };
                for t in &mut types {
                    self.ty(module, t, params, e.span)?;
                }
                if let Some(id) = symbol.as_ref().and_then(|s| s.intrinsic) {
                    let args = match &mut e.kind {
                        ExprKind::Call(_, a) | ExprKind::GenericCall(_, _, a) => std::mem::take(a),
                        _ => unreachable!(),
                    };
                    e.kind = ExprKind::IntrinsicCall(id.id(), types, args);
                } else {
                    let name = match &mut e.kind {
                        ExprKind::Call(n, _) | ExprKind::GenericCall(n, _, _) => n,
                        _ => unreachable!(),
                    };
                    *name = if let Some(local) = Self::local(scopes, name) {
                        local.target
                    } else {
                        if let Some(symbol) = symbol {
                            symbol.target
                        } else {
                            e.kind = ExprKind::Invalid(Diagnostic::new(
                                "E106",
                                format!("undeclared function {name}"),
                                e.span,
                            ));
                            return Ok(());
                        }
                    };
                    if let ExprKind::GenericCall(_, ts, _) = &mut e.kind {
                        *ts = types
                    }
                }
            }
            ExprKind::Record(n, fs) | ExprKind::GenericRecord(n, _, fs) => {
                *n = self.symbol(module, n, Kind::Type, e.span)?;
                for (_, value) in fs {
                    self.expr(module, value, scopes, params)?;
                }
                if let ExprKind::GenericRecord(_, ts, _) = &mut e.kind {
                    for t in ts {
                        self.ty(module, t, params, e.span)?;
                    }
                }
            }
            ExprKind::ArrayLiteral(es) | ExprKind::Tuple(es) => {
                for value in es {
                    self.expr(module, value, scopes, params)?;
                }
            }
            ExprKind::Field(base, name) => {
                self.expr(module, base, scopes, params)?;
                if let Some(property) = self.path(module, name, Some(Kind::Property)) {
                    e.kind = ExprKind::ExtensionField(base.clone(), name.clone(), property.target);
                }
            }
            ExprKind::Parenthesized(base)
            | ExprKind::Unary(_, base)
            | ExprKind::TupleField(base, _) => self.expr(module, base, scopes, params)?,
            ExprKind::Cast(base, t) => {
                self.expr(module, base, scopes, params)?;
                self.ty(module, t, params, e.span)?;
            }
            ExprKind::Binary(_, a, b) => {
                self.expr(module, a, scopes, params)?;
                self.expr(module, b, scopes, params)?;
            }
            ExprKind::Index(base, indices) | ExprKind::Invoke(base, indices) => {
                self.expr(module, base, scopes, params)?;
                for index in indices {
                    self.expr(module, index, scopes, params)?;
                }
            }
            ExprKind::Closure(f, refs) => {
                for capture in refs {
                    let n = &mut capture.name;
                    *n = if let Some(local) = Self::local(scopes, n) {
                        local.target
                    } else {
                        self.symbol(module, n, Kind::Value, e.span)?
                    };
                }
                self.function(module, f, scopes, params)?;
            }
            ExprKind::Frozen(_, t) => self.ty(module, t, params, e.span)?,
            ExprKind::Literal(_)
            | ExprKind::ClosureValue(..)
            | ExprKind::ExtensionField(..)
            | ExprKind::Invalid(_) => {}
        }
        Ok(())
    }
    fn block(
        &mut self,
        module: usize,
        body: &mut [Stmt],
        scopes: &mut Vec<HashMap<String, Local>>,
        params: &[Box<str>],
        top: bool,
    ) -> Result<()> {
        scopes.push(HashMap::new());
        for s in body {
            match &mut s.kind {
                StmtKind::Binding(name, ty, _, mutable, value) => {
                    if let Some(t) = ty {
                        self.ty(module, t, params, s.span)?;
                    }
                    self.expr(module, value, scopes, params)?;
                    let target = if top {
                        self.nodes[module]
                            .bindings
                            .get(&s.span.start)
                            .cloned()
                            .expect("declared binding")
                    } else {
                        self.fresh(name)
                    };
                    let writable = *mutable
                        || matches!(ty, Some(Type::Reference { mutable: true, .. }))
                        || matches!(value.kind, ExprKind::Unary(Operator::BorrowMut, _));
                    scopes.last_mut().unwrap().insert(
                        name.to_string(),
                        Local {
                            target: target.clone(),
                            writable,
                        },
                    );
                    *name = target;
                }
                StmtKind::Constant(name, value) => {
                    self.expr(module, value, scopes, params)?;
                    let target = if top {
                        self.symbol(module, name, Kind::Value, s.span)?
                    } else {
                        self.fresh(name)
                    };
                    scopes.last_mut().unwrap().insert(
                        name.to_string(),
                        Local {
                            target: target.clone(),
                            writable: false,
                        },
                    );
                    *name = target;
                }
                StmtKind::Assign(target, value) | StmtKind::Compound(target, _, value) => {
                    fn root(e: &Expr) -> Option<&str> {
                        match &e.kind {
                            ExprKind::Name(n) => Some(n),
                            ExprKind::Parenthesized(b) => root(b),
                            _ => None,
                        }
                    }
                    if let Some(name) = root(target)
                        && let Some(local) = Self::local(scopes, name)
                        && !local.writable
                    {
                        return Err(Diagnostic::new(
                            "E107",
                            "cannot modify an immutable binding",
                            target.span,
                        ));
                    }
                    self.expr(module, target, scopes, params)?;
                    self.expr(module, value, scopes, params)?;
                }
                StmtKind::Evaluate(e) | StmtKind::Return(e) | StmtKind::Input(e) => {
                    self.expr(module, e, scopes, params)?
                }
                StmtKind::Output(es) => {
                    for e in es {
                        self.expr(module, e, scopes, params)?;
                    }
                }
                StmtKind::Call(n, es) => {
                    *n = self.symbol(module, n, Kind::Value, s.span)?;
                    for e in es {
                        self.expr(module, e, scopes, params)?;
                    }
                }
                StmtKind::If(condition, a, b) => {
                    self.expr(module, condition, scopes, params)?;
                    self.block(module, a, scopes, params, false)?;
                    self.block(module, b, scopes, params, false)?;
                }
                StmtKind::While(condition, b) | StmtKind::Repeat(b, condition) => {
                    self.expr(module, condition, scopes, params)?;
                    self.block(module, b, scopes, params, false)?;
                }
                StmtKind::For(name, first, last, step, b) => {
                    self.expr(module, first, scopes, params)?;
                    self.expr(module, last, scopes, params)?;
                    if let Some(e) = step {
                        self.expr(module, e, scopes, params)?;
                    }
                    let target = self.fresh(name);
                    scopes.push(HashMap::from([(
                        name.to_string(),
                        Local {
                            target: target.clone(),
                            writable: false,
                        },
                    )]));
                    *name = target;
                    self.block(module, b, scopes, params, false)?;
                    scopes.pop();
                }
                StmtKind::Case(..) | StmtKind::Declare(..) => {
                    return Err(error("unsupported modern statement", s.span));
                }
                StmtKind::Break | StmtKind::Continue | StmtKind::ReturnVoid => {}
            }
        }
        scopes.pop();
        Ok(())
    }
    fn function(
        &mut self,
        module: usize,
        f: &mut Function,
        outer: &[HashMap<String, Local>],
        inherited_params: &[Box<str>],
    ) -> Result<()> {
        let mut scopes = outer.to_vec();
        let mut parameters = HashMap::new();
        let mut type_params = inherited_params.to_vec();
        type_params.extend(f.type_params.iter().cloned());
        for p in &mut f.params {
            self.ty(module, &mut p.ty, &type_params, p.span)?;
            if parameters.contains_key(p.name.as_ref()) {
                return Err(error("duplicate parameter", p.span));
            }
            let target = self.fresh(&p.name);
            parameters.insert(
                p.name.to_string(),
                Local {
                    target: target.clone(),
                    writable: p.by_ref || matches!(p.ty, Type::Reference { mutable: true, .. }),
                },
            );
            p.name = target;
        }
        self.ty(module, &mut f.result, &type_params, f.span)?;
        scopes.push(parameters);
        self.block(module, &mut f.body, &mut scopes, &type_params, false)
    }
    fn finish(mut self) -> Result<Ast> {
        self.declarations()?;
        self.imports()?;
        let mut linked = Ast::default();
        linked.traits.push(crate::memory::allocator_declaration());
        let mut constants = Vec::new();
        for index in 0..self.nodes.len() {
            let mut ast = std::mem::take(&mut self.nodes[index].ast);
            for record in &mut ast.records {
                record.name = self.symbol(index, &record.name, Kind::Type, record.span)?;
                for (_, t) in &mut record.fields {
                    self.ty(index, t, &record.type_params, record.span)?;
                }
            }
            for f in &mut ast.functions {
                f.name = self.symbol(index, &f.name, Kind::Value, f.span)?;
                self.function(index, f, &[], &[])?;
            }
            for t in &mut ast.traits {
                t.name = self.symbol(index, &t.name, Kind::Type, t.span)?;
                for f in &mut t.methods {
                    self.function(index, f, &[], &[])?;
                }
            }
            for imp in &mut ast.implementations {
                imp.trait_name = self.symbol(index, &imp.trait_name, Kind::Type, imp.span)?;
                self.ty(index, &mut imp.receiver, &[], imp.span)?;
                for f in &mut imp.methods {
                    self.function(index, f, &[], &[])?;
                }
            }
            for property in &mut ast.properties {
                self.ty(index, &mut property.receiver, &[], property.span)?;
                property.getter.name =
                    self.symbol(index, &property.name, Kind::Property, property.span)?;
                self.function(index, &mut property.getter, &[], &[])?;
            }
            if index != 0
                && ast.body.iter().any(|s| {
                    !matches!(
                        s.kind,
                        StmtKind::Binding(_, _, true, _, _) | StmtKind::Constant(..)
                    )
                })
            {
                return Err(error(
                    "module initialization requires constants or an explicit procedure",
                    ast.body[0].span,
                ));
            }
            self.block(index, &mut ast.body, &mut Vec::new(), &[], true)?;
            if index == 0 {
                linked.body.append(&mut ast.body);
            } else {
                constants.append(&mut ast.body);
            }
            linked.functions.append(&mut ast.functions);
            linked.records.append(&mut ast.records);
            linked.traits.append(&mut ast.traits);
            linked.implementations.append(&mut ast.implementations);
            linked.properties.append(&mut ast.properties);
        }
        // Topologically order imported constants without executing module code.
        let names: HashMap<_, _> = constants
            .iter()
            .enumerate()
            .filter_map(|(i, s)| match &s.kind {
                StmtKind::Binding(n, ..) | StmtKind::Constant(n, _) => Some((n.clone(), i)),
                _ => None,
            })
            .collect();
        let mut dependencies = vec![Vec::new(); constants.len()];
        for (i, s) in constants.iter_mut().enumerate() {
            cyallo_ast::visit::visit_block(
                std::slice::from_mut(s),
                &mut |e| {
                    if let ExprKind::Name(n) = &e.kind
                        && let Some(j) = names.get(n)
                    {
                        dependencies[i].push(*j);
                    }
                    Ok(())
                },
                &mut |_, _| Ok(()),
            )?;
        }
        fn order(
            i: usize,
            deps: &[Vec<usize>],
            marks: &mut [u8],
            out: &mut Vec<usize>,
            body: &[Stmt],
        ) -> Result<()> {
            if marks[i] == 2 {
                return Ok(());
            }
            if marks[i] == 1 {
                return Err(error("cyclic constant initialization", body[i].span));
            }
            marks[i] = 1;
            for j in &deps[i] {
                order(*j, deps, marks, out, body)?;
            }
            marks[i] = 2;
            out.push(i);
            Ok(())
        }
        let mut marks = vec![0; constants.len()];
        let mut sorted = Vec::new();
        for i in 0..constants.len() {
            order(i, &dependencies, &mut marks, &mut sorted, &constants)?;
        }
        let mut body: Vec<_> = sorted.into_iter().map(|i| constants[i].clone()).collect();
        body.append(&mut linked.body);
        linked.body = body;
        linked.source_names = self.source_names;
        Ok(linked)
    }
}
pub fn link(asts: Vec<Ast>, paths: &[String], entry: usize) -> Result<Ast> {
    let mut files: Vec<_> = asts.into_iter().map(Some).collect();
    let root = files[entry]
        .take()
        .ok_or_else(|| error("missing entry module", Span::default()))?;
    let dir = Path::new(&paths[entry]).parent().unwrap_or(Path::new(""));
    let mut resolver = Resolver {
        source_names: HashMap::new(),
        nodes: Vec::new(),
        next_binding: 0,
    };
    resolver.add("crate".into(), root, &mut files, paths, dir)?;
    resolver.finish()
}
pub fn single(ast: Ast) -> Result<Ast> {
    link(vec![ast], &["main.ya".into()], 0)
}
