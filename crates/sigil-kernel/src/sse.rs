use anyhow::{Result, anyhow};

/// Buffers SSE text and emits complete normalized frame chunks.
///
/// This helper owns only transport-level normalization: CR/LF handling, buffering split frames,
/// and flushing the final partial frame. Providers still parse each frame and retain their own
/// protocol-specific stop semantics.
#[derive(Debug, Default)]
pub struct SseFrameBuffer {
    normalized_buffer: String,
    pending_cr: bool,
    pending_utf8: Vec<u8>,
}

impl SseFrameBuffer {
    /// Appends already-decoded SSE text and emits complete frames.
    ///
    /// This compatibility API shares state with [`Self::push_bytes`], so callers may migrate to
    /// byte-oriented transport reads without changing frame parsing.
    ///
    /// # Errors
    ///
    /// Returns an error when the accumulated bytes are not valid UTF-8 or `parse_chunk` rejects a
    /// complete frame.
    pub fn push<T>(
        &mut self,
        raw: &str,
        mut parse_chunk: impl FnMut(&str) -> Result<T>,
    ) -> Result<Vec<T>> {
        self.push_bytes_inner(raw.as_bytes())?;
        self.drain_complete_frames(&mut parse_chunk)
    }

    /// Appends raw transport bytes and emits complete UTF-8 SSE frames.
    ///
    /// A trailing partial UTF-8 code point is retained until the next call. Truly invalid byte
    /// sequences fail immediately instead of being replaced or decoded chunk-by-chunk.
    ///
    /// # Errors
    ///
    /// Returns an error when the accumulated bytes contain invalid UTF-8 or `parse_chunk` rejects
    /// a complete frame.
    pub fn push_bytes<T>(
        &mut self,
        raw: &[u8],
        mut parse_chunk: impl FnMut(&str) -> Result<T>,
    ) -> Result<Vec<T>> {
        self.push_bytes_inner(raw)?;
        self.drain_complete_frames(&mut parse_chunk)
    }

    /// Flushes the final partial SSE frame.
    ///
    /// # Errors
    ///
    /// Returns an error when the stream ends with an incomplete UTF-8 code point or `parse_chunk`
    /// rejects the final frame.
    pub fn finish<T>(&mut self, mut parse_chunk: impl FnMut(&str) -> Result<T>) -> Result<Vec<T>> {
        if !self.pending_utf8.is_empty() {
            return Err(anyhow!(
                "incomplete UTF-8 SSE sequence at end of stream ({} buffered bytes)",
                self.pending_utf8.len()
            ));
        }
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

    fn push_bytes_inner(&mut self, raw: &[u8]) -> Result<()> {
        self.pending_utf8.extend_from_slice(raw);

        let valid_up_to = match std::str::from_utf8(&self.pending_utf8) {
            Ok(_) => self.pending_utf8.len(),
            Err(error) if error.error_len().is_none() => error.valid_up_to(),
            Err(error) => {
                return Err(anyhow!(
                    "invalid UTF-8 SSE byte sequence at offset {}",
                    error.valid_up_to()
                ));
            }
        };

        if valid_up_to == 0 {
            return Ok(());
        }

        let valid = std::str::from_utf8(&self.pending_utf8[..valid_up_to])
            .map_err(|error| anyhow!("failed to decode validated UTF-8 SSE bytes: {error}"))?
            .to_owned();
        self.pending_utf8.drain(..valid_up_to);
        self.append_normalized(&valid);
        Ok(())
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
