use super::*;

pub(super) fn resolve_headers(
    config: &McpStreamableHttpHeaderConfig,
    environment: &dyn McpStreamableHttpHeaderEnvironment,
    endpoint: &Url,
) -> Result<ResolvedHeaders, McpStreamableHttpError> {
    if config.literal.len().saturating_add(config.from_env.len())
        + usize::from(config.bearer_token_env_var.is_some())
        > MAX_CUSTOM_HEADERS
    {
        return Err(McpStreamableHttpError::ConfigurationInvalid);
    }
    let mut values = BTreeMap::<String, (HeaderName, SecretString, &'static str, String)>::new();
    for (raw_name, value) in &config.literal {
        let name = parse_custom_header_name(raw_name)?;
        if is_sensitive_header(name.as_str()) {
            return Err(McpStreamableHttpError::ConfigurationInvalid);
        }
        validate_header_value(value)?;
        insert_header(
            &mut values,
            name,
            SecretString::new(value.clone()),
            "literal",
            String::new(),
        )?;
    }
    for (raw_name, env_name) in &config.from_env {
        let name = parse_custom_header_name(raw_name)?;
        validate_env_name(env_name)?;
        let value = environment.resolve(env_name).ok_or_else(|| {
            if is_sensitive_header(name.as_str()) {
                McpStreamableHttpError::AuthenticationRequired
            } else {
                McpStreamableHttpError::ConfigurationInvalid
            }
        })?;
        validate_header_value(value.expose_secret())?;
        insert_header(&mut values, name, value, "env", env_name.clone())?;
    }
    if let Some(env_name) = config.bearer_token_env_var.as_ref() {
        validate_env_name(env_name)?;
        let token = environment
            .resolve(env_name)
            .ok_or(McpStreamableHttpError::AuthenticationRequired)?;
        validate_header_value(token.expose_secret())?;
        let bearer = SecretString::new(format!("Bearer {}", token.expose_secret()));
        insert_header(
            &mut values,
            AUTHORIZATION,
            bearer,
            "bearer_env",
            env_name.clone(),
        )?;
    }
    let total = values
        .iter()
        .try_fold(0usize, |total, (name, (_, value, _, _))| {
            total
                .checked_add(name.len())
                .and_then(|total| total.checked_add(value.expose_secret().len()))
                .ok_or(McpStreamableHttpError::ConfigurationInvalid)
        })?;
    if total > MAX_HEADER_TOTAL_BYTES {
        return Err(McpStreamableHttpError::ConfigurationInvalid);
    }
    let has_static_credential = values.keys().any(|name| is_sensitive_header(name.as_str()));
    if has_static_credential && endpoint.scheme() != "https" {
        return Err(McpStreamableHttpError::ConfigurationInvalid);
    }
    let key = HEADER_FINGERPRINT_KEY.get_or_init(|| {
        let mut key = [0u8; 32];
        let random = Uuid::new_v4();
        key[..16].copy_from_slice(random.as_bytes());
        let second = Uuid::new_v4();
        key[16..].copy_from_slice(second.as_bytes());
        key
    });
    let mut material = Vec::new();
    for (name, (_, value, source, source_name)) in &values {
        material.extend_from_slice(name.as_bytes());
        material.push(0);
        material.extend_from_slice(source.as_bytes());
        material.push(0);
        material.extend_from_slice(source_name.as_bytes());
        material.push(0);
        let mut mac = Hmac::<Sha256>::new_from_slice(key)
            .map_err(|_| McpStreamableHttpError::ConfigurationInvalid)?;
        mac.update(value.expose_secret().as_bytes());
        material.extend_from_slice(&mac.finalize().into_bytes());
    }
    let mut mac = Hmac::<Sha256>::new_from_slice(key)
        .map_err(|_| McpStreamableHttpError::ConfigurationInvalid)?;
    mac.update(&material);
    let live_fingerprint = format!("hmac-sha256:{:x}", mac.finalize().into_bytes());
    Ok(ResolvedHeaders {
        values: values
            .into_values()
            .map(|(name, value, _, _)| (name, value))
            .collect(),
        has_static_credential,
        live_fingerprint,
    })
}

fn insert_header(
    values: &mut BTreeMap<String, (HeaderName, SecretString, &'static str, String)>,
    name: HeaderName,
    value: SecretString,
    source: &'static str,
    source_name: String,
) -> Result<(), McpStreamableHttpError> {
    let key = name.as_str().to_ascii_lowercase();
    if values
        .insert(key, (name, value, source, source_name))
        .is_some()
    {
        return Err(McpStreamableHttpError::ConfigurationInvalid);
    }
    Ok(())
}

fn parse_custom_header_name(value: &str) -> Result<HeaderName, McpStreamableHttpError> {
    if value.is_empty() || value.len() > MAX_HEADER_NAME_BYTES {
        return Err(McpStreamableHttpError::ConfigurationInvalid);
    }
    let name = HeaderName::from_bytes(value.as_bytes())
        .map_err(|_| McpStreamableHttpError::ConfigurationInvalid)?;
    if is_owned_header(name.as_str()) {
        return Err(McpStreamableHttpError::ConfigurationInvalid);
    }
    Ok(name)
}

fn is_owned_header(name: &str) -> bool {
    matches!(
        name.to_ascii_lowercase().as_str(),
        "host"
            | "content-length"
            | "transfer-encoding"
            | "connection"
            | "accept"
            | "content-type"
            | "origin"
            | "user-agent"
            | "mcp-protocol-version"
            | "mcp-session-id"
            | "last-event-id"
            | "cookie"
            | "set-cookie"
            | "proxy-authorization"
            | "referer"
    )
}

fn is_sensitive_header(name: &str) -> bool {
    let normalized = name
        .chars()
        .filter(|character| character.is_ascii_alphanumeric())
        .collect::<String>()
        .to_ascii_lowercase();
    normalized == "authorization"
        || ["apikey", "token", "secret", "credential"]
            .iter()
            .any(|marker| normalized.contains(marker))
}

fn validate_header_value(value: &str) -> Result<(), McpStreamableHttpError> {
    if value.is_empty()
        || value.len() > MAX_HEADER_VALUE_BYTES
        || value
            .bytes()
            .any(|byte| byte == b'\r' || byte == b'\n' || byte == 0 || byte < 0x20)
        || HeaderValue::from_str(value).is_err()
    {
        return Err(McpStreamableHttpError::ConfigurationInvalid);
    }
    Ok(())
}

fn validate_env_name(value: &str) -> Result<(), McpStreamableHttpError> {
    let valid = !value.is_empty()
        && value.len() <= 128
        && value.bytes().enumerate().all(|(index, byte)| {
            byte == b'_' || byte.is_ascii_alphanumeric() && (index > 0 || !byte.is_ascii_digit())
        });
    if valid {
        Ok(())
    } else {
        Err(McpStreamableHttpError::ConfigurationInvalid)
    }
}

pub(super) fn normalize_status(
    status: StatusCode,
    headers: &HeaderMap,
    static_credential_sent: bool,
    session_sent: bool,
) -> Result<(), McpStreamableHttpError> {
    if status.is_redirection() {
        return Err(McpStreamableHttpError::UnexpectedHttpStatus {
            status: status.as_u16(),
        });
    }
    match status {
        StatusCode::UNAUTHORIZED => classify_unauthorized(headers, static_credential_sent),
        StatusCode::FORBIDDEN => Err(McpStreamableHttpError::AccessDenied),
        StatusCode::TOO_MANY_REQUESTS => Err(McpStreamableHttpError::RateLimited),
        StatusCode::NOT_FOUND if session_sent => Err(McpStreamableHttpError::SessionExpired),
        status if status.is_server_error() => Err(McpStreamableHttpError::ServiceUnavailable),
        _ => Ok(()),
    }
}

pub(super) fn classify_unauthorized(
    headers: &HeaderMap,
    static_credential_sent: bool,
) -> Result<(), McpStreamableHttpError> {
    let challenges = headers.get_all(WWW_AUTHENTICATE);
    if challenges.iter().count() > 1 {
        return Err(McpStreamableHttpError::InvalidAuthenticationChallenge);
    }
    if let Some(challenge) = challenges.iter().next() {
        let challenge = challenge
            .to_str()
            .map_err(|_| McpStreamableHttpError::InvalidAuthenticationChallenge)?;
        if challenge.len() > MAX_AUTH_CHALLENGE_BYTES || challenge.chars().any(char::is_control) {
            return Err(McpStreamableHttpError::InvalidAuthenticationChallenge);
        }
        if let Some((scheme, parameters)) = challenge.split_once(char::is_whitespace)
            && scheme.eq_ignore_ascii_case("bearer")
        {
            let parsed = parse_auth_parameters(parameters)?;
            let oauth_keys = [
                "resource_metadata",
                "resource_metadata_url",
                "authorization_uri",
            ];
            let oauth_values = oauth_keys
                .iter()
                .filter_map(|key| parsed.get(*key))
                .collect::<Vec<_>>();
            if oauth_values.len() > 1 {
                return Err(McpStreamableHttpError::InvalidAuthenticationChallenge);
            }
            if let Some(value) = oauth_values.first() {
                let metadata = Url::parse(value)
                    .map_err(|_| McpStreamableHttpError::InvalidAuthenticationChallenge)?;
                if metadata.scheme() != "https"
                    || metadata.host_str().is_none()
                    || !metadata.username().is_empty()
                    || metadata.password().is_some()
                    || metadata.fragment().is_some()
                {
                    return Err(McpStreamableHttpError::InvalidAuthenticationChallenge);
                }
                return Err(McpStreamableHttpError::OAuthUnsupported);
            }
        }
    }
    Err(if static_credential_sent {
        McpStreamableHttpError::AuthenticationFailed
    } else {
        McpStreamableHttpError::AuthenticationRequired
    })
}

fn parse_auth_parameters(input: &str) -> Result<BTreeMap<String, String>, McpStreamableHttpError> {
    let bytes = input.as_bytes();
    let mut index = 0usize;
    let mut parsed = BTreeMap::new();
    while index < bytes.len() {
        while index < bytes.len() && (bytes[index] == b' ' || bytes[index] == b',') {
            index += 1;
        }
        let key_start = index;
        while index < bytes.len()
            && (bytes[index].is_ascii_alphanumeric() || matches!(bytes[index], b'-' | b'_'))
        {
            index += 1;
        }
        if key_start == index {
            return Err(McpStreamableHttpError::InvalidAuthenticationChallenge);
        }
        let key = input[key_start..index].to_ascii_lowercase();
        while index < bytes.len() && bytes[index] == b' ' {
            index += 1;
        }
        if bytes.get(index).copied() != Some(b'=') {
            return Err(McpStreamableHttpError::InvalidAuthenticationChallenge);
        }
        index += 1;
        while index < bytes.len() && bytes[index] == b' ' {
            index += 1;
        }
        if bytes.get(index).copied() != Some(b'"') {
            return Err(McpStreamableHttpError::InvalidAuthenticationChallenge);
        }
        index += 1;
        let mut value = String::new();
        let mut closed = false;
        while index < bytes.len() {
            match bytes[index] {
                b'"' => {
                    index += 1;
                    closed = true;
                    break;
                }
                b'\\' => {
                    index += 1;
                    let escaped = bytes
                        .get(index)
                        .copied()
                        .filter(|byte| matches!(byte, b'"' | b'\\'))
                        .ok_or(McpStreamableHttpError::InvalidAuthenticationChallenge)?;
                    value.push(char::from(escaped));
                    index += 1;
                }
                byte if byte.is_ascii_control() => {
                    return Err(McpStreamableHttpError::InvalidAuthenticationChallenge);
                }
                byte => {
                    value.push(char::from(byte));
                    index += 1;
                }
            }
        }
        if !closed || value.is_empty() || parsed.insert(key, value).is_some() {
            return Err(McpStreamableHttpError::InvalidAuthenticationChallenge);
        }
        while index < bytes.len() && bytes[index] == b' ' {
            index += 1;
        }
        if index < bytes.len() && bytes[index] != b',' {
            return Err(McpStreamableHttpError::InvalidAuthenticationChallenge);
        }
    }
    Ok(parsed)
}

pub(super) fn validate_session_header(
    headers: &HeaderMap,
) -> Result<Option<SecretString>, McpStreamableHttpError> {
    let values = headers.get_all(MCP_SESSION_HEADER);
    if values.iter().count() > 1 {
        return Err(McpStreamableHttpError::InvalidSessionId);
    }
    let Some(value) = values.iter().next() else {
        return Ok(None);
    };
    let bytes = value.as_bytes();
    if bytes.is_empty()
        || bytes.len() > MAX_SESSION_ID_BYTES
        || bytes.iter().any(|byte| !(0x21..=0x7e).contains(byte))
    {
        return Err(McpStreamableHttpError::InvalidSessionId);
    }
    let value = value
        .to_str()
        .map_err(|_| McpStreamableHttpError::InvalidSessionId)?;
    Ok(Some(SecretString::new(value)))
}
