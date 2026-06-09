use anyhow::Result;

use super::{DeepSeekSseDecoder, test_support::parse_sse_frames};
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
