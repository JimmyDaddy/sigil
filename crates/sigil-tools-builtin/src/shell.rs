use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
    sync::Arc,
};

use anyhow::{Context, Result};
use async_trait::async_trait;
use serde_json::{Value, json};
use sigil_kernel::{
    ExecutionBackend, ExecutionReceipt, ExecutionRequest, Tool, ToolAccess, ToolCategory,
    ToolContext, ToolErrorKind, ToolOperation, ToolPreviewCapability, ToolResult, ToolResultMeta,
    ToolSpec, ToolSubject,
};

use crate::{
    constants::{DEFAULT_TEXT_LIMIT_BYTES, HARD_TEXT_LIMIT_BYTES, SIGIL_SCRATCH_DIR_ENV},
    path::{
        ResolvedToolPath, absolute_path_from, canonical_workspace_root, resolve_tool_path_from_base,
    },
    support::{limit_text_head_tail, required_string},
};

pub(crate) struct BashTool {
    pub(crate) scratch_root: PathBuf,
    pub(crate) scratch_label: String,
    pub(crate) backend: Arc<dyn ExecutionBackend>,
}

#[async_trait]
impl Tool for BashTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "bash".to_owned(),
            description: format!(
                "Run a shell command from the workspace root. Use ${SIGIL_SCRATCH_DIR_ENV} for temporary shell files (shown as {}); OS temp directories are outside the workspace and require permission.external_directory.",
                self.scratch_label
            ),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "command": { "type": "string" },
                    "timeout_secs": { "type": "integer" }
                },
                "required": ["command"]
            }),
            category: ToolCategory::Shell,
            access: ToolAccess::Execute,
            preview: ToolPreviewCapability::None,
        }
    }

    fn permission_subjects(&self, ctx: &ToolContext, args: &Value) -> Result<Vec<ToolSubject>> {
        let command = required_string(args, "command")?;
        let mut subjects = vec![ToolSubject::command(
            command.to_owned(),
            command_permission_subject(command),
        )];
        subjects.extend(bash_path_subjects(&ctx.workspace_root, command)?);
        Ok(subjects)
    }

    fn permission_access(&self, _ctx: &ToolContext, args: &Value) -> Result<ToolAccess> {
        let command = required_string(args, "command")?;
        if bash_command_is_safe_readonly(command) {
            Ok(ToolAccess::Read)
        } else {
            Ok(ToolAccess::Execute)
        }
    }

    fn permission_operation(&self, _ctx: &ToolContext, args: &Value) -> Result<ToolOperation> {
        let command = required_string(args, "command")?;
        Ok(shell_command_permission_operation(command))
    }

    async fn execute(&self, ctx: ToolContext, call_id: String, args: Value) -> Result<ToolResult> {
        let command = required_string(&args, "command")?;
        let timeout_secs = args
            .get("timeout_secs")
            .and_then(Value::as_u64)
            .unwrap_or(ctx.timeout_secs);
        let scratch_root = absolute_path_from(&ctx.workspace_root, &self.scratch_root);
        tokio::fs::create_dir_all(&scratch_root)
            .await
            .with_context(|| format!("failed to create {}", self.scratch_label))?;
        let request =
            bash_execution_request(command, &ctx.workspace_root, &scratch_root, timeout_secs);
        let receipt = self.backend.execute(request).await?;
        bash_tool_result_from_execution_receipt(call_id, self.spec().name, receipt)
    }
}

pub(crate) fn bash_execution_request(
    command: &str,
    workspace_root: &Path,
    scratch_root: &Path,
    timeout_secs: u64,
) -> ExecutionRequest {
    ExecutionRequest {
        program: "sh".to_owned(),
        args: vec!["-c".to_owned(), command.to_owned()],
        cwd: workspace_root.to_path_buf(),
        env: BTreeMap::from([(
            SIGIL_SCRATCH_DIR_ENV.to_owned(),
            scratch_root.to_string_lossy().into_owned(),
        )]),
        timeout_ms: None,
        timeout_secs,
        cpu_time_ms: None,
        memory_limit_bytes: None,
        process_count_limit: None,
    }
}

