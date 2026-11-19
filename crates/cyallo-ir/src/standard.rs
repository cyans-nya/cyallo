//! Source assets for the standard package, without a public declaration catalog.
//!
//! A trusted provider may grant provenance to these exact source bytes. A caller
//! supplying a matching filename or module name does not acquire that provenance.

#[derive(Debug, Clone, Copy)]
pub struct Source {
    pub path: &'static str,
    pub source: &'static str,
}

/// The one package entry point; children are discovered from its declarations.
pub const ROOT: &str = "std.ya";

include!(concat!(env!("OUT_DIR"), "/standard_sources.rs"));

/// Borrows a package-relative filename; returns static embedded source storage.
pub fn source(path: &str) -> Option<&'static Source> {
    SOURCES.iter().find(|source| source.path == path)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn check_annotations(annotations: &[cyallo_ast::DeclarationAnnotation], path: &str) {
        for annotation in annotations {
            assert_eq!(annotation.name.as_ref(), "cyallo_internal", "{path}");
            assert!(
                crate::internal::from_key(&annotation.value).is_some(),
                "{path}: unknown private ABI key {}",
                annotation.value
            );
        }
    }

    /// `std/mem/manual.ya` declares module `std::mem::manual`; `std.ya` is `std`.
    fn module_path(file: &str) -> String {
        file.trim_end_matches(".ya").replace('/', "::")
    }
    fn bound(item: &str) -> bool {
        crate::bindings::NativeBindings::embedded()
            .get(item)
            .is_some()
    }
    fn check_native_function(function: &cyallo_ast::Function, path: &str, owner: Option<&str>) {
        check_annotations(&function.annotations, path);
        // A provider binding stands in for the source annotation.
        let item = match owner {
            Some(owner) => format!("{}::{owner}::{}", module_path(path), function.name),
            None => format!("{}::{}", module_path(path), function.name),
        };
        if bound(&item) {
            assert!(
                function.annotations.is_empty(),
                "{path}: {item} is both annotated and bound"
            );
            return;
        }
        assert_eq!(
            function.body_kind,
            cyallo_ast::FunctionBodyKind::Declaration,
            "{path}: native source has a fabricated body"
        );
        assert!(
            !function.annotations.is_empty(),
            "{path}: native callable lacks its semantic binding"
        );
    }

    #[test]
    fn every_bundled_asset_is_actual_cyallo_source() {
        assert!(source(ROOT).is_some());
        let mut paths = std::collections::HashSet::new();
        for source in SOURCES {
            assert!(
                paths.insert(source.path),
                "duplicate source asset: {}",
                source.path
            );
            let ast = cyallo_parser::parse_modern(source.source, false)
                .unwrap_or_else(|error| panic!("{}: {error}", source.path));
            for module in &ast.modules {
                check_annotations(&module.annotations, source.path);
            }
            for record in &ast.records {
                check_annotations(&record.annotations, source.path);
            }
            // Every binding's key is a known private ABI key.
            for (item, key) in crate::bindings::NativeBindings::embedded().iter() {
                assert!(
                    crate::internal::from_key(key).is_some(),
                    "{item}: unknown key {key}"
                );
            }
            for trait_ in &ast.traits {
                check_annotations(&trait_.annotations, source.path);
            }
            for function in &ast.functions {
                check_native_function(function, source.path, None);
            }
            for implementation in &ast.inherent_implementations {
                // Same addressing as the linker: a nominal receiver by name, any other
                // receiver (`[]T`, `rc T`) by its spelling.
                let owner = match &implementation.receiver {
                    cyallo_ast::Type::Named(name) | cyallo_ast::Type::Applied(name, _) => {
                        name.to_string()
                    }
                    other => other.to_string(),
                };
                for function in &implementation.methods {
                    check_native_function(&function.function, source.path, Some(&owner));
                }
            }
        }
    }
}
