use std::{io, str};

use serde_json::Value;
use thiserror::Error;
use tokio::io::{AsyncBufRead, AsyncBufReadExt, AsyncWrite, AsyncWriteExt};

pub(super) const MCP_STDIO_FRAME_LIMIT_BYTES: usize = 4 * 1024 * 1024;

#[derive(Debug)]
pub(super) struct McpFrame {
    pub(super) value: Value,
    pub(super) wire_bytes: usize,
}

#[derive(Debug, Error)]
pub(super) enum McpFramingError {
    #[error("MCP stdio frame exceeded {limit_bytes} bytes")]
    FrameTooLarge {
        limit_bytes: usize,
        observed_at_least_bytes: usize,
    },
    #[error("MCP stdio stream closed")]
    StreamClosed,
    #[error("MCP stdio frame ended before its newline delimiter after {received_bytes} bytes")]
    MissingNewline { received_bytes: usize },
    #[error("MCP stdio frame exceeded the remaining operation wire budget of {limit_bytes} bytes")]
    WireBudgetExceeded {
        limit_bytes: usize,
        observed_at_least_bytes: usize,
    },
    #[error("MCP stdio frame is not valid UTF-8")]
    InvalidUtf8(#[source] str::Utf8Error),
    #[error("MCP stdio frame is not valid JSON")]
    InvalidJson(#[source] serde_json::Error),
    #[error("failed to read MCP stdio frame")]
    Read(#[source] io::Error),
    #[error("failed to write MCP stdio frame")]
    Write(#[source] io::Error),
}

impl McpFramingError {
    pub(super) fn code(&self) -> &'static str {
        match self {
            Self::FrameTooLarge { .. } => "frame_too_large",
            Self::StreamClosed => "stream_closed",
            Self::MissingNewline { .. } => "missing_newline",
            Self::WireBudgetExceeded { .. } => "wire_budget_exceeded",
            Self::InvalidUtf8(_) => "invalid_utf8",
            Self::InvalidJson(_) => "invalid_json",
            Self::Read(_) => "read_failed",
            Self::Write(_) => "write_failed",
        }
    }
}

#[cfg(test)]
pub(super) async fn read_ndjson_message<R>(reader: &mut R) -> Result<McpFrame, McpFramingError>
where
    R: AsyncBufRead + Unpin,
{
    read_ndjson_message_with_wire_limit(reader, usize::MAX).await
}

pub(super) async fn read_ndjson_message_with_wire_limit<R>(
    reader: &mut R,
    wire_limit_bytes: usize,
) -> Result<McpFrame, McpFramingError>
where
    R: AsyncBufRead + Unpin,
{
    let mut frame = Vec::with_capacity(8 * 1024);
    loop {
        let available = reader.fill_buf().await.map_err(McpFramingError::Read)?;
        if available.is_empty() {
            return if frame.is_empty() {
                Err(McpFramingError::StreamClosed)
            } else {
                Err(McpFramingError::MissingNewline {
                    received_bytes: frame.len(),
                })
            };
        }

        let newline = available.iter().position(|byte| *byte == b'\n');
        let consumed = newline.map_or(available.len(), |index| index + 1);
        let observed_wire_bytes = frame.len().saturating_add(consumed);
        if observed_wire_bytes > wire_limit_bytes {
            return Err(McpFramingError::WireBudgetExceeded {
                limit_bytes: wire_limit_bytes,
                observed_at_least_bytes: observed_wire_bytes,
            });
        }
        let available_body_bytes = newline.unwrap_or(available.len());
        let body_bytes = frame.len().saturating_add(available_body_bytes);
        let trailing_body_byte = if available_body_bytes > 0 {
            available.get(available_body_bytes - 1).copied()
        } else {
            frame.last().copied()
        };
        let valid_crlf_candidate = body_bytes == MCP_STDIO_FRAME_LIMIT_BYTES.saturating_add(1)
            && trailing_body_byte == Some(b'\r');
        if body_bytes > MCP_STDIO_FRAME_LIMIT_BYTES && !valid_crlf_candidate {
            return Err(McpFramingError::FrameTooLarge {
                limit_bytes: MCP_STDIO_FRAME_LIMIT_BYTES,
                observed_at_least_bytes: body_bytes,
            });
        }
        frame.extend_from_slice(&available[..consumed]);
        reader.consume(consumed);
        if newline.is_some() {
            break;
        }
    }

    let wire_bytes = frame.len();
    frame.pop();
    if frame.last() == Some(&b'\r') {
        frame.pop();
    }
    if frame.len() > MCP_STDIO_FRAME_LIMIT_BYTES {
        return Err(McpFramingError::FrameTooLarge {
            limit_bytes: MCP_STDIO_FRAME_LIMIT_BYTES,
            observed_at_least_bytes: frame.len(),
        });
    }
    str::from_utf8(&frame).map_err(McpFramingError::InvalidUtf8)?;
    let value = serde_json::from_slice(&frame).map_err(McpFramingError::InvalidJson)?;
    Ok(McpFrame { value, wire_bytes })
}

pub(super) async fn write_ndjson_message<W>(
    writer: &mut W,
    value: &Value,
) -> Result<usize, McpFramingError>
where
    W: AsyncWrite + Unpin,
{
    let mut encoded = BoundedFrameWriter::new(MCP_STDIO_FRAME_LIMIT_BYTES);
    let serialized = serde_json::to_writer(&mut encoded, value);
    if encoded.overflowed {
        return Err(McpFramingError::FrameTooLarge {
            limit_bytes: MCP_STDIO_FRAME_LIMIT_BYTES,
            observed_at_least_bytes: encoded.observed_at_least_bytes,
        });
    }
    serialized.map_err(McpFramingError::InvalidJson)?;
    let body = encoded.into_inner();
    writer
        .write_all(&body)
        .await
        .map_err(McpFramingError::Write)?;
    writer
        .write_all(b"\n")
        .await
        .map_err(McpFramingError::Write)?;
    writer.flush().await.map_err(McpFramingError::Write)?;
    Ok(body.len().saturating_add(1))
}

struct BoundedFrameWriter {
    bytes: Vec<u8>,
    limit_bytes: usize,
    overflowed: bool,
    observed_at_least_bytes: usize,
}

impl BoundedFrameWriter {
    fn new(limit_bytes: usize) -> Self {
        Self {
            bytes: Vec::with_capacity(8 * 1024),
            limit_bytes,
            overflowed: false,
            observed_at_least_bytes: 0,
        }
    }

    fn into_inner(self) -> Vec<u8> {
        self.bytes
    }
}

impl io::Write for BoundedFrameWriter {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        let observed = self.bytes.len().saturating_add(bytes.len());
        self.observed_at_least_bytes = self.observed_at_least_bytes.max(observed);
        if observed > self.limit_bytes {
            self.overflowed = true;
            return Err(io::Error::other("MCP stdio frame limit exceeded"));
        }
        self.bytes.extend_from_slice(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

#[cfg(test)]
#[path = "tests/framing_tests.rs"]
mod tests;
