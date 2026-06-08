use anyhow::{Result, anyhow};

use crate::response::DeepSeekSseFrame;

#[derive(Debug, Default)]
pub struct DeepSeekSseDecoder {
    normalized_buffer: String,
    pending_cr: bool,
}

impl DeepSeekSseDecoder {
    pub fn push(&mut self, raw: &str) -> Result<Vec<DeepSeekSseFrame>> {
        self.append_normalized(raw);
        self.drain_complete_frames()
    }

    pub fn finish(&mut self) -> Result<Vec<DeepSeekSseFrame>> {
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

    fn drain_complete_frames(&mut self) -> Result<Vec<DeepSeekSseFrame>> {
        let mut frames = Vec::new();
        while let Some(separator_index) = self.normalized_buffer.find("\n\n") {
            let chunk = self.normalized_buffer[..separator_index].to_owned();
            self.normalized_buffer = self.normalized_buffer[separator_index + 2..].to_owned();
            frames.push(parse_sse_chunk(&chunk)?);
        }
        Ok(frames)
    }
}

#[cfg(test)]
pub fn parse_sse_frames(raw: &str) -> Result<Vec<DeepSeekSseFrame>> {
    let mut decoder = DeepSeekSseDecoder::default();
    let mut frames = decoder.push(raw)?;
    frames.extend(decoder.finish()?);
    Ok(frames)
}

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
mod tests {
    use anyhow::Result;

    use super::{DeepSeekSseDecoder, parse_sse_frames};
    use crate::response::DeepSeekSseFrame;

    #[test]
    fn decoder_buffers_split_json_events_until_frame_is_complete() -> Result<()> {
        let mut decoder = DeepSeekSseDecoder::default();

        let first = decoder.push("data: {\"choices\":[{\"delta\":{\"content\":\"hel")?;
        assert!(first.is_empty());

        let second = decoder.push("lo\"},\"finish_reason\":\"stop\"}]}\n\n")?;
        assert!(
            matches!(second.as_slice(), [DeepSeekSseFrame::Data(data)] if data.contains("\"hello\""))
        );
        assert!(decoder.finish()?.is_empty());
        Ok(())
    }

    #[test]
    fn decoder_merges_crlf_boundaries_split_across_chunks() -> Result<()> {
        let mut decoder = DeepSeekSseDecoder::default();

        assert!(decoder.push("data: {\"choices\":[]}\r")?.is_empty());
        let frames = decoder.push("\n\r\n")?;

        assert!(
            matches!(frames.as_slice(), [DeepSeekSseFrame::Data(data)] if data == "{\"choices\":[]}")
        );
        Ok(())
    }

    #[test]
    fn parse_sse_frames_dispatches_last_frame_at_eof() -> Result<()> {
        let frames = parse_sse_frames("data: {\"choices\":[]}")?;
        assert!(
            matches!(frames.as_slice(), [DeepSeekSseFrame::Data(data)] if data == "{\"choices\":[]}")
        );
        Ok(())
    }
}
