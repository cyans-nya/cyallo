//! Compilation-instance-owned cache of checked modules, independent of link slots.
use crate::{analysis::CheckedModule, ir, linking::Relocation, services::Metadata};
use cyallo_ast::*;
use std::{
    collections::{HashMap, HashSet},
    sync::{Arc, Mutex},
};
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ModuleKey {
    path: Box<str>,
    // Digests of the normalized debug text, not the text: every lookup hashes and
    // compares the key, and the native-declaration text covers the whole standard
    // package, so hashing it per module dominated compile time.
    source: crate::sdk::Digest,
    dependencies: Vec<(Box<str>, crate::sdk::Digest)>,
    native: crate::sdk::Digest,
    permissions: crate::sdk::Digest,
}
/// Streams a value's structure into BLAKE3. Identity comes from `Hash` on the
/// declarations themselves, never from their debug text, whose format is not a contract.
struct Fingerprint(blake3::Hasher);
impl std::hash::Hasher for Fingerprint {
    fn write(&mut self, bytes: &[u8]) {
        self.0.update(bytes);
    }
    fn finish(&self) -> u64 {
        let digest = self.0.finalize();
        u64::from_le_bytes(
            digest.as_bytes()[..8]
                .try_into()
                .expect("a digest has 32 bytes"),
        )
    }
}
pub(crate) fn fingerprint(value: &impl std::hash::Hash) -> crate::sdk::Digest {
    let mut state = Fingerprint(blake3::Hasher::new());
    value.hash(&mut state);
    crate::sdk::Digest(*state.0.finalize().as_bytes())
}
#[derive(Debug, Clone)]
pub struct LinkLayout {
    pub routines: Vec<Box<str>>,
    pub globals: Vec<ir::Local>,
    pub services: Arc<Metadata>,
    pub source_files: Vec<crate::module_graph::SourceFile>,
    pub source_len: usize,
}
impl LinkLayout {
    fn relocation(&self, new: &Self) -> Arc<Relocation> {
        let routines: HashMap<_, _> = new
            .routines
            .iter()
            .enumerate()
            .map(|(i, n)| (n.as_ref(), i))
            .collect();
        let globals: HashMap<_, _> = new
            .globals
            .iter()
            .enumerate()
            .map(|(i, l)| (l.name.as_ref(), i))
            .collect();
        Arc::new(Relocation {
            routines: self
                .routines
                .iter()
                .map(|n| routines.get(n.as_ref()).copied())
                .collect(),
            globals: self
                .globals
                .iter()
                .map(|l| globals.get(l.name.as_ref()).copied())
                .collect(),
            implementations: self
                .services
                .implementations
                .iter()
                .map(|old| {
                    new.services.implementations.iter().position(|new_imp| {
                        old.receiver == new_imp.receiver
                            && self.services.traits[old.trait_id].name
                                == new.services.traits[new_imp.trait_id].name
                    })
                })
                .collect(),
            keys: self
                .services
                .keys
                .iter()
                .map(|key| {
                    let source = self
                        .source_files
                        .iter()
                        .find(|f| key.span >= f.span.start && key.span < f.span.end);
                    new.services.keys.iter().position(|candidate| {
                        key.label == candidate.label
                            && key.ty == candidate.ty
                            && source.map_or(key.span == candidate.span, |old| {
                                new.source_files
                                    .iter()
                                    .find(|f| f.id == old.id)
                                    .is_some_and(|current| {
                                        candidate.span
                                            == current.span.start + key.span - old.span.start
                                    })
                            })
                    })
                })
                .collect(),
            routine_names: self.routines.clone(),
            global_names: self.globals.iter().map(|l| l.name.clone()).collect(),
            globals_now: new.globals.clone().into(),
            sources_before: self.source_files.clone(),
            sources_now: new.source_files.clone(),
            source_len_before: self.source_len,
            source_len_now: new.source_len,
        })
    }
}
#[derive(Debug)]
struct Entry {
    unit: Arc<CheckedModule>,
    layout: Arc<LinkLayout>,
}
#[derive(Debug, Default)]
pub struct ModuleCache {
    entries: Mutex<HashMap<ModuleKey, Entry>>,
}
impl ModuleCache {
    pub fn get(&self, key: &ModuleKey, layout: &LinkLayout) -> Option<Arc<CheckedModule>> {
        let entries = self.entries.lock().expect("module cache lock");
        let old = entries.get(key)?;
        let relocation = old.layout.relocation(layout);
        let mut unit = old.unit.as_ref().clone();
        unit.globals = unit
            .globals
            .iter()
            .map(|&id| relocation.globals[id])
            .collect::<Option<_>>()?;
        unit.initializer = match unit.initializer {
            Some(id) => Some(relocation.routines[id]?),
            None => None,
        };
        unit.routines = unit
            .routines
            .iter()
            .map(|(id, r)| {
                Some((
                    relocation.routines[*id]?,
                    ir::Routine::relocated(r.clone(), relocation.clone(), *id == 0),
                ))
            })
            .collect::<Option<_>>()?;
        Some(Arc::new(unit))
    }
    pub fn insert(&self, key: ModuleKey, unit: Arc<CheckedModule>, layout: Arc<LinkLayout>) {
        self.entries
            .lock()
            .expect("module cache lock")
            .entry(key)
            .or_insert(Entry { unit, layout });
    }
}
pub(crate) fn keys(
    graph: &crate::module_graph::ResolvedProgram,
    asts: &mut [Ast],
    types: &crate::declarations::TypeEnvironment,
    globals: &[ir::Local],
    services: &Metadata,
) -> Vec<ModuleKey> {
    let mut native = graph.native.clone();
    native.import_policy = None;
    let native = normalized(
        &mut Ast {
            native,
            ..Ast::default()
        },
        &graph.source_files,
    );
    let signatures: HashMap<_, _> = asts
        .iter()
        .flat_map(|a| &a.functions)
        .map(|f| {
            (
                f.name.clone(),
                fingerprint(&(
                    f.kind,
                    f.params
                        .iter()
                        .map(|p| (&p.ty, p.by_ref, p.passing, p.arity, p.receiver))
                        .collect::<Vec<_>>(),
                    &f.result,
                    f.generator,
                    f.attributes.heat,
                    f.attributes.compilation,
                    f.attributes.inlining,
                )),
            )
        })
        .collect();
    // Owned, so the loop below can relocate each AST in place while hashing it.
    let routine_symbols: Vec<Box<str>> = std::iter::once("<main>".into())
        .chain(
            asts.iter()
                .flat_map(|a| a.functions.iter().map(|f| f.name.clone())),
        )
        .collect();
    asts.iter_mut()
        .zip(&graph.modules)
        .map(|(ast, module)| {
            let mut names = HashSet::<Box<str>>::new();
            let mut requesters = HashSet::new();
            let mut type_references = vec![];
            let mut visit = |body: &[Stmt]| {
                // Read-only: a body is inspected where it lives, never copied.
                cyallo_ast::visit::inspect_block(
                    body,
                    &mut |e| {
                        match &e.kind {
                            ExprKind::Name(n)
                            | ExprKind::Call(n, _)
                            | ExprKind::Record(n, _)
                            | ExprKind::DeferredCallable(n)
                            | ExprKind::ClosureValue(n, _, _) => {
                                names.insert(n.clone());
                            }
                            ExprKind::ExtensionField(_, _, getter) => {
                                names.insert(getter.clone());
                            }
                            _ => {}
                        }
                        if let Some(id) = graph
                            .native
                            .import_policy
                            .as_ref()
                            .and_then(|p| p.requesters.get(&e.span.start))
                        {
                            requesters.insert(*id);
                        }
                    },
                    &mut |ty, _| type_references.push(ty.clone()),
                );
            };
            visit(&ast.body);
            for f in &ast.functions {
                visit(&f.body)
            }
            let mut ts: Vec<_> = ast
                .functions
                .iter()
                .flat_map(|f| {
                    f.params
                        .iter()
                        .map(|p| p.ty.clone())
                        .chain(std::iter::once(f.result.clone()))
                })
                .collect();
            ts.extend(type_references);
            for r in &ast.records {
                ts.extend(r.fields.iter().map(|(_, t)| t.clone()));
            }
            for e in &ast.enums {
                ts.extend(e.variants.iter().filter_map(|v| v.payload.clone()));
            }
            for t in &mut ts {
                cyallo_ast::visit::visit_type(t, &mut |ty| match ty {
                    Type::Enum(e) => {
                        names.insert(e.name.clone());
                    }
                    Type::EnumRef(_, n) => {
                        names.insert(n.clone());
                    }
                    Type::Named(n)
                    | Type::RecordRef(n)
                    | Type::Record(n, _)
                    | Type::TraitObject(n) => {
                        names.insert(n.clone());
                    }
                    _ => {}
                });
            }
            loop {
                let before = names.len();
                let referenced: Vec<_> = names
                    .iter()
                    .filter_map(|n| {
                        types
                            .get(n.as_ref())
                            .cloned()
                            .or_else(|| globals.iter().find(|g| g.name == *n).map(|g| g.ty.clone()))
                    })
                    .collect();
                for mut ty in referenced {
                    cyallo_ast::visit::visit_type(&mut ty, &mut |t| match t {
                        Type::Enum(e) => {
                            names.insert(e.name.clone());
                        }
                        Type::EnumRef(_, n) => {
                            names.insert(n.clone());
                        }
                        Type::Record(n, _)
                        | Type::RecordRef(n)
                        | Type::Named(n)
                        | Type::TraitObject(n) => {
                            names.insert(n.clone());
                        }
                        _ => {}
                    });
                }
                if names.len() == before {
                    break;
                }
            }
            // Own declarations and every resolved reference use typed contracts. An
            // unrelated new module adds no dependency to this key.
            let mut dependencies: Vec<_> = names
                .iter()
                .filter_map(|name| {
                    signatures
                        .get(name)
                        .map(|s| (name.clone(), (*s)))
                        .or_else(|| {
                            globals
                                .iter()
                                .find(|g| g.name == *name)
                                .map(|g| (name.clone(), fingerprint(&(&g.ty, g.constant))))
                        })
                        .or_else(|| {
                            types
                                .get(name.as_ref())
                                .map(|t| (name.clone(), fingerprint(t)))
                        })
                })
                .collect();
            for imp in &services.implementations {
                let relevant = match &imp.receiver {
                    Type::Record(n, _) | Type::RecordRef(n) => names.contains(n),
                    _ => false,
                } || names.contains(&services.traits[imp.trait_id].name);
                if relevant {
                    dependencies.push((
                        format!("implementation:{}", fingerprint(&imp.receiver)).into(),
                        fingerprint(&(
                            &services.traits[imp.trait_id],
                            imp.methods
                                .iter()
                                .map(|&id| &routine_symbols[id])
                                .collect::<Vec<_>>(),
                        )),
                    ));
                }
            }
            dependencies.sort();
            requesters.insert(module.id.0);
            let mut requesters: Vec<_> = requesters.into_iter().collect();
            requesters.sort_by_key(|id| id.0);
            let permissions = graph.native.import_policy.as_ref().map(|p| {
                p.modules
                    .iter()
                    .filter(|m| m.trust == SourceTrust::Core)
                    .map(|m| {
                        (
                            m.path.clone(),
                            requesters
                                .iter()
                                .map(|id| {
                                    p.access
                                        .iter()
                                        .find(|a| a.referrer == *id)
                                        .is_some_and(|a| a.allowed.contains(&m.id))
                                })
                                .collect::<Vec<_>>(),
                        )
                    })
                    .collect::<Vec<_>>()
            });
            let indexed_bodies: Vec<_> = ast
                .functions
                .iter()
                .filter_map(|function| {
                    graph
                        .indexed_bodies
                        .get(&function.name)
                        .map(|body| (&function.name, body.digest()))
                })
                .collect();
            let indexed_digest = (!indexed_bodies.is_empty()).then(|| fingerprint(&indexed_bodies));
            let source = normalized(ast, &graph.source_files);
            ModuleKey {
                path: module.path.clone(),
                source: indexed_digest.map_or(source, |digest| fingerprint(&(source, digest))),
                dependencies,
                native,
                permissions: fingerprint(&permissions),
            }
        })
        .collect()
}

