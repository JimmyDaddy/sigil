use std::path::Path;

use anyhow::{Context, Result};
use tree_sitter::{Language, Parser, Query, QueryCursor, StreamingIterator};

use crate::{
    context::RepoSymbolKind, language::rust_document_symbols_from_source, service::CodeRange,
};

const MAX_TAG_NAME_BYTES: usize = 256;

/// Source languages supported by the request-local RepoMap fallback.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum RepoLanguage {
    Rust,
    Python,
    JavaScript,
    TypeScript,
    Tsx,
    Go,
}

impl RepoLanguage {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Rust => "rust",
            Self::Python => "python",
            Self::JavaScript => "javascript",
            Self::TypeScript => "typescript",
            Self::Tsx => "tsx",
            Self::Go => "go",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RepoDefinitionTag {
    pub name: String,
    pub kind: RepoSymbolKind,
    pub range: CodeRange,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RepoReferenceTag {
    pub name: String,
    pub range: CodeRange,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct RepoTagSet {
    pub definitions: Vec<RepoDefinitionTag>,
    pub references: Vec<RepoReferenceTag>,
}

#[derive(Clone, Copy)]
struct RepoLanguageAdapter {
    language: RepoLanguage,
    extensions: &'static [&'static str],
    parser_language: fn() -> Language,
    tags_queries: &'static [&'static str],
    preserve_rust_definitions: bool,
}

const REPO_LANGUAGE_ADAPTERS: &[RepoLanguageAdapter] = &[
    RepoLanguageAdapter {
        language: RepoLanguage::Rust,
        extensions: &["rs"],
        parser_language: rust_language,
        tags_queries: &[tree_sitter_rust::TAGS_QUERY],
        preserve_rust_definitions: true,
    },
    RepoLanguageAdapter {
        language: RepoLanguage::Python,
        extensions: &["py", "pyi"],
        parser_language: python_language,
        tags_queries: &[tree_sitter_python::TAGS_QUERY],
        preserve_rust_definitions: false,
    },
    RepoLanguageAdapter {
        language: RepoLanguage::JavaScript,
        extensions: &["js", "jsx", "mjs", "cjs"],
        parser_language: javascript_language,
        tags_queries: &[tree_sitter_javascript::TAGS_QUERY],
        preserve_rust_definitions: false,
    },
    RepoLanguageAdapter {
        language: RepoLanguage::TypeScript,
        extensions: &["ts", "mts", "cts"],
        parser_language: typescript_language,
        tags_queries: &[
            tree_sitter_javascript::TAGS_QUERY,
            tree_sitter_typescript::TAGS_QUERY,
        ],
        preserve_rust_definitions: false,
    },
    RepoLanguageAdapter {
        language: RepoLanguage::Tsx,
        extensions: &["tsx"],
        parser_language: tsx_language,
        tags_queries: &[
            tree_sitter_javascript::TAGS_QUERY,
            tree_sitter_typescript::TAGS_QUERY,
        ],
        preserve_rust_definitions: false,
    },
    RepoLanguageAdapter {
        language: RepoLanguage::Go,
        extensions: &["go"],
        parser_language: go_language,
        tags_queries: &[tree_sitter_go::TAGS_QUERY],
        preserve_rust_definitions: false,
    },
];

pub(crate) fn repo_language_for_path(path: &Path) -> Option<RepoLanguage> {
    let extension = path.extension()?.to_str()?.to_ascii_lowercase();
    REPO_LANGUAGE_ADAPTERS
        .iter()
        .find(|adapter| adapter.extensions.contains(&extension.as_str()))
        .map(|adapter| adapter.language)
}

pub(crate) fn extract_repo_tags(
    language: RepoLanguage,
    workspace_root: &Path,
    path: &Path,
    source: &str,
    max_definitions: usize,
    max_references: usize,
) -> Result<RepoTagSet> {
    let adapter = language_adapter(language);
    let parser_language = (adapter.parser_language)();
    let mut parser = Parser::new();
    parser
        .set_language(&parser_language)
        .with_context(|| format!("failed to load {} grammar", language.as_str()))?;
    let Some(tree) = parser.parse(source, None) else {
        return Ok(RepoTagSet::default());
    };

    let mut tags = RepoTagSet::default();
    if adapter.preserve_rust_definitions {
        for symbol in
            rust_document_symbols_from_source(workspace_root, path, source, None, max_definitions)?
        {
            tags.definitions.push(RepoDefinitionTag {
                name: symbol.name,
                kind: repo_symbol_kind(&symbol.kind),
                range: symbol.range,
            });
        }
    }

    let source_bytes = source.as_bytes();
    'queries: for tags_query in adapter.tags_queries {
        let query = Query::new(&parser_language, tags_query)
            .with_context(|| format!("failed to compile {} tags query", language.as_str()))?;
        let capture_names = query.capture_names();
        let mut cursor = QueryCursor::new();
        let mut matches = cursor.matches(&query, tree.root_node(), source_bytes);
        while let Some(query_match) = matches.next() {
            let Some(name_capture) = query_match.captures.iter().find(|capture| {
                capture_names
                    .get(capture.index as usize)
                    .is_some_and(|name| *name == "name")
            }) else {
                continue;
            };
            let Ok(name) = name_capture.node.utf8_text(source_bytes) else {
                continue;
            };
            let name = name.trim();
            if name.is_empty() || name.len() > MAX_TAG_NAME_BYTES {
                continue;
            }

            for capture in query_match.captures {
                let Some(capture_name) = capture_names.get(capture.index as usize).copied() else {
                    continue;
                };
                if let Some(definition_kind) = capture_name.strip_prefix("definition.") {
                    if !adapter.preserve_rust_definitions
                        && tags.definitions.len() < max_definitions
                    {
                        tags.definitions.push(RepoDefinitionTag {
                            name: name.to_owned(),
                            kind: repo_symbol_kind(definition_kind),
                            range: code_range_from_node(capture.node),
                        });
                    }
                } else if capture_name.starts_with("reference.")
                    && tags.references.len() < max_references
                {
                    tags.references.push(RepoReferenceTag {
                        name: name.to_owned(),
                        range: code_range_from_node(capture.node),
                    });
                }
            }
            if tags.definitions.len() >= max_definitions && tags.references.len() >= max_references
            {
                break 'queries;
            }
        }
    }

