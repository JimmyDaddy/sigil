use anyhow::{Result, anyhow};
use sigil_kernel::SseFrameBuffer;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OpenAiResponsesSseFrame {
    Event { event: String, data: String },
    Comment,
    Blank,
}

#[derive(Debug, Default)]
pub struct OpenAiResponsesSseDecoder {
    buffer: SseFrameBuffer,
}

impl OpenAiResponsesSseDecoder {
    #[cfg(test)]
    pub fn push(&mut self, raw: &str) -> Result<Vec<OpenAiResponsesSseFrame>> {
        self.buffer.push(raw, parse_sse_chunk)
    }

    pub fn push_bytes(&mut self, raw: &[u8]) -> Result<Vec<OpenAiResponsesSseFrame>> {
        self.buffer.push_bytes(raw, parse_sse_chunk)
    }

    pub fn finish(&mut self) -> Result<Vec<OpenAiResponsesSseFrame>> {
        self.buffer.finish(parse_sse_chunk)
    }
}

fn parse_sse_chunk(chunk: &str) -> Result<OpenAiResponsesSseFrame> {
    if chunk.trim().is_empty() {
        return Ok(OpenAiResponsesSseFrame::Blank);
    }

    let mut event = None;
    let mut data_lines = Vec::new();
    let mut saw_comment = false;
    for line in chunk.lines() {
        if line.starts_with(':') {
            saw_comment = true;
        } else if let Some(value) = line.strip_prefix("event:") {
            let value = value.trim();
            if value.is_empty() || event.replace(value.to_owned()).is_some() {
                return Err(anyhow!("invalid OpenAI Responses SSE event frame"));
            }
        } else if let Some(value) = line.strip_prefix("data:") {
            data_lines.push(value.trim_start().to_owned());
        }
    }
    match (event, data_lines.is_empty(), saw_comment) {
        (Some(event), false, _) => Ok(OpenAiResponsesSseFrame::Event {
            event,
            data: data_lines.join("\n"),
        }),
        (None, true, true) => Ok(OpenAiResponsesSseFrame::Comment),
        _ => Err(anyhow!("invalid OpenAI Responses SSE frame")),
    }
}

#[cfg(test)]
#[path = "tests/stream_tests.rs"]
mod tests;