/// Digest of an AST with every coordinate relative to its file. The relocation is
/// applied in place and undone before returning, so the tree is never copied: the
/// span walk visits the same coordinates in the same order both times.
fn normalized(ast: &mut Ast, files: &[crate::module_graph::SourceFile]) -> crate::sdk::Digest {
    // Each coordinate is file-relative plus the identity of its file. The identity
    // is a first-use ordinal into `ids`, hashed once: hashing the path string with
    // every span made the digest input several times larger than the tree itself.
    // Equal (ordinals, ids) sequences are exactly equal (identity, span) sequences.
    let mut ids = vec![];
    let mut ordinals = vec![None; files.len()];
    let mut coordinates = vec![];
    let mut originals = vec![];
    cyallo_ast::spans::visit_spans(ast, |span| {
        originals.push(*span);
        if let Some(index) = files
            .iter()
            .position(|f| span.start >= f.span.start && span.end <= f.span.end)
        {
            let file = &files[index];
            *span = Span::new(span.start - file.span.start, span.end - file.span.start);
            let ordinal = *ordinals[index].get_or_insert_with(|| {
                ids.push(&file.id);
                ids.len() - 1
            });
            coordinates.push((ordinal, *span));
        }
    });
    let digest = fingerprint(&(&*ast, &ids, &coordinates));
    let mut originals = originals.into_iter();
    cyallo_ast::spans::visit_spans(ast, |span| {
        *span = originals.next().expect("one original per coordinate");
    });
    digest
}

