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

#[test]
fn parse_sse_frames_joins_multiline_data_and_done() -> Result<()> {
    let frames = parse_sse_frames("data: first\ndata: second\n\ndata: [DONE]\n\n")?;
    assert!(matches!(
        frames.as_slice(),
        [DeepSeekSseFrame::Data(data), DeepSeekSseFrame::Done] if data == "first\nsecond"
    ));
    Ok(())
}

#[test]
fn decoder_finish_flushes_pending_carriage_return_as_blank_frame() -> Result<()> {
    let mut decoder = DeepSeekSseDecoder::default();
    assert!(decoder.push("\r")?.is_empty());
    let frames = decoder.finish()?;
    assert!(matches!(frames.as_slice(), [DeepSeekSseFrame::Blank]));
    Ok(())
}

#[test]
fn parse_sse_frames_errors_for_invalid_chunk() {
    let error = parse_sse_frames("event: ping\n\n").expect_err("invalid frame should fail");
    assert!(error.to_string().contains("invalid SSE chunk"));
}