pub(crate) fn bash_tool_result_from_execution_receipt(
    call_id: String,
    tool_name: String,
    receipt: ExecutionReceipt,
) -> Result<ToolResult> {
    if receipt.timed_out {
        let mut result = ToolResult::error(
            call_id,
            tool_name,
            ToolErrorKind::Timeout,
            "bash command timed out",
        );
        result.metadata = ToolResultMeta {
            exit_code: receipt.exit_code,
            stdout_bytes: Some(receipt.stdout.len() as u64),
            stderr_bytes: Some(receipt.stderr.len() as u64),
            total_bytes: Some(receipt.stdout.len() as u64 + receipt.stderr.len() as u64),
            details: execution_receipt_details(&receipt),
            ..ToolResultMeta::default()
        };
        return Ok(result);
    }
    let stdout = String::from_utf8_lossy(&receipt.stdout);
    let stderr = String::from_utf8_lossy(&receipt.stderr);
    let limit_bytes = DEFAULT_TEXT_LIMIT_BYTES.min(HARD_TEXT_LIMIT_BYTES);
    let limited_stdout = limit_text_head_tail(&stdout, limit_bytes);
    let limited_stderr = limit_text_head_tail(&stderr, limit_bytes);
    let mut content = String::new();
    if !limited_stdout.content.is_empty() {
        content.push_str(&limited_stdout.content);
    }
    if !limited_stderr.content.is_empty() {
        if !content.is_empty() {
            content.push('\n');
        }
        content.push_str(&limited_stderr.content);
    }
    let metadata = ToolResultMeta {
        exit_code: receipt.exit_code,
        stdout_bytes: Some(receipt.stdout.len() as u64),
        stderr_bytes: Some(receipt.stderr.len() as u64),
        truncated: limited_stdout.truncated || limited_stderr.truncated,
        omitted_bytes: Some(limited_stdout.omitted_bytes + limited_stderr.omitted_bytes),
        limit_bytes: Some(limit_bytes as u64),
        returned_bytes: Some(limited_stdout.returned_bytes + limited_stderr.returned_bytes),
        total_bytes: Some(receipt.stdout.len() as u64 + receipt.stderr.len() as u64),
        returned_lines: Some(limited_stdout.returned_lines + limited_stderr.returned_lines),
        total_lines: Some(limited_stdout.total_lines + limited_stderr.total_lines),
        details: execution_receipt_details(&receipt),
        ..ToolResultMeta::default()
    };
    if receipt.exit_code == Some(0) {
        Ok(ToolResult::ok(call_id, tool_name, content, metadata))
    } else {
        let mut result = ToolResult::error(
            call_id,
            tool_name,
            ToolErrorKind::ExitStatus,
            if content.is_empty() {
                "bash command exited with non-zero status".to_owned()
            } else {
                content.clone()
            },
        );
        result.content = content;
        result.metadata = metadata;
        Ok(result)
    }
}

pub(crate) fn execution_receipt_details(receipt: &ExecutionReceipt) -> Value {
    json!({
        "execution": {
            "backend": receipt.backend,
            "capabilities": receipt.capabilities,
            "network": receipt.network,
            "resources": receipt.resources,
        }
    })
}

pub(crate) fn command_permission_subject(command: &str) -> String {
    const MAX_CHARS: usize = 120;
    let normalized = command.split_whitespace().collect::<Vec<_>>().join(" ");
    let char_count = normalized.chars().count();
    if char_count <= MAX_CHARS {
        return normalized;
    }
    let truncated = normalized.chars().take(MAX_CHARS).collect::<String>();
    format!("{truncated}...")
}

pub(crate) fn shell_command_permission_operation(command: &str) -> ToolOperation {
    if shell_command_is_destructive(command) {
        ToolOperation::ExecuteDestructiveCommand
    } else if bash_command_is_safe_readonly(command) {
        ToolOperation::ExecuteReadOnlyCommand
    } else {
        ToolOperation::ExecuteUnknownCommand
    }
}

pub(crate) fn terminal_input_permission_operation(input: &str) -> ToolOperation {
    if shell_command_is_destructive(input) {
        ToolOperation::ExecuteDestructiveCommand
    } else {
        ToolOperation::SendTerminalInput
    }
}

