use anyhow::{Result, anyhow};
use sigil_kernel::SseFrameBuffer;

use crate::response::DeepSeekSseFrame;

#[derive(Debug, Default)]
pub struct DeepSeekSseDecoder {
    buffer: SseFrameBuffer,
}

impl DeepSeekSseDecoder {
    #[cfg(test)]
    pub fn push(&mut self, raw: &str) -> Result<Vec<DeepSeekSseFrame>> {
        self.buffer.push(raw, parse_sse_chunk)
    }

    pub fn push_bytes(&mut self, raw: &[u8]) -> Result<Vec<DeepSeekSseFrame>> {
        self.buffer.push_bytes(raw, parse_sse_chunk)
    }

    pub fn finish(&mut self) -> Result<Vec<DeepSeekSseFrame>> {
        self.buffer.finish(parse_sse_chunk)
    }
}

#[cfg(test)]
#[path = "tests/stream_test_support.rs"]
pub(crate) mod test_support;

fn parse_sse_chunk(chunk: &str) -> Result<DeepSeekSseFrame> {
    if chunk.trim().is_empty() {
        return Ok(DeepSeekSseFrame::Blank);
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
            Ok(DeepSeekSseFrame::Done)
        } else {
            Ok(DeepSeekSseFrame::Data(data))
        }
    } else if saw_comment {
        Ok(DeepSeekSseFrame::Comment)
    } else {
        Err(anyhow!("invalid SSE chunk: {chunk}"))
    }
}

#[cfg(test)]
#[path = "tests/stream_tests.rs"]
mod tests;
