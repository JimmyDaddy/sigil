use super::*;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum McpRemoteFormFieldKind {
    String,
    Number,
    Integer,
    Boolean,
    SingleSelect,
    MultiSelect,
}

#[derive(Debug, Clone, PartialEq)]
pub struct McpRemoteFormField {
    pub name: String,
    pub kind: McpRemoteFormFieldKind,
    pub required: bool,
    pub title: Option<String>,
    pub description: Option<String>,
    pub choices: Vec<Value>,
    schema: Value,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ValidatedMcpFormRequest {
    pub safe_message: String,
    pub fields: Vec<McpRemoteFormField>,
    schema: Value,
}

impl ValidatedMcpFormRequest {
    pub fn parse(params: &Value) -> Result<Self, McpStreamableHttpError> {
        Self::parse_for_version(params, McpRemoteProtocolVersion::V2025_11_25)
    }

    pub fn parse_for_version(
        params: &Value,
        version: McpRemoteProtocolVersion,
    ) -> Result<Self, McpStreamableHttpError> {
        let object = params
            .as_object()
            .ok_or(McpStreamableHttpError::InvalidForm)?;
        if object.get("mode").and_then(Value::as_str) == Some("url") {
            return Err(McpStreamableHttpError::UrlElicitationUnsupported);
        }
        if object
            .get("mode")
            .and_then(Value::as_str)
            .is_some_and(|mode| mode != "form")
        {
            return Err(McpStreamableHttpError::InvalidForm);
        }
        let message = object
            .get("message")
            .and_then(Value::as_str)
            .unwrap_or_default();
        if message.len() > MAX_FORM_MESSAGE_BYTES {
            return Err(McpStreamableHttpError::InvalidForm);
        }
        let schema = object
            .get("requestedSchema")
            .ok_or(McpStreamableHttpError::InvalidForm)?;
        reject_form_refs(schema)?;
        schema::compile_bounded_schema(
            schema,
            MAX_FORM_SCHEMA_BYTES,
            MAX_FORM_SCHEMA_DEPTH,
            MAX_FORM_SCHEMA_NODES,
        )
        .map_err(|_| McpStreamableHttpError::InvalidForm)?;
        let schema_object = schema
            .as_object()
            .ok_or(McpStreamableHttpError::InvalidForm)?;
        if schema_object.get("type").and_then(Value::as_str) != Some("object") {
            return Err(McpStreamableHttpError::InvalidForm);
        }
        let properties = schema_object
            .get("properties")
            .and_then(Value::as_object)
            .ok_or(McpStreamableHttpError::InvalidForm)?;
        if properties.len() > MAX_FORM_PROPERTIES {
            return Err(McpStreamableHttpError::InvalidForm);
        }
        let mut required = BTreeSet::new();
        if let Some(items) = schema_object.get("required") {
            let items = items
                .as_array()
                .ok_or(McpStreamableHttpError::InvalidForm)?;
            if items.len() > MAX_FORM_PROPERTIES {
                return Err(McpStreamableHttpError::InvalidForm);
            }
            for item in items {
                let name = item.as_str().ok_or(McpStreamableHttpError::InvalidForm)?;
                if !properties.contains_key(name) || !required.insert(name.to_owned()) {
                    return Err(McpStreamableHttpError::InvalidForm);
                }
            }
        }
        let mut fields = Vec::with_capacity(properties.len());
        for (name, field_schema) in properties {
            if name.is_empty() || name.len() > 128 || name.chars().any(char::is_control) {
                return Err(McpStreamableHttpError::InvalidForm);
            }
            if looks_like_credential_request(name) {
                return Err(McpStreamableHttpError::InvalidForm);
            }
            fields.push(parse_form_field(
                name,
                field_schema,
                required.contains(name),
                version,
            )?);
        }
        Ok(Self {
            safe_message: sanitize_form_text(message),
            fields,
            schema: schema.clone(),
        })
    }