pub(crate) fn shell_command_is_destructive(command: &str) -> bool {
    let tokens = tokenize_shell_subject_words(command);
    let mut segment = Vec::new();
    for token in tokens {
        if matches!(token.as_str(), "&&" | "||" | ";") {
            if shell_segment_is_destructive(&segment) {
                return true;
            }
            segment.clear();
        } else {
            segment.push(token);
        }
    }
    shell_segment_is_destructive(&segment)
}

pub(crate) fn shell_segment_is_destructive(words: &[String]) -> bool {
    let Some((command, args)) = shell_segment_command_and_args(words) else {
        return false;
    };

    if matches!(command, "sudo" | "doas" | "env" | "command") && !args.is_empty() {
        return shell_segment_is_destructive(args);
    }

    if shell_segment_has_overwrite_redirection(words) {
        return true;
    }

    match command {
        "rm" => true,
        "rmdir" => true,
        "truncate" => true,
        "dd" => args.iter().any(|word| word.starts_with("of=")),
        "find" => find_segment_is_destructive(args),
        "git" => git_segment_is_destructive(args),
        "sh" | "bash" | "zsh" | "fish" => shell_invocation_is_destructive(args),
        _ => false,
    }
}

pub(crate) fn shell_segment_command_and_args(words: &[String]) -> Option<(&str, &[String])> {
    let mut index = 0usize;
    while let Some(word) = words.get(index) {
        if is_shell_assignment(word) {
            index += 1;
            continue;
        }
        return Some((shell_command_basename(word), &words[index + 1..]));
    }
    None
}

pub(crate) fn is_shell_assignment(word: &str) -> bool {
    let Some((name, _)) = word.split_once('=') else {
        return false;
    };
    !name.is_empty()
        && name
            .chars()
            .all(|ch| ch == '_' || ch.is_ascii_alphanumeric())
        && name
            .chars()
            .next()
            .is_some_and(|ch| ch == '_' || ch.is_ascii_alphabetic())
}

pub(crate) fn shell_command_basename(command: &str) -> &str {
    Path::new(command)
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or(command)
}

pub(crate) fn shell_segment_has_overwrite_redirection(words: &[String]) -> bool {
    words
        .iter()
        .any(|word| is_overwrite_redirection_operator(word) || overwrite_redirection_target(word))
}

pub(crate) fn is_overwrite_redirection_operator(word: &str) -> bool {
    matches!(word, ">" | "1>" | "2>" | "&>")
}

pub(crate) fn overwrite_redirection_target(word: &str) -> bool {
    ["1>", "2>", "&>", ">"].iter().any(|prefix| {
        word.strip_prefix(prefix)
            .is_some_and(|target| !target.is_empty())
    })
}

pub(crate) fn find_segment_is_destructive(words: &[String]) -> bool {
    words.iter().enumerate().any(|(index, word)| {
        word == "-delete"
            || matches!(word.as_str(), "-exec" | "-execdir")
                && words
                    .get(index + 1)
                    .map(|command| shell_command_basename(command) == "rm")
                    .unwrap_or(false)
    })
}

pub(crate) fn git_segment_is_destructive(words: &[String]) -> bool {
    let Some(subcommand) = words.first().map(String::as_str) else {
        return false;
    };
    match subcommand {
        "clean" => true,
        "reset" => words.iter().skip(1).any(|word| word == "--hard"),
        "checkout" | "restore" => words
            .iter()
            .skip(1)
            .any(|word| word == "-f" || word == "--force"),
        _ => false,
    }
}

pub(crate) fn shell_invocation_is_destructive(words: &[String]) -> bool {
    words.windows(2).any(|pair| {
        matches!(pair[0].as_str(), "-c" | "-lc") && shell_command_is_destructive(&pair[1])
    })
}

pub(crate) fn bash_command_is_safe_readonly(command: &str) -> bool {
    let trimmed = command.trim();
    if trimmed.is_empty() || contains_unsupported_safe_shell_syntax(trimmed) {
        return false;
    }

    let tokens = tokenize_shell_subject_words(trimmed);
    if tokens.is_empty() {
        return false;
    }

    let mut segment = Vec::new();
    for token in tokens {
        if matches!(token.as_str(), "&&" | "||" | ";") {
            if !bash_segment_is_safe_readonly(&segment) {
                return false;
            }
            segment.clear();
        } else {
            segment.push(token);
        }
    }
    bash_segment_is_safe_readonly(&segment)
}

