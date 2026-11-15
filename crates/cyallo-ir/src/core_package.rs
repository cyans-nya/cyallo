//! Explicit host-owned core source provider and its archive coordinate layout.
//!
//! Filenames and public module names carry no trust. Choosing this provider is
//! the host's trust decision; the linker must keep its ASTs separate from user
//! source until it assigns loader-issued provenance.

use cyallo_ast::{Ast, Diagnostic, Span};
use std::{
    collections::{HashMap, TryReserveError},
    fmt,
};

const HEADER_PREFIX: &str = "// module: ";

#[derive(Debug, Clone, Copy)]
pub struct SourceText<'a> {
    pub path: &'a str,
    pub source: &'a str,
}

#[derive(Debug, Clone)]
pub struct CorePackage<'a> {
    root: &'a str,
    sources: Vec<SourceText<'a>>,
}

#[derive(Debug, Clone)]
pub struct CoreSourceRange {
    pub path: Box<str>,
    /// Source text only, excluding the archive header and appended newline.
    pub span: Span,
}

#[derive(Debug, Clone)]
pub struct CoreArchive {
    /// First module header, after the optional separator newline.
    pub base: u32,
    pub ranges: Vec<CoreSourceRange>,
}

#[derive(Debug, Clone)]
pub struct ParsedCore {
    pub asts: Vec<Ast>,
    pub paths: Vec<String>,
    pub entry: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ArchivePart {
    Header,
    Source,
    Terminator,
}
impl fmt::Display for ArchivePart {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Header => "module header",
            Self::Source => "source text",
            Self::Terminator => "source newline",
        })
    }
}

#[derive(Debug)]
pub enum CoreSourceError {
    EmptyRoot,
    EmptyPath {
        index: usize,
    },
    MissingRoot {
        root: Box<str>,
    },
    DuplicatePath {
        path: Box<str>,
        first: usize,
        second: usize,
    },
    ExistingArchiveTooLarge {
        length: usize,
    },
    SeparatorOverflow {
        offset: u32,
    },
    HeaderLengthOverflow {
        path: Box<str>,
    },
    LengthOverflow {
        path: Box<str>,
        part: ArchivePart,
        offset: u32,
        length: usize,
    },
    Allocation(TryReserveError),
}
impl fmt::Display for CoreSourceError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::EmptyRoot => f.write_str("core source package requires an entry path"),
            Self::EmptyPath { index } => write!(f, "core source {} has an empty path", index + 1),
            Self::MissingRoot { root } => {
                write!(f, "core package has no source for entry `{root}`")
            }
            Self::DuplicatePath {
                path,
                first,
                second,
            } => write!(
                f,
                "core source path `{path}` is supplied at indices {first} and {second}"
            ),
            Self::ExistingArchiveTooLarge { length } => write!(
                f,
                "existing source archive length {length} exceeds the 4 GiB span limit"
            ),
            Self::SeparatorOverflow { offset } => write!(
                f,
                "separator after source offset {offset} exceeds the 4 GiB span limit"
            ),
            Self::HeaderLengthOverflow { path } => write!(
                f,
                "core source header for `{path}` exceeds the supported length"
            ),
            Self::LengthOverflow {
                path,
                part,
                offset,
                length,
            } => write!(
                f,
                "{part} for core source `{path}` at offset {offset} with length {length} exceeds the 4 GiB span limit"
            ),
            Self::Allocation(error) => {
                write!(f, "cannot reserve core source archive storage: {error}")
            }
        }
    }
}
impl std::error::Error for CoreSourceError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Allocation(error) => Some(error),
            _ => None,
        }
    }
}

/// Owns diagnostic-code mapping for package validation and archive layout errors.
pub fn invalid_core_source(error: CoreSourceError) -> Diagnostic {
    let span = match &error {
        CoreSourceError::SeparatorOverflow { offset }
        | CoreSourceError::LengthOverflow { offset, .. } => Span::new(*offset, *offset),
        _ => Span::default(),
    };
    Diagnostic::new("E176", error.to_string(), span)
}

