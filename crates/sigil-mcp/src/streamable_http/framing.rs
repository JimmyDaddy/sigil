use super::*;

pub(super) async fn read_bounded_body(
    response: reqwest::Response,
    limits: McpStreamableHttpLimits,
    budget: &mut WebBudgetReservation,
) -> Result<Vec<u8>, McpStreamableHttpError> {
    tokio::time::timeout(limits.response_timeout, async {
        validate_response_headers(response.headers(), limits.max_header_bytes)?;
        if let Some(length) = single_header(response.headers(), CONTENT_LENGTH)? {
            let length = length
                .parse::<usize>()
                .map_err(|_| McpStreamableHttpError::HeaderLimitExceeded)?;
            if length > limits.max_body_bytes {
                return Err(McpStreamableHttpError::BodyLimitExceeded);
            }
        }
        let mut stream = response.bytes_stream();
        let mut body = Vec::new();
        while let Some(chunk) = stream.next().await {
            let chunk = chunk.map_err(|_| McpStreamableHttpError::Transport)?;
            budget
                .charge_chunk(WebBudgetByteKind::Wire, chunk.len() as u64)
                .and_then(|()| budget.charge_chunk(WebBudgetByteKind::Decoded, chunk.len() as u64))
                .map_err(|_| McpStreamableHttpError::BudgetExhausted)?;
            if body.len().saturating_add(chunk.len()) > limits.max_body_bytes {
                return Err(McpStreamableHttpError::BodyLimitExceeded);
            }
            body.extend_from_slice(&chunk);
        }
        Ok(body)
    })
    .await
    .map_err(|_| McpStreamableHttpError::Timeout)?
}

fn validate_response_headers(
    headers: &HeaderMap,
    limit: usize,
) -> Result<(), McpStreamableHttpError> {
    let total = headers.iter().try_fold(0usize, |total, (name, value)| {
        total
            .checked_add(name.as_str().len())
            .and_then(|total| total.checked_add(value.as_bytes().len()))
            .ok_or(McpStreamableHttpError::HeaderLimitExceeded)
    })?;
    if total > limit {
        Err(McpStreamableHttpError::HeaderLimitExceeded)
    } else {
        Ok(())
    }
}

pub(super) fn single_header(
    headers: &HeaderMap,
    name: impl reqwest::header::AsHeaderName,
) -> Result<Option<String>, McpStreamableHttpError> {
    let values = headers.get_all(name);
    if values.iter().count() > 1 {
        return Err(McpStreamableHttpError::HeaderLimitExceeded);
    }
    values
        .iter()
        .next()
        .map(|value| {
            value
                .to_str()
                .map(str::to_owned)
                .map_err(|_| McpStreamableHttpError::HeaderLimitExceeded)
        })
        .transpose()
}

pub(super) fn matches_content_type(
    value: &str,
    expected: &str,
) -> Result<bool, McpStreamableHttpError> {
    if value.len() > 1024 || !value.is_ascii() || value.chars().any(char::is_control) {
        return Err(McpStreamableHttpError::UnexpectedContentType);
    }
    let mut parts = value.split(';');
    let essence = parts.next().unwrap_or_default().trim();
    if !essence.eq_ignore_ascii_case(expected) {
        return Ok(false);
    }
    for parameter in parts {
        let (name, raw_value) = parameter
            .trim()
            .split_once('=')
            .ok_or(McpStreamableHttpError::UnexpectedContentType)?;
        let name = name.trim();
        let raw_value = raw_value.trim();
        if name.is_empty()
            || !name
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
            || raw_value.is_empty()
            || (raw_value.starts_with('"') != raw_value.ends_with('"'))
        {
            return Err(McpStreamableHttpError::UnexpectedContentType);
        }
    }
    Ok(true)
}