pub(crate) fn contains_unsupported_safe_shell_syntax(command: &str) -> bool {
    command.chars().any(|ch| {
        matches!(
            ch,
            '|' | '>' | '<' | '$' | '`' | '(' | ')' | '*' | '?' | '[' | ']'
        )
    })
}

pub(crate) fn bash_segment_is_safe_readonly(words: &[String]) -> bool {
    let Some(command) = words.first().map(String::as_str) else {
        return false;
    };

    if words
        .iter()
        .skip(1)
        .any(|word| is_redirection_operator(word) || redirection_target(word).is_some())
    {
        return false;
    }

    if is_help_or_version_query(words) {
        return true;
    }

    match command {
        "pwd" | "ls" | "cat" | "head" | "tail" | "wc" | "stat" | "du" | "file" | "readlink"
        | "realpath" | "basename" | "dirname" | "diff" | "cmp" | "grep" | "rg" | "which"
        | "uname" | "date" | "whoami" | "id" => true,
        "command" => matches!(words.get(1).map(String::as_str), Some("-v")) && words.len() >= 3,
        "find" => find_segment_is_safe_readonly(words),
        "git" => git_segment_is_safe_readonly(words),
        _ => false,
    }
}

pub(crate) fn is_help_or_version_query(words: &[String]) -> bool {
    words.len() == 2
        && matches!(
            words[1].as_str(),
            "--version" | "-V" | "--help" | "-h" | "help"
        )
}

pub(crate) fn find_segment_is_safe_readonly(words: &[String]) -> bool {
    !words.iter().skip(1).any(|word| {
        matches!(
            word.as_str(),
            "-exec" | "-execdir" | "-ok" | "-okdir" | "-delete" | "-fprint" | "-fprintf" | "-fls"
        )
    })
}

pub(crate) fn git_segment_is_safe_readonly(words: &[String]) -> bool {
    let Some(subcommand) = words.get(1).map(String::as_str) else {
        return false;
    };
    match subcommand {
        "status" | "diff" | "log" | "show" | "blame" | "rev-parse" | "ls-files" | "grep" => true,
        "branch" => words
            .iter()
            .skip(2)
            .all(|word| matches!(word.as_str(), "--show-current" | "--list")),
        _ => false,
    }
}

pub(crate) fn bash_path_subjects(workspace_root: &Path, command: &str) -> Result<Vec<ToolSubject>> {
    let workspace_root = canonical_workspace_root(workspace_root)?;
    bash_path_subjects_from_cwd(&workspace_root, &workspace_root, command)
}

pub(crate) fn bash_path_subjects_from_cwd(
    workspace_root: &Path,
    initial_cwd: &Path,
    command: &str,
) -> Result<Vec<ToolSubject>> {
    let tokens = tokenize_shell_subject_words(command);
    let mut subjects = Vec::new();
    let mut cwd = initial_cwd.to_path_buf();
    let mut segment_words = Vec::new();
    for token in tokens {
        if token == "&&" || token == "||" || token == ";" {
            collect_bash_segment_subjects(workspace_root, &mut cwd, &segment_words, &mut subjects)?;
            segment_words.clear();
        } else {
            segment_words.push(token);
        }
    }
    collect_bash_segment_subjects(workspace_root, &mut cwd, &segment_words, &mut subjects)?;
    Ok(subjects)
}

