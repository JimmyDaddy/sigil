use anyhow::Result;

use crate::response::DeepSeekSseFrame;

use super::DeepSeekSseDecoder;

pub(crate) fn parse_sse_frames(raw: &str) -> Result<Vec<DeepSeekSseFrame>> {
    let mut decoder = DeepSeekSseDecoder::default();
    let mut frames = decoder.push(raw)?;
    frames.extend(decoder.finish()?);
    Ok(frames)
}
