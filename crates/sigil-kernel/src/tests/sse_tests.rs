use super::*;

#[test]
fn frame_buffer_normalizes_crlf_across_push_boundaries() -> anyhow::Result<()> {
    let mut buffer = SseFrameBuffer::default();

    assert!(buffer.push("data: one\r", parse_chunk)?.is_empty());
    let frames = buffer.push("\n\r\n", parse_chunk)?;

    assert_eq!(frames, vec!["data: one".to_owned()]);
    Ok(())
}

#[test]
fn frame_buffer_normalizes_bare_cr_before_non_newline() -> anyhow::Result<()> {
    let mut buffer = SseFrameBuffer::default();

    assert!(buffer.push("data: one\r", parse_chunk)?.is_empty());
    let frames = buffer.push("id: ignored\n\n", parse_chunk)?;

    assert_eq!(frames, vec!["data: one\nid: ignored".to_owned()]);
    Ok(())
}

#[test]
fn frame_buffer_flushes_final_partial_frame() -> anyhow::Result<()> {
    let mut buffer = SseFrameBuffer::default();

    assert!(buffer.push("data: tail", parse_chunk)?.is_empty());
    let frames = buffer.finish(parse_chunk)?;

    assert_eq!(frames, vec!["data: tail".to_owned()]);
    assert!(buffer.finish(parse_chunk)?.is_empty());
    Ok(())
}

fn parse_chunk(chunk: &str) -> anyhow::Result<String> {
    Ok(chunk.to_owned())
}
