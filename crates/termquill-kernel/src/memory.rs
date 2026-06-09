use std::{
    collections::BTreeSet,
    fs,
    path::{Path, PathBuf},
};

use anyhow::{Context, Result, anyhow, bail};
use sha2::{Digest, Sha256};

use crate::{MemoryConfig, ModelMessage, PrefixSnapshot};

const ROOT_MEMORY_FILENAMES: &[&str] = &[
    "TERMQUILL.md",
    "AGENTS.md",
    "CLAUDE.md",
    "TERMQUILL.local.md",
];
const BASE_SYSTEM_PROMPT: &str = "You are Termquill, a TUI-first Rust coding agent working inside the user's workspace. Prefer inspecting the workspace before edits, keep changes auditable, and follow loaded workspace instructions.";

/// Loaded workspace memory summary for UI and request materialization.
#[derive(Debug, Clone, Default)]
pub struct MemoryLoadReport {
    pub enabled: bool,
    pub document_count: usize,
    pub fingerprint: String,
}

#[derive(Debug, Clone)]
pub(crate) struct MemoryDocument {
    pub relative_path: String,
    pub content: String,
}

#[derive(Debug, Clone)]
pub(crate) struct MaterializedMemory {
    pub messages: Vec<ModelMessage>,
    pub report: MemoryLoadReport,
}

/// Loads the current workspace memory summary without building a provider request.
///
/// # Errors
///
/// Returns an error when one declared memory file or `@path` import cannot be loaded safely.
pub fn inspect_memory_documents(
    workspace_root: &Path,
    config: &MemoryConfig,
) -> Result<MemoryLoadReport> {
    let docs = load_memory_documents(workspace_root, config)?;
    Ok(build_memory_report(config.enabled, &docs))
}

pub(crate) fn materialize_memory(
    workspace_root: &Path,
    config: &MemoryConfig,
) -> Result<MaterializedMemory> {
    let docs = load_memory_documents(workspace_root, config)?;
    let report = build_memory_report(config.enabled, &docs);
    let mut messages = vec![ModelMessage {
        id: "system:base".to_owned(),
        role: crate::MessageRole::System,
        content: Some(BASE_SYSTEM_PROMPT.to_owned()),
        tool_calls: Vec::new(),
        tool_call_id: None,
    }];

    for document in &docs {
        if document.content.trim().is_empty() {
            continue;
        }
        messages.push(ModelMessage {
            id: stable_memory_message_id(&document.relative_path, &document.content),
            role: crate::MessageRole::System,
            content: Some(format!(
                "# Memory: {}\n\n{}",
                document.relative_path, document.content
            )),
            tool_calls: Vec::new(),
            tool_call_id: None,
        });
    }

    Ok(MaterializedMemory { messages, report })
}

pub(crate) fn apply_memory_report(snapshot: &mut PrefixSnapshot, report: &MemoryLoadReport) {
    snapshot.memory_fingerprint = report.fingerprint.clone();
}

fn build_memory_report(enabled: bool, docs: &[MemoryDocument]) -> MemoryLoadReport {
    let mut digest_input = String::new();
    for document in docs {
        digest_input.push_str(&document.relative_path);
        digest_input.push('\n');
        digest_input.push_str(&document.content);
        digest_input.push_str("\n---\n");
    }

    MemoryLoadReport {
        enabled,
        document_count: docs.len(),
        fingerprint: if docs.is_empty() {
            "none".to_owned()
        } else {
            format!("{:x}", Sha256::digest(digest_input.as_bytes()))
        },
    }
}

fn load_memory_documents(
    workspace_root: &Path,
    config: &MemoryConfig,
) -> Result<Vec<MemoryDocument>> {
    if !config.enabled {
        return Ok(Vec::new());
    }

    let canonical_root = fs::canonicalize(workspace_root)
        .with_context(|| format!("failed to canonicalize {}", workspace_root.display()))?;
    let mut documents = Vec::new();
    let mut visited = BTreeSet::new();
    let mut stack = Vec::new();

    for filename in ROOT_MEMORY_FILENAMES {
        let path = workspace_root.join(filename);
        if path.is_file() {
            load_memory_file(
                &path,
                &canonical_root,
                &mut documents,
                &mut visited,
                &mut stack,
            )?;
        }
    }

    Ok(documents)
}

fn load_memory_file(
    path: &Path,
    canonical_root: &Path,
    documents: &mut Vec<MemoryDocument>,
    visited: &mut BTreeSet<PathBuf>,
    stack: &mut Vec<PathBuf>,
) -> Result<()> {
    let canonical_path = fs::canonicalize(path)
        .with_context(|| format!("failed to resolve memory file {}", path.display()))?;
    if !canonical_path.starts_with(canonical_root) {
        bail!(
            "memory file {} escapes workspace root {}",
            canonical_path.display(),
            canonical_root.display()
        );
    }
    if let Some(index) = stack.iter().position(|entry| entry == &canonical_path) {
        let cycle = stack[index..]
            .iter()
            .chain(std::iter::once(&canonical_path))
            .map(|entry| entry.display().to_string())
            .collect::<Vec<_>>()
            .join(" -> ");
        bail!("memory import cycle detected: {cycle}");
    }
    if visited.contains(&canonical_path) {
        return Ok(());
    }

    stack.push(canonical_path.clone());
    let raw = fs::read_to_string(&canonical_path)
        .with_context(|| format!("failed to read memory file {}", canonical_path.display()))?;
    let (content, imports) = parse_memory_file(&raw);
    let relative_path = canonical_path
        .strip_prefix(canonical_root)
        .map_err(|error| anyhow!("failed to relativize {}: {error}", canonical_path.display()))?
        .to_string_lossy()
        .to_string();
    documents.push(MemoryDocument {
        relative_path,
        content,
    });
    visited.insert(canonical_path.clone());

    let parent = canonical_path
        .parent()
        .ok_or_else(|| anyhow!("memory file {} has no parent", canonical_path.display()))?;
    for import in imports {
        let imported = resolve_import_path(parent, &import)?;
        load_memory_file(&imported, canonical_root, documents, visited, stack)?;
    }

    stack.pop();
    Ok(())
}

fn parse_memory_file(raw: &str) -> (String, Vec<String>) {
    let mut content_lines = Vec::new();
    let mut imports = Vec::new();

    for line in raw.lines() {
        let trimmed = line.trim();
        if let Some(path) = trimmed.strip_prefix('@') {
            let path = path.trim();
            if !path.is_empty() {
                imports.push(path.to_owned());
                continue;
            }
        }
        content_lines.push(line);
    }

    (content_lines.join("\n"), imports)
}

fn resolve_import_path(base_dir: &Path, import: &str) -> Result<PathBuf> {
    let import_path = Path::new(import);
    if import_path.is_absolute() {
        bail!("memory import must be relative: {import}");
    }
    Ok(base_dir.join(import_path))
}

fn stable_memory_message_id(relative_path: &str, content: &str) -> String {
    let digest = Sha256::digest(format!("{relative_path}\n{content}").as_bytes());
    format!("memory:{digest:x}")
}

#[cfg(test)]
#[path = "tests/memory_tests.rs"]
mod tests;
