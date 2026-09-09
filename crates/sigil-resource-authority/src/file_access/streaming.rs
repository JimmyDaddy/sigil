//! Bounded text projection over authority-opened handles.

use std::io::{BufRead, BufReader, Read, Write};

use sigil_kernel::managed_file_access::{
    ManagedFileAccessErrorV1, ManagedFileExecutionContextV1, ManagedFileOutputCaptureV1,
    ManagedFileOutputDetailsV1,
};
use sigil_kernel::session::ToolArtifactCaptureSink;

use super::PhysicalExecutionOutcomeV1;

const MAX_LINE_BYTES: usize = 1024 * 1024;
const MAX_MODEL_LINE_CHARS: usize = 2000;

pub(super) fn check_context(
    context: &ManagedFileExecutionContextV1,
) -> Result<(), ManagedFileAccessErrorV1> {
    if context
        .cancellation
        .as_ref()
        .is_some_and(|handle| handle.is_cancel_requested())
    {
        return Err(ManagedFileAccessErrorV1::Interrupted);
    }
    if context
        .deadline
        .is_some_and(|deadline| std::time::Instant::now() >= deadline)
    {
        return Err(ManagedFileAccessErrorV1::Timeout);
    }
    Ok(())
}

struct LogicalLine {
    bytes: Vec<u8>,
    source_bytes: u64,
    oversized: bool,
}

fn logical_line(
    reader: &mut impl BufRead,
    context: &ManagedFileExecutionContextV1,
) -> Result<Option<LogicalLine>, ManagedFileAccessErrorV1> {
    let mut bytes = Vec::new();
    let mut source_bytes = 0u64;
    let mut oversized = false;
    let mut saw_data = false;
    loop {
        check_context(context)?;
        let available = reader.fill_buf().map_err(super::relative_io_error)?;
        if available.is_empty() {
            break;
        }
        saw_data = true;
        let newline = available.iter().position(|byte| *byte == b'\n');
        let consumed = newline.map_or(available.len(), |index| index + 1);
        let body_len = newline.unwrap_or(available.len());
        source_bytes = source_bytes.saturating_add(body_len as u64);
        if !oversized {
            if body_len <= MAX_LINE_BYTES.saturating_sub(bytes.len()) {
                bytes.extend_from_slice(&available[..body_len]);
            } else {
                bytes.clear();
                oversized = true;
            }
        }
        reader.consume(consumed);
        if newline.is_some() {
            break;
        }
    }
    if !saw_data {
        return Ok(None);
    }
    if bytes.last() == Some(&b'\r') {
        bytes.pop();
        source_bytes = source_bytes.saturating_sub(1);
    }
    Ok(Some(LogicalLine {
        bytes,
        source_bytes,
        oversized,
    }))
}

fn model_line(line: &str) -> String {
    if line.chars().count() <= MAX_MODEL_LINE_CHARS {
        return line.to_owned();
    }
    let mut text: String = line.chars().take(MAX_MODEL_LINE_CHARS).collect();
    text.push_str("[sigil: line truncated]");
    text
}

fn finish_capture(
    sink: Option<ToolArtifactCaptureSink>,
    source_bytes: u64,
    redactions: u32,
) -> ManagedFileOutputCaptureV1 {
    match sink {
        None => ManagedFileOutputCaptureV1::NotAttached,
        Some(sink) => match sink.finish_with_source_evidence(source_bytes, redactions) {
            Ok(descriptor) => ManagedFileOutputCaptureV1::Published {
                descriptor: Box::new(descriptor),
            },
            Err(_) => ManagedFileOutputCaptureV1::Unavailable {
                observed_bytes: source_bytes,
            },
        },
    }
}