pub(super) fn parse_sse_response(
    body: &[u8],
    expected_id: u64,
    limits: McpStreamableHttpLimits,
) -> Result<(Value, Vec<Value>), McpStreamableHttpError> {
    let messages = parse_sse_messages(body, limits)?;
    let mut matched = None;
    let mut inbound = Vec::new();
    for value in messages {
        if value.get("jsonrpc").and_then(Value::as_str) != Some("2.0") {
            return Err(McpStreamableHttpError::MalformedEnvelope);
        }
        if value.get("method").and_then(Value::as_str).is_some() {
            inbound.push(value);
        } else if value.get("id").and_then(Value::as_u64) == Some(expected_id) {
            if matched.replace(value).is_some() {
                return Err(McpStreamableHttpError::ResponseIdMismatch);
            }
        } else {
            return Err(McpStreamableHttpError::ResponseIdMismatch);
        }
    }
    matched
        .map(|response| (response, inbound))
        .ok_or(McpStreamableHttpError::ResponseIdMismatch)
}

pub(super) fn parse_sse_messages(
    body: &[u8],
    limits: McpStreamableHttpLimits,
) -> Result<Vec<Value>, McpStreamableHttpError> {
    let text = std::str::from_utf8(body).map_err(|_| McpStreamableHttpError::MalformedEnvelope)?;
    let mut data = String::new();
    let mut events = 0usize;
    let mut messages = Vec::new();
    for line in text.split_inclusive('\n') {
        if line.len() > limits.max_sse_line_bytes {
            return Err(McpStreamableHttpError::SseLimitExceeded);
        }
        let line = line.trim_end_matches(['\r', '\n']);
        if line.is_empty() {
            finish_sse_event(&mut data, &mut events, &mut messages, limits)?;
            continue;
        }
        if let Some(value) = line.strip_prefix("data:") {
            let value = value.strip_prefix(' ').unwrap_or(value);
            if data.len().saturating_add(value.len()).saturating_add(1) > limits.max_sse_event_bytes
            {
                return Err(McpStreamableHttpError::SseLimitExceeded);
            }
            data.push_str(value);
            data.push('\n');
        } else if !line.starts_with(':') && !line.starts_with("event:") && !line.starts_with("id:")
        {
            return Err(McpStreamableHttpError::MalformedEnvelope);
        }
    }
    finish_sse_event(&mut data, &mut events, &mut messages, limits)?;
    Ok(messages)
}

fn finish_sse_event(
    data: &mut String,
    events: &mut usize,
    messages: &mut Vec<Value>,
    limits: McpStreamableHttpLimits,
) -> Result<(), McpStreamableHttpError> {
    if data.is_empty() {
        return Ok(());
    }
    *events = events.saturating_add(1);
    if *events > limits.max_sse_events || data.len() > limits.max_sse_event_bytes {
        return Err(McpStreamableHttpError::SseLimitExceeded);
    }
    let value: Value = serde_json::from_str(data.trim_end_matches('\n'))
        .map_err(|_| McpStreamableHttpError::MalformedEnvelope)?;
    messages.push(value);
    data.clear();
    Ok(())
}

pub(super) fn validate_response_envelope(
    value: &Value,
    expected_id: u64,
) -> Result<(), McpStreamableHttpError> {
    let object = value
        .as_object()
        .ok_or(McpStreamableHttpError::MalformedEnvelope)?;
    if object.get("jsonrpc").and_then(Value::as_str) != Some("2.0") {
        return Err(McpStreamableHttpError::MalformedEnvelope);
    }
    if object.get("id").and_then(Value::as_u64) != Some(expected_id) {
        return Err(McpStreamableHttpError::ResponseIdMismatch);
    }
    if object.contains_key("result") == object.contains_key("error") {
        return Err(McpStreamableHttpError::MalformedEnvelope);
    }
    Ok(())
}

pub(super) fn rpc_result(
    response: &RpcResponse,
    expected_id: u64,
) -> Result<&Value, McpStreamableHttpError> {
    if response.expected_id != expected_id {
        return Err(McpStreamableHttpError::ResponseIdMismatch);
    }
    if let Some(error) = response.value.get("error") {
        let code = error
            .get("code")
            .and_then(Value::as_i64)
            .ok_or(McpStreamableHttpError::MalformedEnvelope)?;
        if code == -32042 {
            return Err(McpStreamableHttpError::UrlElicitationUnsupported);
        }
        return Err(McpStreamableHttpError::JsonRpcError { code });
    }
    response
        .value
        .get("result")
        .ok_or(McpStreamableHttpError::MalformedEnvelope)
}
