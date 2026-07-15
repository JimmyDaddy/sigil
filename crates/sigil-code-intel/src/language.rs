use std::{fs, path::Path};

use anyhow::{Context, Result};
use tree_sitter::{Node, Parser};

use crate::service::{CodeDiagnostic, CodeRange, CodeSymbol};
use crate::workspace::workspace_relative_path;

pub fn rust_document_symbols(
    workspace_root: &Path,
    path: &Path,
    query: Option<&str>,
    max_results: usize,
) -> Result<Vec<CodeSymbol>> {
    let source =
        fs::read_to_string(path).with_context(|| format!("failed to read {}", path.display()))?;
    rust_document_symbols_from_source(workspace_root, path, &source, query, max_results)
}

/// Extracts Rust document symbols from caller-bounded UTF-8 source.
///
/// RepoMap callers use this entrypoint so they do not reopen or fully read a file after applying
/// their request-local byte cap. Interactive code-intelligence requests continue to use
/// [`rust_document_symbols`].
pub(crate) fn rust_document_symbols_from_source(
    workspace_root: &Path,
    path: &Path,
    source: &str,
    query: Option<&str>,
    max_results: usize,
) -> Result<Vec<CodeSymbol>> {
    let mut parser = rust_parser()?;
    let Some(tree) = parser.parse(source, None) else {
        return Ok(Vec::new());
    };
    let query = query.map(str::to_ascii_lowercase);
    let mut symbols = Vec::new();
    collect_rust_symbols(
        tree.root_node(),
        source.as_bytes(),
        workspace_root,
        path,
        query.as_deref(),
        max_results,
        &mut symbols,
    );
    Ok(symbols)
}

pub fn rust_syntax_diagnostics(workspace_root: &Path, path: &Path) -> Result<Vec<CodeDiagnostic>> {
    let source =
        fs::read_to_string(path).with_context(|| format!("failed to read {}", path.display()))?;
    let mut parser = rust_parser()?;
    let Some(tree) = parser.parse(&source, None) else {
        return Ok(Vec::new());
    };
    let mut diagnostics = Vec::new();
    collect_error_nodes(tree.root_node(), workspace_root, path, &mut diagnostics);
    Ok(diagnostics)
}

fn rust_parser() -> Result<Parser> {
    let mut parser = Parser::new();
    let language = tree_sitter_rust::LANGUAGE;
    parser
        .set_language(&language.into())
        .context("failed to load tree-sitter rust grammar")?;
    Ok(parser)
}

fn collect_rust_symbols(
    node: Node<'_>,
    source: &[u8],
    workspace_root: &Path,
    path: &Path,
    query: Option<&str>,
    max_results: usize,
    symbols: &mut Vec<CodeSymbol>,
) {
    if symbols.len() >= max_results {
        return;
    }
    if let Some((name, kind)) = rust_symbol_name_and_kind(node, source) {
        let matches_query = query
            .map(|needle| name.to_ascii_lowercase().contains(needle))
            .unwrap_or(true);
        if matches_query {
            symbols.push(CodeSymbol {
                name,
                kind,
                path: workspace_relative_path(workspace_root, path),
                range: range_from_node(node),
                container_name: None,
            });
        }
    }

    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        collect_rust_symbols(
            child,
            source,
            workspace_root,
            path,
            query,
            max_results,
            symbols,
        );
        if symbols.len() >= max_results {
            return;
        }
    }
}

fn rust_symbol_name_and_kind(node: Node<'_>, source: &[u8]) -> Option<(String, String)> {
    let kind = match node.kind() {
        "function_item" => "function",
        "struct_item" => "struct",
        "enum_item" => "enum",
        "trait_item" => "trait",
        "type_item" => "type",
        "const_item" => "const",
        "static_item" => "static",
        "mod_item" => "module",
        "impl_item" => "impl",
        _ => return None,
    };
    let name_node = if node.kind() == "impl_item" {
        node.child_by_field_name("trait")
            .or_else(|| node.child_by_field_name("type"))
    } else {
        node.child_by_field_name("name")
    }?;
    let name = name_node.utf8_text(source).ok()?.trim().to_owned();
    (!name.is_empty()).then_some((name, kind.to_owned()))
}

fn collect_error_nodes(
    node: Node<'_>,
    workspace_root: &Path,
    path: &Path,
    diagnostics: &mut Vec<CodeDiagnostic>,
) {
    if node.is_error() || node.is_missing() {
        diagnostics.push(CodeDiagnostic {
            path: workspace_relative_path(workspace_root, path),
            range: range_from_node(node),
            severity: "error".to_owned(),
            message: if node.is_missing() {
                format!("missing {}", node.kind())
            } else {
                "syntax error".to_owned()
            },
            source: Some("tree-sitter-rust".to_owned()),
        });
    }
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        collect_error_nodes(child, workspace_root, path, diagnostics);
    }
}

fn range_from_node(node: Node<'_>) -> CodeRange {
    let start = node.start_position();
    let end = node.end_position();
    CodeRange {
        start_line: start.row as u64 + 1,
        start_character: start.column as u64,
        end_line: end.row as u64 + 1,
        end_character: end.column as u64,
    }
}

#[cfg(test)]
#[path = "tests/language_tests.rs"]
mod tests;