struct Layout {
    ranges: Vec<CoreSourceRange>,
    end: u32,
}

impl CorePackage<'static> {
    /// Uses the exact source bytes furnished by the build's embedded provider.
    /// The generated asset bundle contains its root once and has unique paths.
    pub fn embedded() -> Self {
        Self {
            root: crate::standard::ROOT,
            sources: crate::standard::SOURCES
                .iter()
                .map(|source| SourceText {
                    path: source.path,
                    source: source.source,
                })
                .collect(),
        }
    }
}

impl<'a> CorePackage<'a> {
    /// Explicit host trust decision. Copies source descriptors into an owned
    /// vector; borrows path/source bytes and root for the package's lifetime.
    pub fn from_trusted_sources(
        root: &'a str,
        sources: impl IntoIterator<Item = SourceText<'a>>,
    ) -> Result<Self, CoreSourceError> {
        if root.is_empty() {
            return Err(CoreSourceError::EmptyRoot);
        }
        let sources: Vec<_> = sources.into_iter().collect();
        let mut paths = HashMap::new();
        for (index, source) in sources.iter().enumerate() {
            if source.path.is_empty() {
                return Err(CoreSourceError::EmptyPath { index });
            }
            if let Some(first) = paths.insert(source.path, index) {
                return Err(CoreSourceError::DuplicatePath {
                    path: source.path.into(),
                    first,
                    second: index,
                });
            }
        }
        if !paths.contains_key(root) {
            return Err(CoreSourceError::MissingRoot { root: root.into() });
        }
        Ok(Self { root, sources })
    }

    pub fn root(&self) -> &'a str {
        self.root
    }
    pub fn sources(&self) -> &[SourceText<'a>] {
        &self.sources
    }

    /// Appends a complete archive after preflighting all u32 coordinates and
    /// reserving its storage. On error the caller's text remains unchanged.
    pub fn append_to(&self, archive: &mut String) -> Result<CoreArchive, CoreSourceError> {
        let initial =
            u32::try_from(archive.len()).map_err(|_| CoreSourceError::ExistingArchiveTooLarge {
                length: archive.len(),
            })?;
        let separator = !archive.is_empty() && !archive.ends_with('\n');
        let base = if separator {
            initial
                .checked_add(1)
                .ok_or(CoreSourceError::SeparatorOverflow { offset: initial })?
        } else {
            initial
        };
        let layout = self.layout(base)?;
        archive
            .try_reserve((layout.end - initial) as usize)
            .map_err(CoreSourceError::Allocation)?;
        if separator {
            archive.push('\n');
        }
        for source in &self.sources {
            archive.push_str(HEADER_PREFIX);
            archive.push_str(source.path);
            archive.push('\n');
            archive.push_str(source.source);
            archive.push('\n');
        }
        debug_assert_eq!(archive.len(), layout.end as usize);
        Ok(CoreArchive {
            base,
            ranges: layout.ranges,
        })
    }

    /// Parses only this trusted provider's sources, shifting ASTs and parse
    /// diagnostics into the layout produced by append_to at `base`.
    pub fn parse(&self, base: u32) -> Result<ParsedCore, Diagnostic> {
        let layout = self.layout(base).map_err(invalid_core_source)?;
        let mut asts = Vec::with_capacity(self.sources.len());
        let mut paths = Vec::with_capacity(self.sources.len());
        let mut entry = None;
        for (index, (source, range)) in self.sources.iter().zip(&layout.ranges).enumerate() {
            let mut ast = match cyallo_parser::parse_modern(source.source, false) {
                Ok(ast) => ast,
                Err(mut diagnostic) => {
                    cyallo_ast::spans::shift_diagnostic(&mut diagnostic, range.span.start)?;
                    return Err(diagnostic);
                }
            };
            cyallo_ast::spans::shift_ast(&mut ast, range.span.start)?;
            asts.push(ast);
            paths.push(source.path.to_owned());
            if source.path == self.root {
                entry = Some(index);
            }
        }
        let entry = entry.ok_or_else(|| {
            invalid_core_source(CoreSourceError::MissingRoot {
                root: self.root.into(),
            })
        })?;
        Ok(ParsedCore { asts, paths, entry })
    }

    fn layout(&self, base: u32) -> Result<Layout, CoreSourceError> {
        let mut offset = base;
        let mut ranges = Vec::with_capacity(self.sources.len());
        for source in &self.sources {
            let header = HEADER_PREFIX
                .len()
                .checked_add(source.path.len())
                .and_then(|length| length.checked_add(1))
                .ok_or_else(|| CoreSourceError::HeaderLengthOverflow {
                    path: source.path.into(),
                })?;
            offset = advance(offset, header, source.path, ArchivePart::Header)?;
            let end = advance(
                offset,
                source.source.len(),
                source.path,
                ArchivePart::Source,
            )?;
            ranges.push(CoreSourceRange {
                path: source.path.into(),
                span: Span::new(offset, end),
            });
            offset = advance(end, 1, source.path, ArchivePart::Terminator)?;
        }
        Ok(Layout {
            ranges,
            end: offset,
        })
    }
}

