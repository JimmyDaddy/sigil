use super::*;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub struct CodeRange {
    pub start_line: u64,
    pub start_character: u64,
    pub end_line: u64,
    pub end_character: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub struct CodeSymbol {
    pub name: String,
    pub kind: String,
    pub path: String,
    pub range: CodeRange,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub container_name: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub struct CodeLocation {
    pub path: String,
    pub range: CodeRange,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub preview: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub struct CodeDiagnostic {
    pub path: String,
    pub range: CodeRange,
    pub severity: String,
    pub message: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub struct CodeActionSummary {
    pub title: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kind: Option<String>,
    pub is_preferred: bool,
    pub diagnostics: usize,
    pub has_edit: bool,
    pub has_command: bool,
}

pub(super) fn collect_lsp_symbols(
    value: &Value,
    path: &str,
    query: Option<&str>,
    symbols: &mut Vec<CodeSymbol>,
) {
    let Some(items) = value.as_array() else {
        return;
    };
    for item in items {
        collect_lsp_symbol_item(item, path, query, None, symbols);
    }
}

pub(super) fn collect_lsp_symbol_item(
    item: &Value,
    path: &str,
    query: Option<&str>,
    container: Option<String>,
    symbols: &mut Vec<CodeSymbol>,
) {
    let Some(name) = item.get("name").and_then(Value::as_str) else {
        return;
    };
    let matches_query = query
        .map(|needle| name.to_ascii_lowercase().contains(needle))
        .unwrap_or(true);
    if matches_query {
        let range = item
            .get("selectionRange")
            .or_else(|| item.get("range"))
            .and_then(parse_range)
            .unwrap_or(CodeRange {
                start_line: 1,
                start_character: 0,
                end_line: 1,
                end_character: 0,
            });
        symbols.push(CodeSymbol {
            name: name.to_owned(),
            kind: lsp_symbol_kind(item.get("kind").and_then(Value::as_u64)),
            path: path.to_owned(),
            range,
            container_name: container.clone(),
        });
    }
    if let Some(children) = item.get("children").and_then(Value::as_array) {
        for child in children {
            collect_lsp_symbol_item(child, path, query, Some(name.to_owned()), symbols);
        }
    }
}

pub(super) fn parse_diagnostic_value(
    workspace_root: &Path,
    fallback_path: &Path,
    value: &Value,
) -> Option<CodeDiagnostic> {
    Some(CodeDiagnostic {
        path: value
            .get("uri")
            .and_then(Value::as_str)
            .and_then(|uri| lsp_uri_to_workspace_path(workspace_root, uri).map(|item| item.0))
            .unwrap_or_else(|| workspace_relative_path(workspace_root, fallback_path)),
        range: value
            .get("range")
            .and_then(parse_range)
            .unwrap_or(CodeRange {
                start_line: 1,
                start_character: 0,
                end_line: 1,
                end_character: 0,
            }),
        severity: lsp_diagnostic_severity(value.get("severity").and_then(Value::as_u64)),
        message: value
            .get("message")
            .and_then(Value::as_str)
            .unwrap_or("diagnostic")
            .chars()
            .take(500)
            .collect(),
        source: value
            .get("source")
            .and_then(Value::as_str)
            .map(str::to_owned),
    })
}

pub(super) fn code_action_params(
    path: &Path,
    line: u64,
    character: u64,
    end_line: Option<u64>,
    end_character: Option<u64>,
    only: Option<&str>,
) -> Value {
    let end_line = end_line.unwrap_or(line);
    let end_character = end_character.unwrap_or(character);
    let mut context = json!({ "diagnostics": [] });
    if let Some(only) = only.filter(|value| !value.trim().is_empty()) {
        context["only"] = json!([only]);
    }
    json!({
        "textDocument": text_document_identifier(path),
        "range": {
            "start": {
                "line": line.saturating_sub(1),
                "character": character
            },
            "end": {
                "line": end_line.saturating_sub(1),
                "character": end_character
            }
        },
        "context": context
    })
}

pub(super) fn parse_code_action_summary(value: &Value) -> Option<CodeActionSummary> {
    let title = value.get("title")?.as_str()?.to_owned();
    Some(CodeActionSummary {
        title,
        kind: value.get("kind").and_then(Value::as_str).map(str::to_owned),
        is_preferred: value
            .get("isPreferred")
            .and_then(Value::as_bool)
            .unwrap_or(false),
        diagnostics: value
            .get("diagnostics")
            .and_then(Value::as_array)
            .map_or(0, Vec::len),
        has_edit: value.get("edit").is_some(),
        has_command: value.get("command").is_some(),
    })
}

pub(super) fn select_code_action(
    actions: Vec<Value>,
    title: Option<&str>,
    kind: Option<&str>,
) -> Result<Value> {
    let mut candidates = actions
        .into_iter()
        .filter(|action| {
            let title_matches = title.is_none_or(|expected| {
                action.get("title").and_then(Value::as_str) == Some(expected)
            });
            let kind_matches = kind.is_none_or(|expected| {
                action
                    .get("kind")
                    .and_then(Value::as_str)
                    .is_some_and(|actual| {
                        actual == expected || actual.starts_with(&format!("{expected}."))
                    })
            });
            title_matches && kind_matches
        })
        .collect::<Vec<_>>();
    if candidates.is_empty() {
        bail!("no code action matched the provided selector");
    }
    if candidates.len() == 1 {
        return Ok(candidates.remove(0));
    }
    let editable = candidates
        .iter()
        .filter(|action| action.get("edit").is_some())
        .collect::<Vec<_>>();
    if title.is_none() && kind.is_none() && editable.len() == 1 {
        return Ok(editable[0].clone());
    }
    bail!("multiple code actions matched; provide an exact title or narrower kind")
}

pub(super) fn pull_diagnostics_from_response(value: Value) -> Vec<Value> {
    if let Some(items) = value.get("items").and_then(Value::as_array) {
        return items.clone();
    }
    value.as_array().cloned().unwrap_or_default()
}

pub(super) fn is_rust_source_path(path: &Path) -> bool {
    path.extension()
        .and_then(std::ffi::OsStr::to_str)
        .is_some_and(|extension| extension.eq_ignore_ascii_case("rs"))
}

pub(crate) fn parse_range(value: &Value) -> Option<CodeRange> {
    let start = value.get("start")?;
    let end = value.get("end")?;
    Some(CodeRange {
        start_line: start.get("line")?.as_u64()?.saturating_add(1),
        start_character: start.get("character")?.as_u64()?,
        end_line: end.get("line")?.as_u64()?.saturating_add(1),
        end_character: end.get("character")?.as_u64()?,
    })
}

pub(super) fn lsp_symbol_kind(kind: Option<u64>) -> String {
    match kind {
        Some(1) => "file",
        Some(2) => "module",
        Some(3) => "namespace",
        Some(4) => "package",
        Some(5) => "class",
        Some(6) => "method",
        Some(7) => "property",
        Some(8) => "field",
        Some(9) => "constructor",
        Some(10) => "enum",
        Some(11) => "interface",
        Some(12) => "function",
        Some(13) => "variable",
        Some(14) => "constant",
        Some(15) => "string",
        Some(16) => "number",
        Some(17) => "boolean",
        Some(18) => "array",
        Some(19) => "object",
        Some(20) => "key",
        Some(21) => "null",
        Some(22) => "enum_member",
        Some(23) => "struct",
        Some(24) => "event",
        Some(25) => "operator",
        Some(26) => "type_parameter",
        _ => "symbol",
    }
    .to_owned()
}

pub(super) fn lsp_diagnostic_severity(severity: Option<u64>) -> String {
    match severity {
        Some(1) => "error",
        Some(2) => "warning",
        Some(3) => "information",
        Some(4) => "hint",
        _ => "unknown",
    }
    .to_owned()
}
