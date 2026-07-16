use std::{collections::BTreeMap, io::ErrorKind, path::Path, time::Duration};

use anyhow::{Result, bail};
use serde_json::{Value, json};
use tokio::{
    io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt, BufReader},
    time,
};

use crate::{
    error::CodeIntelError,
    workspace::{file_uri_from_path, path_from_file_uri},
};

pub struct LspClient<R, W> {
    reader: BufReader<R>,
    writer: W,
    next_id: u64,
    diagnostics_by_uri: BTreeMap<String, Vec<Value>>,
}

impl<R, W> LspClient<R, W>
where
    R: AsyncRead + Unpin + Send,
    W: AsyncWrite + Unpin + Send,
{
    pub fn new(reader: R, writer: W) -> Self {
        Self {
            reader: BufReader::new(reader),
            writer,
            next_id: 1,
            diagnostics_by_uri: BTreeMap::new(),
        }
    }

    pub async fn initialize(
        &mut self,
        root: &Path,
        initialization_options: Value,
        timeout: Duration,
    ) -> Result<Value> {
        let root_uri = file_uri_from_path(root);
        let result = self
            .request(
                "initialize",
                json!({
                    "processId": std::process::id(),
                    "rootUri": root_uri,
                    "workspaceFolders": [{
                        "uri": root_uri,
                        "name": root.file_name().and_then(|value| value.to_str()).unwrap_or("workspace")
                    }],
                    "capabilities": client_capabilities(),
                    "initializationOptions": initialization_options
                }),
                timeout,
            )
            .await?;
        self.notify("initialized", json!({})).await?;
        Ok(result
            .get("capabilities")
            .cloned()
            .unwrap_or_else(|| json!({})))
    }

    pub async fn did_open(
        &mut self,
        path: &Path,
        language_id: &str,
        version: i32,
        text: String,
    ) -> Result<()> {
        self.notify(
            "textDocument/didOpen",
            json!({
                "textDocument": {
                    "uri": file_uri_from_path(path),
                    "languageId": language_id,
                    "version": version,
                    "text": text
                }
            }),
        )
        .await
    }

    pub async fn did_change(&mut self, path: &Path, version: i32, text: String) -> Result<()> {
        self.notify(
            "textDocument/didChange",
            json!({
                "textDocument": {
                    "uri": file_uri_from_path(path),
                    "version": version
                },
                "contentChanges": [{ "text": text }]
            }),
        )
        .await
    }

    pub async fn request(
        &mut self,
        method: &str,
        params: Value,
        timeout: Duration,
    ) -> Result<Value> {
        let id = self.next_id;
        self.next_id = self.next_id.saturating_add(1);
        let payload = json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": method,
            "params": params
        });
        self.write_message(&payload).await?;
        let operation = method.to_owned();
        time::timeout(timeout, self.read_until_response(id))
            .await
            .map_err(|_| CodeIntelError::Timeout { operation })?
    }

    pub async fn notify(&mut self, method: &str, params: Value) -> Result<()> {
        let payload = json!({
            "jsonrpc": "2.0",
            "method": method,
            "params": params
        });
        self.write_message(&payload).await
    }

    pub async fn shutdown(&mut self, timeout: Duration) -> Result<()> {
        let _ = self.request("shutdown", Value::Null, timeout).await;
        let _ = self.notify("exit", Value::Null).await;
        Ok(())
    }

    pub async fn wait_for_diagnostics(
        &mut self,
        uri: &str,
        timeout: Duration,
    ) -> Result<Vec<Value>> {
        if let Some(values) = self.diagnostics_by_uri.get(uri) {
            return Ok(values.clone());
        }
        let deadline = time::Instant::now() + timeout;
        while time::Instant::now() < deadline {
            let remaining = deadline.saturating_duration_since(time::Instant::now());
            match time::timeout(remaining, read_lsp_message(&mut self.reader)).await {
                Ok(Ok(Some(message))) => {
                    self.handle_server_message(&message);
                    if let Some(values) = self.diagnostics_by_uri.get(uri) {
                        return Ok(values.clone());
                    }
                }
                Ok(Ok(None)) => return Ok(Vec::new()),
                Ok(Err(error)) => return Err(error),
                Err(_) => return Ok(Vec::new()),
            }
        }
        Ok(Vec::new())
    }

    fn handle_server_message(&mut self, message: &Value) {
        if message.get("method").and_then(Value::as_str) != Some("textDocument/publishDiagnostics")
        {
            return;
        }
        let Some(params) = message.get("params") else {
            return;
        };
        let Some(uri) = params.get("uri").and_then(Value::as_str) else {
            return;
        };
        let diagnostics = params
            .get("diagnostics")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        self.diagnostics_by_uri.insert(uri.to_owned(), diagnostics);
    }

    async fn read_until_response(&mut self, id: u64) -> Result<Value> {
        loop {
            let Some(message) = read_lsp_message(&mut self.reader).await? else {
                bail!("language server closed stdout");
            };
            self.handle_server_message(&message);
            if message.get("id").and_then(Value::as_u64) != Some(id) {
                continue;
            }
            if let Some(error) = message.get("error") {
                let message = error
                    .get("message")
                    .and_then(Value::as_str)
                    .unwrap_or("language server returned an error");
                bail!("{message}");
            }
            return Ok(message.get("result").cloned().unwrap_or(Value::Null));
        }
    }

    async fn write_message(&mut self, value: &Value) -> Result<()> {
        let body = serde_json::to_vec(value)?;
        let header = format!("Content-Length: {}\r\n\r\n", body.len());
        self.writer.write_all(header.as_bytes()).await?;
        self.writer.write_all(&body).await?;
        self.writer.flush().await?;
        Ok(())
    }
}

