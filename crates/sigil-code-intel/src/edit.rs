use std::{collections::BTreeMap, fs, path::Path};

use anyhow::{Context, Result, anyhow, bail};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use similar::TextDiff;

use crate::{
    lsp::lsp_uri_to_workspace_path,
    service::{CodeRange, parse_range},
    workspace::resolve_workspace_file,
};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub struct CodeWorkspaceEdit {
    pub files: Vec<CodeEditFile>,
    pub external_changes_filtered: usize,
    pub unsupported_changes_filtered: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub struct CodeEditFile {
    pub path: String,
    pub edits: Vec<CodeTextEdit>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub struct CodeTextEdit {
    pub range: CodeRange,
    pub new_text: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CodeEditPreviewFile {
    pub path: String,
    pub diff: String,
}

impl CodeWorkspaceEdit {
    pub fn changed_files(&self) -> Vec<String> {
        self.files.iter().map(|file| file.path.clone()).collect()
    }

    pub fn total_edits(&self) -> usize {
        self.files.iter().map(|file| file.edits.len()).sum()
    }

    pub fn previews(&self, workspace_root: &Path) -> Result<Vec<CodeEditPreviewFile>> {
        let mut previews = Vec::new();
        for file in &self.files {
            let resolved = resolve_workspace_file(workspace_root, &file.path)?;
            let current = fs::read_to_string(&resolved)
                .with_context(|| format!("failed to read {}", resolved.display()))?;
            let proposed = apply_text_edits(&current, &file.edits)
                .with_context(|| format!("failed to preview edits for {}", file.path))?;
            previews.push(CodeEditPreviewFile {
                path: file.path.clone(),
                diff: render_unified_diff(
                    &current,
                    &proposed,
                    &format!("current/{}", file.path),
                    &format!("proposed/{}", file.path),
                ),
            });
        }
        Ok(previews)
    }
}

pub fn workspace_edit_from_lsp(workspace_root: &Path, value: &Value) -> Result<CodeWorkspaceEdit> {
    let mut files = BTreeMap::<String, Vec<CodeTextEdit>>::new();
    let mut external_changes_filtered = 0usize;
    let mut unsupported_changes_filtered = 0usize;

    if let Some(changes) = value.get("changes").and_then(Value::as_object) {
        for (uri, edits) in changes {
            collect_uri_edits(
                workspace_root,
                uri,
                edits.as_array().map(Vec::as_slice).unwrap_or(&[]),
                &mut files,
                &mut external_changes_filtered,
            )?;
        }
    }

    if let Some(document_changes) = value.get("documentChanges").and_then(Value::as_array) {
        for change in document_changes {
            if let Some(edits) = change.get("edits").and_then(Value::as_array) {
                let uri = change
                    .get("textDocument")
                    .and_then(|document| document.get("uri"))
                    .and_then(Value::as_str);
                if let Some(uri) = uri {
                    collect_uri_edits(
                        workspace_root,
                        uri,
                        edits,
                        &mut files,
                        &mut external_changes_filtered,
                    )?;
                } else {
                    unsupported_changes_filtered = unsupported_changes_filtered.saturating_add(1);
                }
            } else {
                unsupported_changes_filtered = unsupported_changes_filtered.saturating_add(1);
            }
        }
    }

    let mut files = files
        .into_iter()
        .map(|(path, edits)| CodeEditFile { path, edits })
        .collect::<Vec<_>>();
    files.sort_by(|left, right| left.path.cmp(&right.path));

    Ok(CodeWorkspaceEdit {
        files,
        external_changes_filtered,
        unsupported_changes_filtered,
    })
}

fn collect_uri_edits(
    workspace_root: &Path,
    uri: &str,
    edits: &[Value],
    files: &mut BTreeMap<String, Vec<CodeTextEdit>>,
    external_changes_filtered: &mut usize,
) -> Result<()> {
    let Some((path, _canonical)) = lsp_uri_to_workspace_path(workspace_root, uri) else {
        *external_changes_filtered = external_changes_filtered.saturating_add(1);
        return Ok(());
    };
    let parsed = edits
        .iter()
        .map(parse_text_edit)
        .collect::<Result<Vec<_>>>()?;
    files.entry(path).or_default().extend(parsed);
    Ok(())
}

fn parse_text_edit(value: &Value) -> Result<CodeTextEdit> {
    let range = value
        .get("range")
        .and_then(parse_range)
        .ok_or_else(|| anyhow!("workspace edit is missing a valid range"))?;
    let new_text = value
        .get("newText")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow!("workspace edit is missing newText"))?
        .to_owned();
    Ok(CodeTextEdit { range, new_text })
}

pub fn apply_text_edits(current: &str, edits: &[CodeTextEdit]) -> Result<String> {
    if edits.is_empty() {
        return Ok(current.to_owned());
    }
    let mut ranges = edits
        .iter()
        .map(|edit| {
            let start = utf16_position_to_byte_index(
                current,
                edit.range.start_line,
                edit.range.start_character,
            )?;
            let end = utf16_position_to_byte_index(
                current,
                edit.range.end_line,
                edit.range.end_character,
            )?;
            if start > end {
                bail!("workspace edit range starts after it ends");
            }
            Ok((start, end, edit.new_text.clone()))
        })
        .collect::<Result<Vec<_>>>()?;
    ranges.sort_by_key(|(start, end, _)| (*start, *end));
    for pair in ranges.windows(2) {
        if pair[0].1 > pair[1].0 {
            bail!("workspace edit contains overlapping ranges");
        }
    }
    let mut updated = current.to_owned();
    for (start, end, new_text) in ranges.into_iter().rev() {
        updated.replace_range(start..end, &new_text);
    }
    Ok(updated)
}

fn utf16_position_to_byte_index(text: &str, line: u64, character: u64) -> Result<usize> {
    if line == 0 {
        bail!("LSP line numbers are expected to be 1-based after parsing");
    }
    let line_index = usize::try_from(line - 1).map_err(|_| anyhow!("line number is too large"))?;
    let character =
        usize::try_from(character).map_err(|_| anyhow!("character offset is too large"))?;
    let starts = line_start_offsets(text);
    let Some(&line_start) = starts.get(line_index) else {
        bail!("line {line} is outside the document");
    };
    let line_end = text[line_start..]
        .find('\n')
        .map(|offset| line_start + offset)
        .unwrap_or(text.len());

    let mut utf16_units = 0usize;
    for (relative_index, ch) in text[line_start..line_end].char_indices() {
        if utf16_units == character {
            return Ok(line_start + relative_index);
        }
        utf16_units = utf16_units.saturating_add(ch.len_utf16());
        if utf16_units > character {
            bail!("character offset lands inside a UTF-16 surrogate pair");
        }
    }
    if utf16_units == character {
        return Ok(line_end);
    }
    bail!("character offset {character} is outside line {line}")
}

fn line_start_offsets(text: &str) -> Vec<usize> {
    let mut starts = vec![0];
    for (index, ch) in text.char_indices() {
        if ch == '\n' && index + 1 < text.len() {
            starts.push(index + 1);
        }
    }
    starts
}

pub(crate) fn render_unified_diff(
    current: &str,
    proposed: &str,
    current_label: &str,
    proposed_label: &str,
) -> String {
    let diff = TextDiff::from_lines(current, proposed)
        .unified_diff()
        .context_radius(2)
        .header(current_label, proposed_label)
        .to_string();

    if diff.trim().is_empty() {
        "No textual changes detected.".to_owned()
    } else {
        diff
    }
}

#[cfg(test)]
#[path = "tests/edit_tests.rs"]
mod tests;