pub(super) fn read(
    file: std::fs::File,
    context: &mut ManagedFileExecutionContextV1,
    offset: usize,
    limit: usize,
    max_bytes: usize,
) -> Result<PhysicalExecutionOutcomeV1, ManagedFileAccessErrorV1> {
    let max_bytes = max_bytes.min(sigil_kernel::session::TOOL_RESULT_INLINE_CAPTURE_MAX_BYTES);
    let metadata = file.metadata().map_err(super::relative_io_error)?;
    if !metadata.is_file() {
        return Err(ManagedFileAccessErrorV1::NotRegularFile);
    }
    let source_truncated = metadata.len() > super::MAX_READ_SCAN_BYTES;
    let mut reader = BufReader::new(file.take(super::MAX_READ_SCAN_BYTES));
    let mut sink = context.output_sink.take();
    let mut payload = String::new();
    let mut returned_lines = 0u64;
    let mut selected_source_bytes = 0u64;
    let mut selected_bytes = 0u64;
    let mut selected_lines = 0usize;
    let mut total_lines = 0usize;
    let mut truncated = source_truncated;
    let mut accepting = true;
    let mut redactions = 0u32;
    let mut oversized_lines = 0u64;
    while let Some(line) = logical_line(&mut reader, context)? {
        let index = total_lines;
        total_lines = total_lines.saturating_add(1);
        if index < offset || selected_lines >= limit {
            continue;
        }
        let separator = usize::from(selected_lines > 0);
        selected_source_bytes = selected_source_bytes
            .saturating_add(separator as u64)
            .saturating_add(line.source_bytes);
        let safe = if line.oversized {
            oversized_lines = oversized_lines.saturating_add(1);
            redactions = redactions.saturating_add(1);
            format!(
                "[sigil: policy omitted oversized line {total_lines} ({} bytes)]",
                line.source_bytes
            )
        } else {
            let bytes = match std::str::from_utf8(&line.bytes) {
                Ok(text) => text,
                Err(error) if source_truncated && error.error_len().is_none() => {
                    std::str::from_utf8(&line.bytes[..error.valid_up_to()]).map_err(|_| {
                        ManagedFileAccessErrorV1::InvalidInput("file is not UTF-8 text".to_owned())
                    })?
                }
                Err(_) => {
                    return Err(ManagedFileAccessErrorV1::InvalidInput(
                        "file is not UTF-8 text".to_owned(),
                    ));
                }
            };
            let safe = sigil_kernel::safe_persistence_text(bytes);
            redactions = redactions.saturating_add(u32::from(safe != bytes));
            safe
        };
        selected_bytes = selected_bytes
            .saturating_add(separator as u64)
            .saturating_add(safe.len() as u64);
        if let Some(sink) = sink.as_mut() {
            if separator > 0 {
                sink.write_all(b"\n").map_err(super::relative_io_error)?;
            }
            sink.write_all(safe.as_bytes())
                .map_err(super::relative_io_error)?;
        }
        selected_lines = selected_lines.saturating_add(1);
        let model = model_line(&safe);
        let model_separator = usize::from(returned_lines > 0);
        if accepting
            && payload
                .len()
                .saturating_add(model_separator)
                .saturating_add(model.len())
                <= max_bytes
        {
            if model_separator > 0 {
                payload.push('\n');
            }
            payload.push_str(&model);
            returned_lines = returned_lines.saturating_add(1);
            truncated |= model != safe;
        } else {
            if accepting {
                if model_separator > 0 && payload.len() < max_bytes {
                    payload.push('\n');
                }
                let prefix = super::truncate_utf8(&model, max_bytes.saturating_sub(payload.len()));
                if !prefix.is_empty() {
                    payload.push_str(&prefix);
                    returned_lines = returned_lines.saturating_add(1);
                }
            }
            truncated = true;
            accepting = false;
        }
    }
    let next_offset = (offset.saturating_add(selected_lines) < total_lines)
        .then_some(offset.saturating_add(selected_lines));
    truncated |= next_offset.is_some();
    check_context(context)?;
    Ok(PhysicalExecutionOutcomeV1 {
        payload,
        observed_bytes: metadata.len(),
        returned_lines,
        total_lines: total_lines as u64,
        truncated,
        output_capture: finish_capture(sink, selected_source_bytes, redactions),
        output_details: ManagedFileOutputDetailsV1::Read {
            selected_bytes,
            next_offset,
            oversized_lines,
        },
        ..Default::default()
    })
}

pub(super) struct GrepStream {
    payload: Vec<u8>,
    sink: Option<ToolArtifactCaptureSink>,
    limit: usize,
    max_bytes: usize,
    returned: u64,
    total: u64,
    source_bytes: u64,
    pub(super) scanned_bytes: u64,
    redactions: u32,
    binary_files: u64,
    oversized_lines: u64,
    pub(super) truncated: bool,
}

impl GrepStream {
    pub(super) fn new(
        context: &mut ManagedFileExecutionContextV1,
        limit: usize,
        max_bytes: usize,
    ) -> Result<Self, ManagedFileAccessErrorV1> {
        let max_bytes = max_bytes.min(sigil_kernel::session::TOOL_RESULT_INLINE_CAPTURE_MAX_BYTES);
        if max_bytes < 2 {
            return Err(ManagedFileAccessErrorV1::InvalidInput(
                "grep output budget must hold a JSON array".to_owned(),
            ));
        }
        let mut sink = context.output_sink.take();
        if let Some(sink) = sink.as_mut() {
            sink.write_all(b"[").map_err(super::relative_io_error)?;
        }
        Ok(Self {
            payload: vec![b'['],
            sink,
            limit,
            max_bytes,
            returned: 0,
            total: 0,
            source_bytes: 1,
            scanned_bytes: 0,
            redactions: 0,
            binary_files: 0,
            oversized_lines: 0,
            truncated: false,
        })
    }

