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

#[test]
fn frame_buffer_decodes_multibyte_code_points_across_byte_pushes() -> anyhow::Result<()> {
    let mut buffer = SseFrameBuffer::default();
    let raw = "data: é你🙂\n\n".as_bytes();
    let boundaries = [7, 10, 14];
    let mut start = 0usize;
    let mut frames = Vec::new();

    for end in boundaries.into_iter().chain([raw.len()]) {
        frames.extend(buffer.push_bytes(&raw[start..end], parse_chunk)?);
        start = end;
    }

    assert_eq!(frames, vec!["data: é你🙂".to_owned()]);
    Ok(())
}

#[test]
fn frame_buffer_rejects_invalid_and_incomplete_utf8() -> anyhow::Result<()> {
    let mut invalid = SseFrameBuffer::default();
    let error = invalid
        .push_bytes(b"data: \xff\n\n", parse_chunk)
        .expect_err("invalid UTF-8 must fail");
    assert!(
        error
            .to_string()
            .contains("invalid UTF-8 SSE byte sequence")
    );

    let mut incomplete = SseFrameBuffer::default();
    assert!(
        incomplete
            .push_bytes(&[0xf0, 0x9f], parse_chunk)?
            .is_empty()
    );
    let error = incomplete
        .finish(parse_chunk)
        .expect_err("incomplete UTF-8 at EOF must fail");
    assert!(
        error
            .to_string()
            .contains("incomplete UTF-8 SSE sequence at end of stream")
    );
    Ok(())
}

fn parse_chunk(chunk: &str) -> anyhow::Result<String> {
    Ok(chunk.to_owned())
}
