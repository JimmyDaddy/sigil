use anyhow::Result;

/// Buffers SSE text and emits complete normalized frame chunks.
///
/// This helper owns only transport-level normalization: CR/LF handling, buffering split frames,
/// and flushing the final partial frame. Providers still parse each frame and retain their own
/// protocol-specific stop semantics.
#[derive(Debug, Default)]
pub struct SseFrameBuffer {
    normalized_buffer: String,
    pending_cr: bool,
}

impl SseFrameBuffer {
    pub fn push<T>(
        &mut self,
        raw: &str,
        mut parse_chunk: impl FnMut(&str) -> Result<T>,
    ) -> Result<Vec<T>> {
        self.append_normalized(raw);
        self.drain_complete_frames(&mut parse_chunk)
    }

    pub fn finish<T>(&mut self, mut parse_chunk: impl FnMut(&str) -> Result<T>) -> Result<Vec<T>> {
        if self.pending_cr {
            self.normalized_buffer.push('\n');
            self.pending_cr = false;
        }
        if self.normalized_buffer.is_empty() {
            return Ok(Vec::new());
        }
        let chunk = std::mem::take(&mut self.normalized_buffer);
        Ok(vec![parse_chunk(&chunk)?])
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

    fn drain_complete_frames<T>(
        &mut self,
        parse_chunk: &mut impl FnMut(&str) -> Result<T>,
    ) -> Result<Vec<T>> {
        let mut frames = Vec::new();
        while let Some(separator_index) = self.normalized_buffer.find("\n\n") {
            let chunk = self.normalized_buffer[..separator_index].to_owned();
            self.normalized_buffer = self.normalized_buffer[separator_index + 2..].to_owned();
            frames.push(parse_chunk(&chunk)?);
        }
        Ok(frames)
    }
}

#[cfg(test)]
#[path = "tests/sse_tests.rs"]
mod tests;
