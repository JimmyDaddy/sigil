use anyhow::{Result, anyhow};
use sigil_kernel::SseFrameBuffer;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AnthropicSseFrame {
    Data(String),
    Comment,
    Blank,
}

#[derive(Debug, Default)]
pub struct AnthropicSseDecoder {
    buffer: SseFrameBuffer,
}

impl AnthropicSseDecoder {
    pub fn push(&mut self, raw: &str) -> Result<Vec<AnthropicSseFrame>> {
        self.buffer.push(raw, parse_sse_chunk)
    }

    pub fn finish(&mut self) -> Result<Vec<AnthropicSseFrame>> {
        self.buffer.finish(parse_sse_chunk)
    }
}

fn parse_sse_chunk(chunk: &str) -> Result<AnthropicSseFrame> {
    if chunk.trim().is_empty() {
        return Ok(AnthropicSseFrame::Blank);
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
        Ok(AnthropicSseFrame::Data(data_lines.join("\n")))
    } else if saw_comment {
        Ok(AnthropicSseFrame::Comment)
    } else {
        Err(anyhow!("invalid Anthropic SSE chunk: {chunk}"))
    }
}

#[cfg(test)]
#[path = "tests/stream_tests.rs"]
mod tests;
