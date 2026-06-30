use anyhow::{Context, Result, anyhow};
use serde_json::Value;
use sha2::{Digest, Sha256};
use similar::TextDiff;
use tokio::task;

use crate::constants::MAX_MODEL_LINE_CHARS;

pub(crate) fn required_string<'a>(args: &'a Value, key: &str) -> Result<&'a str> {
    args.get(key)
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow!("missing string field {key}"))
}

pub(crate) fn optional_string<'a>(args: &'a Value, key: &str) -> Option<&'a str> {
    args.get(key).and_then(Value::as_str)
}

pub(crate) fn optional_usize(args: &Value, key: &str) -> Result<Option<usize>> {
    args.get(key)
        .map(|value| {
            value
                .as_u64()
                .ok_or_else(|| anyhow!("{key} must be a positive integer"))
                .and_then(|value| {
                    usize::try_from(value)
                        .map_err(|_| anyhow!("{key} is too large for this platform"))
                })
        })
        .transpose()
}

#[derive(Debug, Clone, Default)]
pub(crate) struct TextLimitResult {
    pub(crate) content: String,
    pub(crate) returned_bytes: u64,
    pub(crate) returned_lines: u64,
    pub(crate) total_bytes: u64,
    pub(crate) total_lines: u64,
    pub(crate) truncated: bool,
    pub(crate) omitted_bytes: u64,
}

pub(crate) fn limit_text_head(input: &str, max_bytes: usize, max_lines: usize) -> TextLimitResult {
    let mut output = String::new();
    let mut returned_lines = 0usize;
    let mut returned_bytes = 0usize;
    let total_lines = input.lines().count();
    let total_bytes = input.len();
    let mut truncated = false;

    for line in input.lines() {
        if returned_lines >= max_lines {
            truncated = true;
            break;
        }
        let line = truncate_line_for_model(line);
        let separator_bytes = usize::from(!output.is_empty());
        if returned_bytes + separator_bytes + line.len() > max_bytes {
            truncated = true;
            break;
        }
        if !output.is_empty() {
            output.push('\n');
            returned_bytes += 1;
        }
        returned_bytes += line.len();
        returned_lines += 1;
        output.push_str(&line);
    }

    if truncated {
        append_truncation_notice(&mut output);
    }

    TextLimitResult {
        content: output,
        returned_bytes: returned_bytes as u64,
        returned_lines: returned_lines as u64,
        total_bytes: total_bytes as u64,
        total_lines: total_lines as u64,
        truncated,
        omitted_bytes: total_bytes.saturating_sub(returned_bytes) as u64,
    }
}

pub(crate) fn limit_text_head_tail(input: &str, max_bytes: usize) -> TextLimitResult {
    if input.len() <= max_bytes {
        return TextLimitResult {
            content: input.to_owned(),
            returned_bytes: input.len() as u64,
            returned_lines: input.lines().count() as u64,
            total_bytes: input.len() as u64,
            total_lines: input.lines().count() as u64,
            truncated: false,
            omitted_bytes: 0,
        };
    }

    let head_budget = max_bytes / 2;
    let tail_budget = max_bytes.saturating_sub(head_budget);
    let head_end = floor_char_boundary(input, head_budget);
    let tail_start = ceil_char_boundary(input, input.len().saturating_sub(tail_budget));
    let omitted_bytes = tail_start.saturating_sub(head_end);
    let mut content = String::new();
    content.push_str(&input[..head_end]);
    if !content.ends_with('\n') {
        content.push('\n');
    }
    content.push_str(&format!(
        "[sigil: output truncated, omitted {omitted_bytes} bytes]\n"
    ));
    content.push_str(&input[tail_start..]);
    TextLimitResult {
        returned_bytes: (input.len() - omitted_bytes) as u64,
        returned_lines: content.lines().count() as u64,
        total_bytes: input.len() as u64,
        total_lines: input.lines().count() as u64,
        truncated: true,
        omitted_bytes: omitted_bytes as u64,
        content,
    }
}

pub(crate) fn truncate_line_for_model(line: &str) -> String {
    if line.chars().count() <= MAX_MODEL_LINE_CHARS {
        line.to_owned()
    } else {
        let mut truncated = line.chars().take(MAX_MODEL_LINE_CHARS).collect::<String>();
        truncated.push_str("[sigil: line truncated]");
        truncated
    }
}

pub(crate) fn append_truncation_notice(output: &mut String) {
    if !output.is_empty() {
        output.push('\n');
    }
    output.push_str(
        "[sigil: output truncated; use offset/limit or a narrower path/pattern to continue]",
    );
}

pub(crate) fn floor_char_boundary(value: &str, index: usize) -> usize {
    let mut index = index.min(value.len());
    while !value.is_char_boundary(index) {
        index -= 1;
    }
    index
}

pub(crate) fn ceil_char_boundary(value: &str, index: usize) -> usize {
    let mut index = index.min(value.len());
    while index < value.len() && !value.is_char_boundary(index) {
        index += 1;
    }
    index
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

pub(crate) fn sha256_hex(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let digest = Sha256::digest(bytes);
    let mut output = String::with_capacity(digest.len() * 2);
    for byte in digest {
        output.push(HEX[(byte >> 4) as usize] as char);
        output.push(HEX[(byte & 0x0f) as usize] as char);
    }
    output
}

pub(crate) async fn run_blocking_io<T, F>(label: &'static str, job: F) -> Result<T>
where
    T: Send + 'static,
    F: FnOnce() -> Result<T> + Send + 'static,
{
    task::spawn_blocking(job)
        .await
        .with_context(|| format!("{label} blocking task failed to join"))?
}
