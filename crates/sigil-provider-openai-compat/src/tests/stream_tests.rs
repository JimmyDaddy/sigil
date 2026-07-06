use anyhow::Result;

use super::{OpenAiSseDecoder, OpenAiSseFrame};

#[test]
fn decoder_handles_comments_done_blanks_and_multiline_data() -> Result<()> {
    let mut decoder = OpenAiSseDecoder::default();
    let frames = decoder.push(
        ":keepalive\n\n\
         data: {\"a\":1}\n\
         data: {\"b\":2}\n\n\
         data: [DONE]\n\n\
         \n\n",
    )?;

    assert!(matches!(frames[0], OpenAiSseFrame::Comment));
    assert!(matches!(
        frames[1],
        OpenAiSseFrame::Data(ref data) if data == "{\"a\":1}\n{\"b\":2}"
    ));
    assert!(matches!(frames[2], OpenAiSseFrame::Done));
    assert!(matches!(frames[3], OpenAiSseFrame::Blank));
    Ok(())
}

#[test]
fn decoder_normalizes_crlf_and_finishes_partial_frame() -> Result<()> {
    let mut decoder = OpenAiSseDecoder::default();

    assert!(decoder.push("data: {\"a\":").expect("push").is_empty());
    let frames = decoder.push("1}\r\n\r\n")?;
    let finished = decoder.finish()?;

    assert!(matches!(
        frames.as_slice(),
        [OpenAiSseFrame::Data(data)] if data == "{\"a\":1}"
    ));
    assert!(finished.is_empty());
    Ok(())
}

#[test]
fn decoder_keeps_split_frame_buffered_until_separator() -> Result<()> {
    let mut decoder = OpenAiSseDecoder::default();

    assert!(decoder.push("data: {\"a\":")?.is_empty());
    assert!(decoder.push("1}")?.is_empty());
    let frames = decoder.push("\n\n")?;

    assert!(matches!(
        frames.as_slice(),
        [OpenAiSseFrame::Data(data)] if data == "{\"a\":1}"
    ));
    Ok(())
}

#[test]
fn decoder_finish_flushes_pending_carriage_return_and_partial_data() -> Result<()> {
    let mut decoder = OpenAiSseDecoder::default();

    let pushed = decoder.push("data: {\"a\":1}\r")?;
    let finished = decoder.finish()?;

    assert!(pushed.is_empty());
    assert!(matches!(
        finished.as_slice(),
        [OpenAiSseFrame::Data(data)] if data == "{\"a\":1}"
    ));
    Ok(())
}

#[test]
fn decoder_normalizes_standalone_carriage_return_before_next_character() -> Result<()> {
    let mut decoder = OpenAiSseDecoder::default();

    let frames = decoder.push("data: a\rdata: b\n\n")?;

    assert!(matches!(
        frames.as_slice(),
        [OpenAiSseFrame::Data(data)] if data == "a\nb"
    ));
    Ok(())
}

#[test]
fn decoder_finish_on_empty_buffer_is_empty() -> Result<()> {
    let mut decoder = OpenAiSseDecoder::default();

    let finished = decoder.finish()?;

    assert!(finished.is_empty());
    Ok(())
}

#[test]
fn decoder_reports_invalid_frame_without_data_or_comment() {
    let mut decoder = OpenAiSseDecoder::default();

    let error = decoder
        .push("event: message\n\n")
        .expect_err("invalid frame should fail");

    assert!(error.to_string().contains("invalid SSE chunk"));
}
