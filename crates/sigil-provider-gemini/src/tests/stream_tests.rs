use super::*;

#[test]
fn sse_decoder_parses_data_done_and_comments_across_chunks() -> anyhow::Result<()> {
    let mut decoder = GeminiSseDecoder::default();

    let mut frames = decoder.push("data: {\"candidates\":[]}\r\n\r\n: keep")?;
    frames.extend(decoder.push("-alive\n\ndata: [DONE]\n\n")?);

    assert_eq!(
        frames,
        vec![
            GeminiSseFrame::Data(r#"{"candidates":[]}"#.to_owned()),
            GeminiSseFrame::Comment,
            GeminiSseFrame::Done
        ]
    );
    Ok(())
}

#[test]
fn sse_decoder_finishes_trailing_frame() -> anyhow::Result<()> {
    let mut decoder = GeminiSseDecoder::default();
    decoder.push("data: {\"candidates\":[]}")?;
    let frames = decoder.finish()?;

    assert_eq!(
        frames,
        vec![GeminiSseFrame::Data(r#"{"candidates":[]}"#.to_owned())]
    );
    Ok(())
}

#[test]
fn sse_decoder_handles_pending_carriage_return_and_invalid_chunks() -> anyhow::Result<()> {
    let mut decoder = GeminiSseDecoder::default();
    decoder.push("data: {\"candidates\":[]}\r")?;
    let frames = decoder.finish()?;
    assert_eq!(
        frames,
        vec![GeminiSseFrame::Data(r#"{"candidates":[]}"#.to_owned())]
    );

    let mut decoder = GeminiSseDecoder::default();
    let frames = decoder.push(": keep\ralive\n\n")?;
    assert_eq!(frames, vec![GeminiSseFrame::Comment]);

    let mut decoder = GeminiSseDecoder::default();
    let error = decoder
        .push("event: message\n\n")
        .expect_err("invalid chunk should fail");
    assert!(error.to_string().contains("invalid Gemini SSE chunk"));
    Ok(())
}