pub(crate) fn collect_bash_segment_subjects(
    workspace_root: &Path,
    cwd: &mut PathBuf,
    words: &[String],
    subjects: &mut Vec<ToolSubject>,
) -> Result<()> {
    if words.is_empty() {
        return Ok(());
    }

    let command = words[0].as_str();
    let mut index = 1usize;
    if command == "cd" {
        if let Some(target) = words.get(1).filter(|word| !word.starts_with('-')) {
            let resolved = resolve_tool_path_from_base(workspace_root, cwd, target)?;
            subjects.push(resolved_tool_path_subject(resolved.clone()));
            *cwd = resolved.canonical;
        }
        return Ok(());
    }

    while index < words.len() {
        let word = &words[index];
        if let Some(target) = redirection_target(word) {
            subjects.push(shell_path_subject(workspace_root, cwd, target)?);
        } else if command == "dd" && word.starts_with("of=") && word.len() > 3 {
            subjects.push(shell_path_subject(workspace_root, cwd, &word[3..])?);
        } else if is_redirection_operator(word) {
            if let Some(target) = words.get(index + 1) {
                subjects.push(shell_path_subject(workspace_root, cwd, target)?);
                index += 1;
            }
        } else if is_path_argument(command, word) {
            subjects.push(shell_path_subject(workspace_root, cwd, word)?);
        }
        index += 1;
    }
    Ok(())
}

pub(crate) fn shell_path_subject(
    workspace_root: &Path,
    cwd: &Path,
    requested: &str,
) -> Result<ToolSubject> {
    resolve_tool_path_from_base(workspace_root, cwd, requested).map(resolved_tool_path_subject)
}

pub(crate) fn resolved_tool_path_subject(resolved: ResolvedToolPath) -> ToolSubject {
    ToolSubject::path_with_scope(
        resolved.original,
        resolved.normalized,
        Some(resolved.canonical),
        resolved.scope,
    )
}

pub(crate) fn tokenize_shell_subject_words(command: &str) -> Vec<String> {
    let mut words = Vec::new();
    let mut current = String::new();
    let mut chars = command.chars().peekable();
    let mut quote = None::<char>;
    while let Some(ch) = chars.next() {
        if quote.is_some() {
            if Some(ch) == quote {
                quote = None;
            } else if ch == '\\' {
                if let Some(next) = chars.next() {
                    current.push(next);
                }
            } else {
                current.push(ch);
            }
            continue;
        }

        match ch {
            '\'' | '"' => quote = Some(ch),
            '\\' => {
                if let Some(next) = chars.next() {
                    current.push(next);
                }
            }
            ' ' | '\t' | '\n' => {
                if !current.is_empty() {
                    words.push(std::mem::take(&mut current));
                }
            }
            ';' => {
                if !current.is_empty() {
                    words.push(std::mem::take(&mut current));
                }
                words.push(";".to_owned());
            }
            '&' if chars.peek() == Some(&'&') => {
                chars.next();
                if !current.is_empty() {
                    words.push(std::mem::take(&mut current));
                }
                words.push("&&".to_owned());
            }
            '|' if chars.peek() == Some(&'|') => {
                chars.next();
                if !current.is_empty() {
                    words.push(std::mem::take(&mut current));
                }
                words.push("||".to_owned());
            }
            _ => current.push(ch),
        }
    }
    if !current.is_empty() {
        words.push(current);
    }
    words
}

pub(crate) fn is_redirection_operator(word: &str) -> bool {
    matches!(
        word,
        ">" | ">>" | "<" | "<<" | "2>" | "2>>" | "&>" | "&>>" | "1>" | "1>>"
    )
}

pub(crate) fn redirection_target(word: &str) -> Option<&str> {
    for prefix in [">>", ">", "<", "2>>", "2>", "&>>", "&>", "1>>", "1>"] {
        if let Some(target) = word
            .strip_prefix(prefix)
            .filter(|target| !target.is_empty() && !target.starts_with('&'))
        {
            return Some(target);
        }
    }
    None
}

pub(crate) fn is_path_argument(command: &str, word: &str) -> bool {
    if word.starts_with('-') || word.contains("://") {
        return false;
    }
    if word.starts_with('/')
        || word.starts_with("./")
        || word.starts_with("../")
        || word == "."
        || word == ".."
        || word.contains('/')
    {
        return true;
    }
    matches!(
        command,
        "cat"
            | "head"
            | "tail"
            | "wc"
            | "stat"
            | "du"
            | "file"
            | "readlink"
            | "realpath"
            | "basename"
            | "dirname"
            | "diff"
            | "cmp"
            | "ls"
            | "find"
            | "rm"
            | "rmdir"
            | "truncate"
            | "dd"
    )
}
