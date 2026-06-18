use super::*;

#[test]
fn sse_decoder_parses_event_data_and_comments_across_chunks() -> anyhow::Result<()> {
    let mut decoder = AnthropicSseDecoder::default();

    let mut frames = decoder.push("event: ping\r\ndata: {\"type\":\"ping\"}\r\n\r\n: keep")?;
    frames.extend(decoder.push("-alive\n\n")?);

    assert_eq!(
        frames,
        vec![
            AnthropicSseFrame::Data(r#"{"type":"ping"}"#.to_owned()),
            AnthropicSseFrame::Comment
        ]
    );
    Ok(())
}

#[test]
fn sse_decoder_finishes_trailing_frame() -> anyhow::Result<()> {
    let mut decoder = AnthropicSseDecoder::default();

    let frames = decoder.finish()?;
    assert!(frames.is_empty());

    let mut decoder = AnthropicSseDecoder::default();
    decoder.push("event: message_stop\ndata: {\"type\":\"message_stop\"}")?;
    let frames = decoder.finish()?;

    assert_eq!(
        frames,
        vec![AnthropicSseFrame::Data(
            r#"{"type":"message_stop"}"#.to_owned()
        )]
    );
    Ok(())
}

#[test]
fn sse_decoder_handles_pending_carriage_return_and_invalid_chunks() -> anyhow::Result<()> {
    let mut decoder = AnthropicSseDecoder::default();
    decoder.push("data: {\"type\":\"ping\"}\r")?;
    let frames = decoder.finish()?;
    assert_eq!(
        frames,
        vec![AnthropicSseFrame::Data(r#"{"type":"ping"}"#.to_owned())]
    );

    let mut decoder = AnthropicSseDecoder::default();
    let frames = decoder.push(": keep\ralive\n\n")?;
    assert_eq!(frames, vec![AnthropicSseFrame::Comment]);

    let mut decoder = AnthropicSseDecoder::default();
    let error = decoder
        .push("event: message_stop\n\n")
        .expect_err("invalid chunk should fail");
    assert!(error.to_string().contains("invalid Anthropic SSE chunk"));
    Ok(())
}
