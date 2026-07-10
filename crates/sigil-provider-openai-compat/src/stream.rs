use anyhow::{Result, anyhow};
use sigil_kernel::SseFrameBuffer;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OpenAiSseFrame {
    Data(String),
    Done,
    Comment,
    Blank,
}

#[derive(Debug, Default)]
pub struct OpenAiSseDecoder {
    buffer: SseFrameBuffer,
}

impl OpenAiSseDecoder {
    #[cfg(test)]
    pub fn push(&mut self, raw: &str) -> Result<Vec<OpenAiSseFrame>> {
        self.buffer.push(raw, parse_sse_chunk)
    }

    pub fn push_bytes(&mut self, raw: &[u8]) -> Result<Vec<OpenAiSseFrame>> {
        self.buffer.push_bytes(raw, parse_sse_chunk)
    }

    pub fn finish(&mut self) -> Result<Vec<OpenAiSseFrame>> {
        self.buffer.finish(parse_sse_chunk)
    }
}

fn parse_sse_chunk(chunk: &str) -> Result<OpenAiSseFrame> {
    if chunk.trim().is_empty() {
        return Ok(OpenAiSseFrame::Blank);
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
            Ok(OpenAiSseFrame::Done)
        } else {
            Ok(OpenAiSseFrame::Data(data))
        }
    } else if saw_comment {
        Ok(OpenAiSseFrame::Comment)
    } else {
        Err(anyhow!("invalid SSE chunk: {chunk}"))
    }
}

#[cfg(test)]
#[path = "tests/stream_tests.rs"]
mod tests;
