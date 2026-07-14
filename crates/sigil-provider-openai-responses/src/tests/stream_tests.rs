use anyhow::Result;

use super::{OpenAiResponsesSseDecoder, OpenAiResponsesSseFrame};

#[test]
fn decoder_requires_event_and_data_for_responses_events() -> Result<()> {
    let mut decoder = OpenAiResponsesSseDecoder::default();
    let frames = decoder.push(
        ":keepalive\n\n\
         event: response.output_text.delta\n\
         data: {\"delta\":\"hello\"}\n\n",
    )?;

    assert!(matches!(frames[0], OpenAiResponsesSseFrame::Comment));
    assert!(matches!(
        frames[1],
        OpenAiResponsesSseFrame::Event { ref event, ref data }
            if event == "response.output_text.delta" && data == "{\"delta\":\"hello\"}"
    ));
    Ok(())
}

#[test]
fn decoder_buffers_split_responses_event() -> Result<()> {
    let mut decoder = OpenAiResponsesSseDecoder::default();

    assert!(
        decoder
            .push("event: response.completed\ndata: {\"response\":")?
            .is_empty()
    );
    let frames = decoder.push("{}}\n\n")?;

    assert!(matches!(
        frames.as_slice(),
        [OpenAiResponsesSseFrame::Event { event, data }]
            if event == "response.completed" && data == "{\"response\":{}}"
    ));
    Ok(())
}

#[test]
fn decoder_rejects_data_without_an_event_type() {
    let mut decoder = OpenAiResponsesSseDecoder::default();

    let error = decoder
        .push("data: {\"delta\":\"hello\"}\n\n")
        .expect_err("Responses events must include their event type");

    assert!(
        error
            .to_string()
            .contains("invalid OpenAI Responses SSE frame")
    );
}