    tags.definitions.sort_by(|left, right| {
        code_range_key(&left.range)
            .cmp(&code_range_key(&right.range))
            .then_with(|| left.name.cmp(&right.name))
            .then_with(|| left.kind.cmp(&right.kind))
    });
    tags.definitions.dedup();
    tags.definitions.truncate(max_definitions);
    tags.references.sort_by(|left, right| {
        code_range_key(&left.range)
            .cmp(&code_range_key(&right.range))
            .then_with(|| left.name.cmp(&right.name))
    });
    tags.references.dedup();
    tags.references.truncate(max_references);
    Ok(tags)
}

fn language_adapter(language: RepoLanguage) -> &'static RepoLanguageAdapter {
    REPO_LANGUAGE_ADAPTERS
        .iter()
        .find(|adapter| adapter.language == language)
        .expect("every RepoLanguage has a static adapter")
}

fn repo_symbol_kind(kind: &str) -> RepoSymbolKind {
    match kind {
        "function" => RepoSymbolKind::Function,
        "method" => RepoSymbolKind::Method,
        "class" => RepoSymbolKind::Class,
        "interface" => RepoSymbolKind::Interface,
        "struct" => RepoSymbolKind::Struct,
        "enum" => RepoSymbolKind::Enum,
        "trait" => RepoSymbolKind::Trait,
        "type" => RepoSymbolKind::Type,
        "constant" | "const" => RepoSymbolKind::Const,
        "static" => RepoSymbolKind::Static,
        "module" => RepoSymbolKind::Module,
        "impl" => RepoSymbolKind::Impl,
        "variable" => RepoSymbolKind::Variable,
        _ => RepoSymbolKind::Other,
    }
}

fn code_range_from_node(node: tree_sitter::Node<'_>) -> CodeRange {
    let start = node.start_position();
    let end = node.end_position();
    CodeRange {
        start_line: start.row as u64 + 1,
        start_character: start.column as u64,
        end_line: end.row as u64 + 1,
        end_character: end.column as u64,
    }
}

fn code_range_key(range: &CodeRange) -> (u64, u64, u64, u64) {
    (
        range.start_line,
        range.start_character,
        range.end_line,
        range.end_character,
    )
}

fn rust_language() -> Language {
    tree_sitter_rust::LANGUAGE.into()
}

fn python_language() -> Language {
    tree_sitter_python::LANGUAGE.into()
}

fn javascript_language() -> Language {
    tree_sitter_javascript::LANGUAGE.into()
}

fn typescript_language() -> Language {
    tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into()
}

fn tsx_language() -> Language {
    tree_sitter_typescript::LANGUAGE_TSX.into()
}

fn go_language() -> Language {
    tree_sitter_go::LANGUAGE.into()
}

#[cfg(test)]
#[path = "tests/repo_language_tests.rs"]
mod tests;