fn client_capabilities() -> Value {
    json!({
        "workspace": {
            "workspaceFolders": true,
            "symbol": { "dynamicRegistration": false }
        },
        "textDocument": {
            "synchronization": { "didSave": false },
            "publishDiagnostics": { "relatedInformation": true },
            "documentSymbol": { "hierarchicalDocumentSymbolSupport": true },
            "definition": { "linkSupport": true },
            "references": {},
            "codeAction": {
                "codeActionLiteralSupport": {
                    "codeActionKind": {
                        "valueSet": ["quickfix", "refactor", "refactor.rename", "source"]
                    }
                },
                "resolveSupport": { "properties": ["edit"] }
            },
            "rename": { "prepareSupport": false },
            "diagnostic": { "dynamicRegistration": false },
            "hover": { "contentFormat": ["markdown", "plaintext"] }
        }
    })
}

pub fn document_symbol_supported(capabilities: &Value) -> bool {
    capability_supported(capabilities, "documentSymbolProvider")
}

pub fn definition_supported(capabilities: &Value) -> bool {
    capability_supported(capabilities, "definitionProvider")
}

pub fn references_supported(capabilities: &Value) -> bool {
    capability_supported(capabilities, "referencesProvider")
}

pub fn code_action_supported(capabilities: &Value) -> bool {
    capability_supported(capabilities, "codeActionProvider")
}

pub fn code_action_resolve_supported(capabilities: &Value) -> bool {
    capabilities
        .get("codeActionProvider")
        .and_then(|provider| provider.get("resolveProvider"))
        .and_then(Value::as_bool)
        .unwrap_or(false)
}

pub fn rename_supported(capabilities: &Value) -> bool {
    capability_supported(capabilities, "renameProvider")
}

pub fn workspace_symbol_supported(capabilities: &Value) -> bool {
    capability_supported(capabilities, "workspaceSymbolProvider")
}

pub fn diagnostics_supported(capabilities: &Value) -> bool {
    capability_supported(capabilities, "diagnosticProvider")
}

fn capability_supported(capabilities: &Value, key: &str) -> bool {
    match capabilities.get(key) {
        Some(Value::Bool(value)) => *value,
        Some(Value::Object(_)) => true,
        _ => false,
    }
}

pub async fn read_lsp_message<R>(reader: &mut BufReader<R>) -> Result<Option<Value>>
where
    R: AsyncRead + Unpin,
{
    let mut header = Vec::new();
    let mut byte = [0_u8; 1];
    loop {
        match reader.read_exact(&mut byte).await {
            Ok(_) => {
                header.push(byte[0]);
                if header.ends_with(b"\r\n\r\n") {
                    break;
                }
                if header.len() > 8192 {
                    return Err(CodeIntelError::Protocol {
                        reason: "message header exceeded 8192 bytes".to_owned(),
                    }
                    .into());
                }
            }
            Err(error) if error.kind() == ErrorKind::UnexpectedEof => {
                return Ok(None);
            }
            Err(error) => return Err(error.into()),
        }
    }

    let header_text = String::from_utf8(header).map_err(|error| CodeIntelError::Protocol {
        reason: format!("header is not utf-8: {error}"),
    })?;
    let content_length = parse_content_length(&header_text)?;
    let mut content = vec![0_u8; content_length];
    reader.read_exact(&mut content).await?;
    let value = serde_json::from_slice(&content).map_err(|error| CodeIntelError::Protocol {
        reason: format!("body is not valid json: {error}"),
    })?;
    Ok(Some(value))
}

#[cfg(test)]
pub fn encode_lsp_message(value: &Value) -> Result<Vec<u8>> {
    let body = serde_json::to_vec(value)?;
    let mut encoded = format!("Content-Length: {}\r\n\r\n", body.len()).into_bytes();
    encoded.extend_from_slice(&body);
    Ok(encoded)
}

fn parse_content_length(header: &str) -> Result<usize> {
    for line in header.lines() {
        let Some((name, value)) = line.split_once(':') else {
            continue;
        };
        if name.eq_ignore_ascii_case("Content-Length") {
            return value.trim().parse::<usize>().map_err(|error| {
                CodeIntelError::Protocol {
                    reason: format!("invalid content length: {error}"),
                }
                .into()
            });
        }
    }
    Err(CodeIntelError::Protocol {
        reason: "missing Content-Length header".to_owned(),
    }
    .into())
}

pub fn text_document_identifier(path: &Path) -> Value {
    json!({ "uri": file_uri_from_path(path) })
}

pub fn position_params(path: &Path, line: u64, character: u64) -> Value {
    json!({
        "textDocument": text_document_identifier(path),
        "position": {
            "line": line.saturating_sub(1),
            "character": character
        }
    })
}

pub fn lsp_uri_to_workspace_path(
    workspace_root: &Path,
    uri: &str,
) -> Option<(String, std::path::PathBuf)> {
    let path = path_from_file_uri(uri)?;
    let canonical = path.canonicalize().ok()?;
    let workspace = workspace_root.canonicalize().ok()?;
    if !canonical.starts_with(&workspace) {
        return None;
    }
    let relative = canonical
        .strip_prefix(&workspace)
        .unwrap_or(&canonical)
        .to_string_lossy()
        .to_string();
    Some((relative, canonical))
}

pub fn response_array(value: Value) -> Vec<Value> {
    match value {
        Value::Array(values) => values,
        Value::Null => Vec::new(),
        other => vec![other],
    }
}

pub fn lsp_error_to_reason(error: anyhow::Error) -> String {
    let message = error.to_string();
    if message.is_empty() {
        "language server request failed".to_owned()
    } else {
        message
    }
}

#[cfg(test)]
#[path = "tests/lsp_framing_tests.rs"]
mod tests;
