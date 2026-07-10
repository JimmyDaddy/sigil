use anyhow::{Result, anyhow};
use sigil_kernel::SseFrameBuffer;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GeminiSseFrame {
    Data(String),
    Done,
    Comment,
    Blank,
}

#[derive(Debug, Default)]
pub struct GeminiSseDecoder {
    buffer: SseFrameBuffer,
}

impl GeminiSseDecoder {
    #[cfg(test)]
    pub fn push(&mut self, raw: &str) -> Result<Vec<GeminiSseFrame>> {
        self.buffer.push(raw, parse_sse_chunk)
    }

    pub fn push_bytes(&mut self, raw: &[u8]) -> Result<Vec<GeminiSseFrame>> {
        self.buffer.push_bytes(raw, parse_sse_chunk)
    }

    pub fn finish(&mut self) -> Result<Vec<GeminiSseFrame>> {
        self.buffer.finish(parse_sse_chunk)
    }
}

fn parse_sse_chunk(chunk: &str) -> Result<GeminiSseFrame> {
    if chunk.trim().is_empty() {
        return Ok(GeminiSseFrame::Blank);
    }

    let mut data_lines = Vec::new();
    let mut saw_comment = false;
    for line in chunk.lines() {
        if line.starts_with(':') {
            saw_comment = true;
            continue;
        }
        if let Some(rest) = line.strip_prefix("data:") {
            data_lines.push(rest.trim_start().to_owned());
        }
    }
    if !data_lines.is_empty() {
        let data = data_lines.join("\n");
        if data == "[DONE]" {
            Ok(GeminiSseFrame::Done)
        } else {
            Ok(GeminiSseFrame::Data(data))
        }
    } else if saw_comment {
        Ok(GeminiSseFrame::Comment)
    } else {
        Err(anyhow!("invalid Gemini SSE chunk: {chunk}"))
    }
}

#[cfg(test)]
#[path = "tests/stream_tests.rs"]
mod tests;