    pub fn validate_response(&self, content: &Value) -> Result<(), McpStreamableHttpError> {
        let bytes = serde_json::to_vec(content).map_err(|_| McpStreamableHttpError::InvalidForm)?;
        if bytes.len() > MAX_FORM_RESPONSE_BYTES {
            return Err(McpStreamableHttpError::InvalidForm);
        }
        let object = content
            .as_object()
            .ok_or(McpStreamableHttpError::InvalidForm)?;
        if object.values().any(|value| {
            value
                .as_str()
                .is_some_and(|text| text.len() > MAX_FORM_STRING_BYTES)
        }) {
            return Err(McpStreamableHttpError::InvalidForm);
        }
        let declared = self
            .fields
            .iter()
            .map(|field| field.name.as_str())
            .collect::<BTreeSet<_>>();
        if object.keys().any(|name| !declared.contains(name.as_str())) {
            return Err(McpStreamableHttpError::InvalidForm);
        }
        if object.values().any(value_looks_like_credential) {
            return Err(McpStreamableHttpError::InvalidForm);
        }
        CompiledMcpSchema::compile(&self.schema)
            .and_then(|compiled| compiled.validate(content))
            .map_err(|_| McpStreamableHttpError::InvalidForm)
    }
}

fn parse_form_field(
    name: &str,
    schema: &Value,
    required: bool,
    version: McpRemoteProtocolVersion,
) -> Result<McpRemoteFormField, McpStreamableHttpError> {
    let object = schema
        .as_object()
        .ok_or(McpStreamableHttpError::InvalidForm)?;
    let title = object
        .get("title")
        .and_then(Value::as_str)
        .map(sanitize_form_text);
    let description = object
        .get("description")
        .and_then(Value::as_str)
        .map(sanitize_form_text);
    if title.as_ref().is_some_and(|value| value.len() > 256)
        || description.as_ref().is_some_and(|value| value.len() > 1024)
        || title.as_deref().is_some_and(looks_like_credential_request)
        || description
            .as_deref()
            .is_some_and(looks_like_credential_request)
    {
        return Err(McpStreamableHttpError::InvalidForm);
    }
    let mut choices = object
        .get("enum")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    if object.contains_key("enumNames") && version != McpRemoteProtocolVersion::V2025_06_18 {
        return Err(McpStreamableHttpError::InvalidForm);
    }
    if let Some(enum_names) = object.get("enumNames") {
        let enum_names = enum_names
            .as_array()
            .ok_or(McpStreamableHttpError::InvalidForm)?;
        if enum_names.len() != choices.len()
            || enum_names.iter().any(|name| {
                name.as_str()
                    .is_none_or(|name| name.len() > 512 || looks_like_credential_request(name))
            })
        {
            return Err(McpStreamableHttpError::InvalidForm);
        }
    }
    if let Some(one_of) = object.get("oneOf") {
        if version != McpRemoteProtocolVersion::V2025_11_25 || !choices.is_empty() {
            return Err(McpStreamableHttpError::InvalidForm);
        }
        choices = one_of
            .as_array()
            .ok_or(McpStreamableHttpError::InvalidForm)?
            .iter()
            .map(|choice| {
                let choice = choice
                    .as_object()
                    .ok_or(McpStreamableHttpError::InvalidForm)?;
                if choice
                    .keys()
                    .any(|key| !matches!(key.as_str(), "const" | "title"))
                {
                    return Err(McpStreamableHttpError::InvalidForm);
                }
                let title = choice
                    .get("title")
                    .and_then(Value::as_str)
                    .ok_or(McpStreamableHttpError::InvalidForm)?;
                if title.len() > 512 || looks_like_credential_request(title) {
                    return Err(McpStreamableHttpError::InvalidForm);
                }
                choice
                    .get("const")
                    .cloned()
                    .ok_or(McpStreamableHttpError::InvalidForm)
            })
            .collect::<Result<Vec<_>, _>>()?;
    }
    if choices.len() > 64
        || choices
            .iter()
            .any(|choice| serde_json::to_vec(choice).map_or(true, |encoded| encoded.len() > 512))
        || serde_json::to_vec(&choices)
            .map_err(|_| McpStreamableHttpError::InvalidForm)?
            .len()
            > 8 * 1024
    {
        return Err(McpStreamableHttpError::InvalidForm);
    }
    let kind = match object.get("type").and_then(Value::as_str) {
        Some("string") if !choices.is_empty() || object.contains_key("oneOf") => {
            McpRemoteFormFieldKind::SingleSelect
        }
        Some("string") => McpRemoteFormFieldKind::String,
        Some("number") => McpRemoteFormFieldKind::Number,
        Some("integer") => McpRemoteFormFieldKind::Integer,
        Some("boolean") => McpRemoteFormFieldKind::Boolean,
        Some("array")
            if object
                .get("items")
                .and_then(Value::as_object)
                .and_then(|items| items.get("enum"))
                .is_some() =>
        {
            if version != McpRemoteProtocolVersion::V2025_11_25 {
                return Err(McpStreamableHttpError::InvalidForm);
            }
            McpRemoteFormFieldKind::MultiSelect
        }
        _ => return Err(McpStreamableHttpError::InvalidForm),
    };
    Ok(McpRemoteFormField {
        name: name.to_owned(),
        kind,
        required,
        title,
        description,
        choices,
        schema: schema.clone(),
    })
}

fn sanitize_form_text(value: &str) -> String {
    let bytes = value.as_bytes();
    let mut plain = String::with_capacity(value.len().min(MAX_SCHEMA_TEXT_BYTES));
    let mut index = 0usize;
    while index < bytes.len() && plain.len() < MAX_SCHEMA_TEXT_BYTES {
        if bytes[index] == 0x1b {
            index += 1;
            if index < bytes.len() && bytes[index] == b'[' {
                index += 1;
                while index < bytes.len() && !(0x40..=0x7e).contains(&bytes[index]) {
                    index += 1;
                }
                index = index.saturating_add(1);
            } else if index < bytes.len() && bytes[index] == b']' {
                index += 1;
                while index < bytes.len() {
                    if bytes[index] == 0x07 {
                        index += 1;
                        break;
                    }
                    if bytes[index] == 0x1b && bytes.get(index + 1).copied() == Some(b'\\') {
                        index += 2;
                        break;
                    }
                    index += 1;
                }
            }
            continue;
        }
        let Some(character) = value[index..].chars().next() else {
            break;
        };
        index += character.len_utf8();
        if !character.is_control()
            && !matches!(character, '\u{202a}'..='\u{202e}' | '\u{2066}'..='\u{2069}')
        {
            plain.push(character);
        }
    }
    plain
        .split_whitespace()
        .map(|token| {
            if token.to_ascii_lowercase().starts_with("http://")
                || token.to_ascii_lowercase().starts_with("https://")
            {
                "[url omitted]"
            } else {
                token
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

fn reject_form_refs(value: &Value) -> Result<(), McpStreamableHttpError> {
    match value {
        Value::Object(object) => {
            if object.contains_key("$ref") || object.contains_key("$dynamicRef") {
                return Err(McpStreamableHttpError::InvalidForm);
            }
            for child in object.values() {
                reject_form_refs(child)?;
            }
        }
        Value::Array(values) => {
            for child in values {
                reject_form_refs(child)?;
            }
        }
        _ => {}
    }
    Ok(())
}

fn looks_like_credential_request(value: &str) -> bool {
    let normalized = value
        .chars()
        .filter(|character| character.is_ascii_alphanumeric())
        .collect::<String>()
        .to_ascii_lowercase();
    [
        "password",
        "passwd",
        "apikey",
        "apitoken",
        "accesstoken",
        "credential",
        "secret",
    ]
    .iter()
    .any(|marker| normalized.contains(marker))
}

fn value_looks_like_credential(value: &Value) -> bool {
    match value {
        Value::String(text) => {
            looks_like_credential_request(text)
                || (text.len() >= 32
                    && !text.contains(char::is_whitespace)
                    && text.chars().any(|character| character.is_ascii_lowercase())
                    && text.chars().any(|character| character.is_ascii_uppercase())
                    && text.chars().any(|character| character.is_ascii_digit()))
        }
        Value::Array(values) => values.iter().any(value_looks_like_credential),
        Value::Object(values) => values.values().any(value_looks_like_credential),
        _ => false,
    }
}
