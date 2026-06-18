use anyhow::{Result, anyhow};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AnthropicSseFrame {
    Data(String),
    Comment,
    Blank,
}

#[derive(Debug, Default)]
pub struct AnthropicSseDecoder {
    normalized_buffer: String,
    pending_cr: bool,
}

impl AnthropicSseDecoder {
    pub fn push(&mut self, raw: &str) -> Result<Vec<AnthropicSseFrame>> {
        self.append_normalized(raw);
        self.drain_complete_frames()
    }

    pub fn finish(&mut self) -> Result<Vec<AnthropicSseFrame>> {
        if self.pending_cr {
            self.normalized_buffer.push('\n');
            self.pending_cr = false;
        }
        if self.normalized_buffer.is_empty() {
            return Ok(Vec::new());
        }
        let chunk = std::mem::take(&mut self.normalized_buffer);
        Ok(vec![parse_sse_chunk(&chunk)?])
    }

    fn append_normalized(&mut self, raw: &str) {
        for character in raw.chars() {
            if self.pending_cr {
                if character == '\n' {
                    self.normalized_buffer.push('\n');
                    self.pending_cr = false;
                    continue;
                }
                self.normalized_buffer.push('\n');
                self.pending_cr = false;
            }

            if character == '\r' {
                self.pending_cr = true;
            } else {
                self.normalized_buffer.push(character);
            }
        }
    }

    fn drain_complete_frames(&mut self) -> Result<Vec<AnthropicSseFrame>> {
        let mut frames = Vec::new();
        while let Some(separator_index) = self.normalized_buffer.find("\n\n") {
            let chunk = self.normalized_buffer[..separator_index].to_owned();
            self.normalized_buffer = self.normalized_buffer[separator_index + 2..].to_owned();
            frames.push(parse_sse_chunk(&chunk)?);
        }
        Ok(frames)
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