fn advance(
    offset: u32,
    length: usize,
    path: &str,
    part: ArchivePart,
) -> Result<u32, CoreSourceError> {
    u32::try_from(length)
        .ok()
        .and_then(|length| offset.checked_add(length))
        .ok_or_else(|| CoreSourceError::LengthOverflow {
            path: path.into(),
            part,
            offset,
            length,
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture() -> CorePackage<'static> {
        CorePackage::from_trusted_sources(
            "foundation.ya",
            [
                SourceText {
                    path: "foundation/task.ya",
                    source: "#[cyallo_internal(\"task_struct\")]\npub record Handle<T> {}\n",
                },
                SourceText {
                    path: "foundation.ya",
                    source: "pub mod task;\n",
                },
            ],
        )
        .unwrap()
    }

    #[test]
    fn archive_ranges_and_parsed_spans_share_exact_byte_offsets() {
        let package = fixture();
        let prefix = "let user = \"水\"";
        let mut archive = prefix.to_owned();
        let core = package.append_to(&mut archive).unwrap();
        assert_eq!(core.base, prefix.len() as u32 + 1);
        assert!(archive.starts_with(&format!("{prefix}\n// module: foundation/task.ya\n")));
        for (range, source) in core.ranges.iter().zip(package.sources()) {
            assert_eq!(
                &archive[range.span.start as usize..range.span.end as usize],
                source.source
            );
            assert_eq!(range.path.as_ref(), source.path);
        }
        let parsed = package.parse(core.base).unwrap();
        assert_eq!(parsed.entry, 1);
        assert_eq!(parsed.paths[parsed.entry], package.root());
        let annotation = &parsed.asts[0].records[0].annotations[0];
        assert_eq!(
            &archive[annotation.span.start as usize..annotation.span.end as usize],
            "#[cyallo_internal(\"task_struct\")]"
        );
        let span = parsed.asts[parsed.entry].modules[0].span;
        assert!(archive[span.start as usize..span.end as usize].contains("pub mod task"));
    }

    #[test]
    fn separator_is_added_only_when_the_existing_archive_needs_one() {
        let package = fixture();
        let mut empty = String::new();
        assert_eq!(package.append_to(&mut empty).unwrap().base, 0);
        assert!(empty.starts_with(HEADER_PREFIX));
        let mut terminated = "user\n".to_owned();
        assert_eq!(package.append_to(&mut terminated).unwrap().base, 5);
        assert!(terminated.starts_with("user\n// module:"));
    }

    #[test]
    fn syntax_errors_and_related_spans_point_inside_the_core_archive() {
        let source = "#[hint(hot)]\n#[hint(cold)]\nproc broken() {}\n";
        let package = CorePackage::from_trusted_sources(
            "alternate.ya",
            [SourceText {
                path: "alternate.ya",
                source,
            }],
        )
        .unwrap();
        let mut archive = "user code\n".to_owned();
        let core = package.append_to(&mut archive).unwrap();
        let diagnostic = package.parse(core.base).unwrap_err();
        let range = core.ranges[0].span;
        assert!(diagnostic.span.start >= range.start && diagnostic.span.end <= range.end);
        assert_eq!(
            &archive[diagnostic.span.start as usize..diagnostic.span.end as usize],
            "#[hint(cold)]"
        );
        assert_eq!(diagnostic.related.len(), 1);
        let related = diagnostic.related[0].0;
        assert_eq!(
            &archive[related.start as usize..related.end as usize],
            "#[hint(hot)]"
        );
    }

    #[test]
    fn package_validation_keeps_distinct_typed_causes() {
        assert!(matches!(
            CorePackage::from_trusted_sources("", []),
            Err(CoreSourceError::EmptyRoot)
        ));
        assert!(matches!(
            CorePackage::from_trusted_sources(
                "root",
                [SourceText {
                    path: "",
                    source: ""
                }]
            ),
            Err(CoreSourceError::EmptyPath { index: 0 })
        ));
        assert!(matches!(
            CorePackage::from_trusted_sources("root", []),
            Err(CoreSourceError::MissingRoot { .. })
        ));
        assert!(matches!(
            CorePackage::from_trusted_sources(
                "root",
                [
                    SourceText {
                        path: "root",
                        source: ""
                    },
                    SourceText {
                        path: "root",
                        source: ""
                    }
                ]
            ),
            Err(CoreSourceError::DuplicatePath {
                first: 0,
                second: 1,
                ..
            })
        ));
    }

    #[test]
    fn layout_overflow_is_checked_without_allocating_a_large_archive() {
        let package = fixture();
        assert!(matches!(
            package.layout(u32::MAX),
            Err(CoreSourceError::LengthOverflow {
                part: ArchivePart::Header,
                ..
            })
        ));
        let diagnostic = package.parse(u32::MAX).unwrap_err();
        assert_eq!(diagnostic.code, "E176");
        assert_eq!(diagnostic.span, Span::new(u32::MAX, u32::MAX));
    }

    #[test]
    fn independent_providers_can_reuse_paths_with_different_contents() {
        let a = CorePackage::from_trusted_sources(
            "core.ya",
            [SourceText {
                path: "core.ya",
                source: "pub record First {}\n",
            }],
        )
        .unwrap();
        let b = CorePackage::from_trusted_sources(
            "core.ya",
            [SourceText {
                path: "core.ya",
                source: "pub record Second {}\n",
            }],
        )
        .unwrap();
        let mut left = "prefix\n".to_owned();
        let mut right = String::new();
        let left_range = a.append_to(&mut left).unwrap();
        let right_range = b.append_to(&mut right).unwrap();
        assert_eq!(
            a.parse(left_range.base).unwrap().asts[0].records[0]
                .name
                .as_ref(),
            "First"
        );
        assert_eq!(
            b.parse(right_range.base).unwrap().asts[0].records[0]
                .name
                .as_ref(),
            "Second"
        );
        assert!(!left.contains("Second"));
        assert!(!right.contains("First"));
    }

    #[test]
    fn embedded_provider_parses_all_real_source_assets_at_a_nonzero_base() {
        let package = CorePackage::embedded();
        let mut archive = "user\n".to_owned();
        let core = package.append_to(&mut archive).unwrap();
        let parsed = package.parse(core.base).unwrap();
        assert_eq!(parsed.asts.len(), package.sources().len());
        assert_eq!(parsed.paths[parsed.entry], package.root());
        for (ast, range) in parsed.asts.iter().zip(&core.ranges) {
            for record in &ast.records {
                assert!(record.span.start >= range.span.start);
                assert!(record.span.end <= range.span.end);
            }
            for implementation in &ast.inherent_implementations {
                assert!(implementation.span.start >= range.span.start);
                for method in &implementation.methods {
                    assert!(method.function.span.start >= range.span.start);
                    assert!(method.function.span.end <= range.span.end);
                }
            }
        }
    }
}