#[cfg(test)]
mod tests {
    use super::*;
    fn id(source: &str) -> crate::sdk::Digest {
        fingerprint(&cyallo_parser::parse(source).unwrap())
    }
    #[test]
    fn identity_is_structural_and_deterministic() {
        let source =
            "pub record R { a: i64 }\nfn f(x: i64) -> i64 { return x + 1 }\nlet v = f(2)\n";
        assert_eq!(id(source), id(source));
        // Structure changes the identity; so does a literal.
        assert_ne!(id(source), id(&source.replace("x + 1", "x + 2")));
        assert_ne!(id(source), id(&source.replace("a: i64", "a: bool")));
        assert_ne!(id(source), id(&source.replace("f(2)", "f(3)")));
    }
    #[test]
    fn real_literals_hash_by_bit_pattern() {
        // `0.0` and `-0.0` are equal as floats but are different programs.
        assert_ne!(id("let x = 0.0\n"), id("let x = -0.0\n"));
        assert_eq!(id("let x = 1.5\n"), id("let x = 1.5\n"));
        assert_ne!(id("let x = 1.5\n"), id("let x = 2.5\n"));
    }
    #[test]
    fn identity_does_not_depend_on_map_iteration_order() {
        // Name maps are ordered, so the same names inserted in any order agree.
        let mut a = Ast::default();
        let mut b = Ast::default();
        for name in ["alpha", "beta", "gamma", "delta", "epsilon"] {
            a.source_names
                .insert(name.into(), name.to_uppercase().into());
        }
        for name in ["epsilon", "delta", "gamma", "beta", "alpha"] {
            b.source_names
                .insert(name.into(), name.to_uppercase().into());
        }
        assert_eq!(fingerprint(&a), fingerprint(&b));
        b.source_names.insert("zeta".into(), "ZETA".into());
        assert_ne!(fingerprint(&a), fingerprint(&b));
    }
    #[test]
    fn nested_structure_is_not_ambiguous() {
        // Length prefixes keep ("ab", "c") apart from ("a", "bc").
        assert_ne!(fingerprint(&("ab", "c")), fingerprint(&("a", "bc")));
        assert_ne!(
            fingerprint(&vec![vec![1u8], vec![]]),
            fingerprint(&vec![vec![], vec![1u8]])
        );
    }
    fn file(path: &str, start: u32, end: u32) -> crate::module_graph::SourceFile {
        crate::module_graph::SourceFile {
            id: crate::module_graph::SourceFileId {
                path: path.into(),
                trust: SourceTrust::User,
            },
            span: Span::new(start, end),
        }
    }
    const SOURCE: &str = "fn f(x: i64) -> i64 { return x + 1 }\nlet v = f(2)\n";
    #[test]
    fn normalizing_restores_every_coordinate() {
        let mut ast = cyallo_parser::parse(SOURCE).unwrap();
        cyallo_ast::spans::shift_ast(&mut ast, 40).unwrap();
        let before = format!("{ast:?}");
        let files = [file("a.ya", 40, 40 + SOURCE.len() as u32)];
        normalized(&mut ast, &files);
        assert_eq!(before, format!("{ast:?}"));
    }
    #[test]
    fn digest_ignores_placement_but_not_file_identity_or_content() {
        let digest = |source: &str, path: &str, offset: u32| {
            let mut ast = cyallo_parser::parse(source).unwrap();
            cyallo_ast::spans::shift_ast(&mut ast, offset).unwrap();
            normalized(
                &mut ast,
                &[file(path, offset, offset + source.len() as u32)],
            )
        };
        let base = digest(SOURCE, "a.ya", 0);
        assert_eq!(base, digest(SOURCE, "a.ya", 1000));
        assert_ne!(base, digest(SOURCE, "b.ya", 0));
        assert_ne!(base, digest(&SOURCE.replace("x + 1", "x + 2"), "a.ya", 0));
    }
    #[test]
    fn read_only_traversal_matches_the_mutating_one() {
        let source = "fn f(x: i64) -> i64 {\n let mut t = [1, 2, x]\n for i in 0..2 { t[i] = g(i) }\n if x > 1 { return h(t[0]) } else { while x < 3 { t[1] += 1 } }\n return match x { 0 => 1, _ => k(x) }\n}\n";
        let ast = cyallo_parser::parse(source).unwrap();
        let mutating = std::cell::RefCell::new(vec![]);
        let reading = std::cell::RefCell::new(vec![]);
        let mut owned = ast.functions[0].body.clone();
        cyallo_ast::visit::visit_block(
            &mut owned,
            &mut |e| {
                mutating.borrow_mut().push(format!("{:?}", e.kind));
                Ok(())
            },
            &mut |t, _| {
                mutating.borrow_mut().push(format!("{t:?}"));
                Ok(())
            },
        )
        .unwrap();
        cyallo_ast::visit::inspect_block(
            &ast.functions[0].body,
            &mut |e| reading.borrow_mut().push(format!("{:?}", e.kind)),
            &mut |t, _| reading.borrow_mut().push(format!("{t:?}")),
        );
        assert!(!reading.borrow().is_empty());
        assert_eq!(mutating, reading);
    }
}