    pub(super) fn scan(
        &mut self,
        file: std::fs::File,
        path: &str,
        regex: &regex::Regex,
        context: &ManagedFileExecutionContextV1,
    ) -> Result<(), ManagedFileAccessErrorV1> {
        check_context(context)?;
        let metadata = file.metadata().map_err(super::relative_io_error)?;
        if !metadata.is_file() {
            return Ok(());
        }
        let budget = super::MAX_GREP_SCAN_BYTES.saturating_sub(self.scanned_bytes);
        if budget == 0 {
            self.truncated = true;
            return Ok(());
        }
        self.truncated |= metadata.len() > budget;
        let mut reader = BufReader::new(file.take(budget));
        let mut line_number = 0usize;
        while let Some(line) = logical_line(&mut reader, context)? {
            line_number = line_number.saturating_add(1);
            if line.oversized {
                self.oversized_lines = self.oversized_lines.saturating_add(1);
                continue;
            }
            let Ok(text) = std::str::from_utf8(&line.bytes) else {
                self.binary_files = self.binary_files.saturating_add(1);
                break;
            };
            if text.contains('\0') {
                self.binary_files = self.binary_files.saturating_add(1);
                break;
            }
            if !regex.is_match(text) {
                continue;
            }
            let raw =
                serde_json::json!({"path": path, "line": line_number, "text": model_line(text)});
            let safe = sigil_kernel::safe_persistence_json_value(raw.clone());
            self.redactions = self.redactions.saturating_add(u32::from(safe != raw));
            let raw_encoded = serde_json::to_vec(&raw).map_err(|error| {
                ManagedFileAccessErrorV1::PhysicalExecutionFailed(error.to_string())
            })?;
            let encoded = serde_json::to_vec(&safe).map_err(|error| {
                ManagedFileAccessErrorV1::PhysicalExecutionFailed(error.to_string())
            })?;
            self.source_bytes = self
                .source_bytes
                .saturating_add(u64::from(self.total > 0))
                .saturating_add(raw_encoded.len() as u64);
            if let Some(sink) = self.sink.as_mut() {
                if self.total > 0 {
                    sink.write_all(b",").map_err(super::relative_io_error)?;
                }
                sink.write_all(&encoded).map_err(super::relative_io_error)?;
            }
            self.total = self.total.saturating_add(1);
            let separator = usize::from(self.returned > 0);
            if self.returned < self.limit as u64
                && self
                    .payload
                    .len()
                    .saturating_add(separator)
                    .saturating_add(encoded.len())
                    .saturating_add(1)
                    <= self.max_bytes
            {
                if separator > 0 {
                    self.payload.push(b',');
                }
                self.payload.extend_from_slice(&encoded);
                self.returned = self.returned.saturating_add(1);
            }
        }
        // Include bytes buffered by BufReader, including binary data that stopped decoding.
        self.scanned_bytes = self
            .scanned_bytes
            .saturating_add(budget.saturating_sub(reader.get_ref().limit()));
        Ok(())
    }

    pub(super) fn finish(
        mut self,
        context: &ManagedFileExecutionContextV1,
    ) -> Result<PhysicalExecutionOutcomeV1, ManagedFileAccessErrorV1> {
        check_context(context)?;
        self.payload.push(b']');
        if let Some(sink) = self.sink.as_mut() {
            sink.write_all(b"]").map_err(super::relative_io_error)?;
        }
        Ok(PhysicalExecutionOutcomeV1 {
            payload: String::from_utf8(self.payload).map_err(|error| {
                ManagedFileAccessErrorV1::PhysicalExecutionFailed(error.to_string())
            })?,
            observed_bytes: self.scanned_bytes,
            returned_lines: self.returned,
            total_entries: self.total,
            truncated: self.truncated || self.returned < self.total,
            output_capture: finish_capture(
                self.sink,
                self.source_bytes.saturating_add(1),
                self.redactions,
            ),
            output_details: ManagedFileOutputDetailsV1::Search {
                binary_files_skipped: self.binary_files,
                oversized_lines_skipped: self.oversized_lines,
            },
            ..Default::default()
        })
    }
}
