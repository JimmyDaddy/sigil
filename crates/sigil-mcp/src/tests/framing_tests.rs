use std::io::Cursor;

use anyhow::Result;
use serde_json::json;
use tokio::io::{AsyncReadExt, AsyncWriteExt, BufReader};

use super::{
    MCP_STDIO_FRAME_LIMIT_BYTES, McpFramingError, read_ndjson_message, write_ndjson_message,
};

#[tokio::test]
async fn ndjson_codec_round_trips_one_bounded_message() -> Result<()> {
    let (mut writer, reader) = tokio::io::duplex(1024);
    let value = json!({"jsonrpc":"2.0","id":1,"result":{"ok":true}});
    let written = write_ndjson_message(&mut writer, &value).await?;
    let frame = read_ndjson_message(&mut BufReader::new(reader)).await?;
    assert_eq!(frame.value, value);
    assert_eq!(frame.wire_bytes, written);
    Ok(())
}

#[tokio::test]
async fn ndjson_reader_requires_newline_and_valid_utf8_json() {
    let mut missing_newline = BufReader::new(Cursor::new(br#"{"jsonrpc":"2.0"}"#.to_vec()));
    let error = read_ndjson_message(&mut missing_newline)
        .await
        .expect_err("EOF before newline must fail");
    assert!(matches!(error, McpFramingError::MissingNewline { .. }));

    let mut invalid_utf8 = BufReader::new(Cursor::new(vec![0xff, b'\n']));
    let error = read_ndjson_message(&mut invalid_utf8)
        .await
        .expect_err("invalid UTF-8 must fail");
    assert!(matches!(error, McpFramingError::InvalidUtf8(_)));

    let mut invalid_json = BufReader::new(Cursor::new(b"not-json\n".to_vec()));
    let error = read_ndjson_message(&mut invalid_json)
        .await
        .expect_err("invalid JSON must fail");
    assert!(matches!(error, McpFramingError::InvalidJson(_)));
}

#[tokio::test]
async fn ndjson_reader_accepts_exact_cap_and_rejects_cap_plus_one() -> Result<()> {
    let prefix = br#"{"value":""#;
    let suffix = br#""}"#;
    let payload_bytes = MCP_STDIO_FRAME_LIMIT_BYTES - prefix.len() - suffix.len();
    let mut exact = Vec::with_capacity(MCP_STDIO_FRAME_LIMIT_BYTES + 1);
    exact.extend_from_slice(prefix);
    exact.extend(std::iter::repeat_n(b'x', payload_bytes));
    exact.extend_from_slice(suffix);
    exact.push(b'\n');
    let frame = read_ndjson_message(&mut BufReader::new(Cursor::new(exact))).await?;
    assert_eq!(frame.wire_bytes, MCP_STDIO_FRAME_LIMIT_BYTES + 1);

    let mut oversized = vec![b'x'; MCP_STDIO_FRAME_LIMIT_BYTES + 1];
    oversized.push(b'\n');
    let error = read_ndjson_message(&mut BufReader::new(Cursor::new(oversized)))
        .await
        .expect_err("cap plus one must fail before JSON decoding");
    assert!(matches!(error, McpFramingError::FrameTooLarge { .. }));
    Ok(())
}

#[tokio::test]
async fn ndjson_reader_rejects_cap_plus_one_without_waiting_for_newline_or_eof() -> Result<()> {
    let mut eof_reader = BufReader::new(Cursor::new(vec![b'x'; MCP_STDIO_FRAME_LIMIT_BYTES + 1]));
    let error = read_ndjson_message(&mut eof_reader)
        .await
        .expect_err("cap plus one non-CR byte must fail before EOF classification");
    assert!(matches!(error, McpFramingError::FrameTooLarge { .. }));

    let (mut writer, reader) = tokio::io::duplex(MCP_STDIO_FRAME_LIMIT_BYTES + 1);
    writer
        .write_all(&vec![b'x'; MCP_STDIO_FRAME_LIMIT_BYTES + 1])
        .await?;
    let outcome = tokio::time::timeout(
        std::time::Duration::from_secs(1),
        read_ndjson_message(&mut BufReader::new(reader)),
    )
    .await
    .expect("reader must reject oversized pending frame without waiting for writer closure")
    .expect_err("cap plus one non-CR byte must fail");
    assert!(matches!(outcome, McpFramingError::FrameTooLarge { .. }));
    Ok(())
}

#[tokio::test]
async fn ndjson_reader_only_allows_cap_plus_one_for_immediate_crlf() -> Result<()> {
    let prefix = br#"{"value":""#;
    let suffix = br#""}"#;
    let payload_bytes = MCP_STDIO_FRAME_LIMIT_BYTES - prefix.len() - suffix.len();
    let mut frame = Vec::with_capacity(MCP_STDIO_FRAME_LIMIT_BYTES + 2);
    frame.extend_from_slice(prefix);
    frame.extend(std::iter::repeat_n(b'x', payload_bytes));
    frame.extend_from_slice(suffix);
    frame.extend_from_slice(b"\r\n");

    let decoded = read_ndjson_message(&mut BufReader::new(Cursor::new(frame))).await?;
    assert_eq!(decoded.wire_bytes, MCP_STDIO_FRAME_LIMIT_BYTES + 2);
    Ok(())
}

#[tokio::test]
async fn outbound_oversize_writes_zero_bytes() -> Result<()> {
    let (mut client, mut server) = tokio::io::duplex(64);
    let value = json!({"value": "x".repeat(MCP_STDIO_FRAME_LIMIT_BYTES)});
    let error = write_ndjson_message(&mut client, &value)
        .await
        .expect_err("oversized outbound frame must fail");
    assert!(matches!(error, McpFramingError::FrameTooLarge { .. }));
    drop(client);
    let mut observed = Vec::new();
    server.read_to_end(&mut observed).await?;
    assert!(observed.is_empty());
    Ok(())
}

#[tokio::test]
async fn ndjson_reader_accepts_crlf_delimiter_for_stdio_compatibility() -> Result<()> {
    let mut reader = BufReader::new(Cursor::new(b"{\"jsonrpc\":\"2.0\",\"id\":1}\r\n".to_vec()));
    let frame = read_ndjson_message(&mut reader).await?;
    assert_eq!(frame.value["id"], 1);
    Ok(())
}
