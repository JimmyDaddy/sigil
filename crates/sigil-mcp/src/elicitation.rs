use super::*;

const MAX_NORMALIZED_FORM_PROPERTIES: usize = 32;
const MAX_NORMALIZED_FORM_OPTIONS: usize = 64;
const MAX_NORMALIZED_FORM_TEXT_BYTES: usize = 1_024;
const MAX_NORMALIZED_FORM_SCHEMA_BYTES: usize = 32 * 1_024;
const MAX_NORMALIZED_FORM_SCHEMA_DEPTH: usize = 8;
const MAX_NORMALIZED_FORM_SCHEMA_NODES: usize = 512;
const MAX_NORMALIZED_FORM_MESSAGE_BYTES: usize = 4 * 1_024;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum McpNormalizedFormFieldKind {
    String,
    Number,
    Integer,
    Boolean,
    SingleSelect,
    MultiSelect,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct McpNormalizedFormOption {
    pub value: String,
    pub label: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct McpNormalizedFormField {
    pub name: String,
    pub kind: McpNormalizedFormFieldKind,
    pub required: bool,
    pub title: Option<String>,
    pub description: Option<String>,
    pub options: Vec<McpNormalizedFormOption>,
    pub default: Option<Value>,
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct McpNormalizedForm {
    pub fields: Vec<McpNormalizedFormField>,
}

/// Produces terminal-safe bounded text for the common MCP form renderer.
pub fn normalize_mcp_form_message(value: &str) -> Result<String> {
    if value.len() > MAX_NORMALIZED_FORM_MESSAGE_BYTES {
        bail!("MCP form message exceeds the supported size");
    }
    let bytes = value.as_bytes();
    let mut plain = String::with_capacity(value.len());
    let mut index = 0usize;
    while index < bytes.len() {
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
    Ok(plain
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
        .join(" "))
}

/// Converts the supported MCP flat-form subset into one provider-neutral renderer contract.
/// Unsupported shapes fail explicitly instead of being silently rendered as text fields.
pub fn normalize_mcp_form_schema(schema: &Value) -> Result<McpNormalizedForm> {
    let fail = |reason: &str| anyhow!("UnsupportedFormShape: {reason}");
    let encoded =
        serde_json::to_vec(schema).map_err(|_| fail("requestedSchema must be valid JSON"))?;
    if encoded.len() > MAX_NORMALIZED_FORM_SCHEMA_BYTES {
        bail!("UnsupportedFormShape: requestedSchema is too large");
    }
    let (depth, nodes) = normalized_form_shape_size(schema);
    if depth > MAX_NORMALIZED_FORM_SCHEMA_DEPTH || nodes > MAX_NORMALIZED_FORM_SCHEMA_NODES {
        bail!("UnsupportedFormShape: requestedSchema is too deeply nested or complex");
    }
    reject_normalized_form_refs(schema).map_err(|_| fail("references are not supported"))?;
    reject_normalized_form_combinators(schema)?;
    let object = schema
        .as_object()
        .ok_or_else(|| fail("requestedSchema must be an object"))?;
    if object.get("type").and_then(Value::as_str) != Some("object") {
        bail!("UnsupportedFormShape: requestedSchema type must be object");
    }
    let empty_properties = serde_json::Map::new();
    let properties = match object.get("properties") {
        Some(properties) => properties
            .as_object()
            .ok_or_else(|| fail("requestedSchema properties must be an object"))?,
        None => &empty_properties,
    };
    if properties.len() > MAX_NORMALIZED_FORM_PROPERTIES {
        bail!("UnsupportedFormShape: form has too many properties");
    }
    let mut required = BTreeSet::new();
    if let Some(values) = object.get("required") {
        for value in values
            .as_array()
            .ok_or_else(|| fail("required must be an array"))?
        {
            let name = value
                .as_str()
                .ok_or_else(|| fail("required entries must be strings"))?;
            if !properties.contains_key(name) || !required.insert(name.to_owned()) {
                bail!("UnsupportedFormShape: required contains an unknown or duplicate field");
            }
        }
    }
    let mut fields = Vec::with_capacity(properties.len());
    for (name, schema) in properties {
        if name.is_empty()
            || name.len() > 128
            || name.chars().any(char::is_control)
            || normalized_form_looks_sensitive(name)
        {
            bail!("UnsupportedFormShape: field name is invalid");
        }
        let field = schema
            .as_object()
            .ok_or_else(|| fail("field schema must be an object"))?;
        let title = normalized_form_text(field.get("title"), "title")?;
        let description = normalized_form_text(field.get("description"), "description")?;
        let (kind, options) = normalized_form_field_kind(field)?;
        let default = field.get("default").cloned();
        validate_normalized_default(default.as_ref(), &kind, &options)?;
        fields.push(McpNormalizedFormField {
            name: name.clone(),
            kind,
            required: required.contains(name),
            title,
            description,
            options,
            default,
        });
    }
    Ok(McpNormalizedForm { fields })
}

fn normalized_form_field_kind(
    field: &serde_json::Map<String, Value>,
) -> Result<(McpNormalizedFormFieldKind, Vec<McpNormalizedFormOption>)> {
    let kind = field
        .get("type")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow!("UnsupportedFormShape: every field requires one explicit type"))?;
    match kind {
        "string" => {
            let options = normalized_form_options(field)?;
            if options.is_empty() {
                Ok((McpNormalizedFormFieldKind::String, options))
            } else {
                Ok((McpNormalizedFormFieldKind::SingleSelect, options))
            }
        }
        "number" => Ok((McpNormalizedFormFieldKind::Number, Vec::new())),
        "integer" => Ok((McpNormalizedFormFieldKind::Integer, Vec::new())),
        "boolean" => Ok((McpNormalizedFormFieldKind::Boolean, Vec::new())),
        "array" => {
            let items = field
                .get("items")
                .and_then(Value::as_object)
                .ok_or_else(|| anyhow!("UnsupportedFormShape: array field requires flat items"))?;
            if items.get("type").and_then(Value::as_str) != Some("string") {
                bail!("UnsupportedFormShape: multi-select items must be strings");
            }
            let options = normalized_form_options(items)?;
            if options.is_empty() {
                bail!("UnsupportedFormShape: arrays are supported only as bounded multi-selects");
            }
            Ok((McpNormalizedFormFieldKind::MultiSelect, options))
        }
        _ => bail!("UnsupportedFormShape: nested or unknown field type {kind}"),
    }
}

fn normalized_form_options(
    field: &serde_json::Map<String, Value>,
) -> Result<Vec<McpNormalizedFormOption>> {
    let enum_values = field.get("enum").and_then(Value::as_array);
    let enum_names = field.get("enumNames").and_then(Value::as_array);
    let one_of = field.get("oneOf").and_then(Value::as_array);
    if enum_values.is_some() && one_of.is_some() {
        bail!("UnsupportedFormShape: enum and oneOf cannot be combined");
    }
    let options = if let Some(values) = enum_values {
        if enum_names.is_some_and(|names| names.len() != values.len()) {
            bail!("UnsupportedFormShape: enumNames must match enum values");
        }
        values
            .iter()
            .enumerate()
            .map(|(index, value)| {
                let value = value.as_str().ok_or_else(|| {
                    anyhow!("UnsupportedFormShape: select values must be strings")
                })?;
                let label = enum_names
                    .and_then(|names| names.get(index))
                    .and_then(Value::as_str)
                    .unwrap_or(value);
                normalized_option(value, label)
            })
            .collect::<Result<Vec<_>>>()?
    } else if let Some(values) = one_of {
        values
            .iter()
            .map(|value| {
                let value = value.as_object().ok_or_else(|| {
                    anyhow!("UnsupportedFormShape: oneOf choices must be objects")
                })?;
                let exact = value.get("const").and_then(Value::as_str).ok_or_else(|| {
                    anyhow!("UnsupportedFormShape: oneOf choice requires string const")
                })?;
                let label = value
                    .get("title")
                    .and_then(Value::as_str)
                    .ok_or_else(|| anyhow!("UnsupportedFormShape: oneOf choice requires title"))?;
                normalized_option(exact, label)
            })
            .collect::<Result<Vec<_>>>()?
    } else {
        Vec::new()
    };
    if options.len() > MAX_NORMALIZED_FORM_OPTIONS {
        bail!("UnsupportedFormShape: select has too many options");
    }
    let mut unique = BTreeSet::new();
    if options.iter().any(|option| !unique.insert(&option.value)) {
        bail!("UnsupportedFormShape: select values must be unique");
    }
    Ok(options)
}

fn normalized_option(value: &str, label: &str) -> Result<McpNormalizedFormOption> {
    if value.is_empty()
        || value.len() > 512
        || label.is_empty()
        || label.len() > 512
        || value.chars().any(char::is_control)
        || label.chars().any(char::is_control)
        || normalized_form_looks_sensitive(label)
    {
        bail!("UnsupportedFormShape: select option is invalid");
    }
    Ok(McpNormalizedFormOption {
        value: value.to_owned(),
        label: label.to_owned(),
    })
}

fn normalized_form_text(value: Option<&Value>, field: &str) -> Result<Option<String>> {
    let Some(value) = value else {
        return Ok(None);
    };
    let text = value
        .as_str()
        .ok_or_else(|| anyhow!("UnsupportedFormShape: {field} must be a string"))?;
    if text.len() > MAX_NORMALIZED_FORM_TEXT_BYTES
        || text.chars().any(char::is_control)
        || normalized_form_looks_sensitive(text)
    {
        bail!("UnsupportedFormShape: {field} is invalid or too large");
    }
    Ok(Some(text.to_owned()))
}

fn validate_normalized_default(
    default: Option<&Value>,
    kind: &McpNormalizedFormFieldKind,
    options: &[McpNormalizedFormOption],
) -> Result<()> {
    let Some(default) = default else {
        return Ok(());
    };
    let valid = match kind {
        McpNormalizedFormFieldKind::String => default.is_string(),
        McpNormalizedFormFieldKind::Number => default.is_number(),
        McpNormalizedFormFieldKind::Integer => default.as_i64().is_some(),
        McpNormalizedFormFieldKind::Boolean => default.is_boolean(),
        McpNormalizedFormFieldKind::SingleSelect => default
            .as_str()
            .is_some_and(|value| options.iter().any(|option| option.value == value)),
        McpNormalizedFormFieldKind::MultiSelect => default.as_array().is_some_and(|values| {
            values.iter().all(|value| {
                value
                    .as_str()
                    .is_some_and(|value| options.iter().any(|option| option.value == value))
            })
        }),
    };
    if !valid {
        bail!("UnsupportedFormShape: default does not match the field type");
    }
    Ok(())
}

fn reject_normalized_form_refs(value: &Value) -> Result<()> {
    match value {
        Value::Object(object) => {
            if object.contains_key("$ref") || object.contains_key("$dynamicRef") {
                bail!("form references are unsupported");
            }
            for value in object.values() {
                reject_normalized_form_refs(value)?;
            }
        }
        Value::Array(values) => {
            for value in values {
                reject_normalized_form_refs(value)?;
            }
        }
        _ => {}
    }
    Ok(())
}

fn reject_normalized_form_combinators(value: &Value) -> Result<()> {
    match value {
        Value::Object(object) => {
            if [
                "allOf",
                "anyOf",
                "not",
                "if",
                "then",
                "else",
                "dependentSchemas",
                "patternProperties",
                "prefixItems",
                "contains",
            ]
            .iter()
            .any(|keyword| object.contains_key(*keyword))
            {
                bail!("UnsupportedFormShape: schema combinators and nested shapes are unsupported");
            }
            for child in object.values() {
                reject_normalized_form_combinators(child)?;
            }
        }
        Value::Array(values) => {
            for child in values {
                reject_normalized_form_combinators(child)?;
            }
        }
        _ => {}
    }
    Ok(())
}

fn normalized_form_shape_size(value: &Value) -> (usize, usize) {
    match value {
        Value::Object(object) => {
            let mut max_child_depth = 0usize;
            let mut nodes = 1usize;
            for child in object.values() {
                let (depth, child_nodes) = normalized_form_shape_size(child);
                max_child_depth = max_child_depth.max(depth);
                nodes = nodes.saturating_add(child_nodes);
            }
            (1usize.saturating_add(max_child_depth), nodes)
        }
        Value::Array(values) => {
            let mut max_child_depth = 0usize;
            let mut nodes = 1usize;
            for child in values {
                let (depth, child_nodes) = normalized_form_shape_size(child);
                max_child_depth = max_child_depth.max(depth);
                nodes = nodes.saturating_add(child_nodes);
            }
            (1usize.saturating_add(max_child_depth), nodes)
        }
        _ => (1, 1),
    }
}

fn normalized_form_looks_sensitive(value: &str) -> bool {
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

#[derive(Debug, Clone, PartialEq)]
pub struct McpElicitationRequest {
    pub server_name: String,
    pub message: String,
    pub requested_schema: Value,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum McpElicitationAction {
    Accept,
    Decline,
    Cancel,
}

impl McpElicitationAction {
    fn as_str(self) -> &'static str {
        match self {
            Self::Accept => "accept",
            Self::Decline => "decline",
            Self::Cancel => "cancel",
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct McpElicitationResponse {
    pub action: McpElicitationAction,
    pub content: Option<Value>,
}

impl McpElicitationResponse {
    pub fn accept(content: Value) -> Self {
        Self {
            action: McpElicitationAction::Accept,
            content: Some(content),
        }
    }

    pub fn decline() -> Self {
        Self {
            action: McpElicitationAction::Decline,
            content: None,
        }
    }

    pub fn cancel() -> Self {
        Self {
            action: McpElicitationAction::Cancel,
            content: None,
        }
    }

    pub(super) fn into_result(self) -> Value {
        match (self.action, self.content) {
            (McpElicitationAction::Accept, Some(content)) => {
                json!({ "action": self.action.as_str(), "content": content })
            }
            (McpElicitationAction::Accept, None) => {
                json!({ "action": self.action.as_str(), "content": {} })
            }
            (action, _) => json!({ "action": action.as_str() }),
        }
    }
}

#[async_trait]
pub trait McpElicitationHandler: Send + Sync {
    fn supports_elicitation(&self) -> bool {
        false
    }

    async fn elicit(&self, _request: McpElicitationRequest) -> Result<McpElicitationResponse> {
        bail!("MCP elicitation is not supported by sigil yet")
    }
}

#[derive(Debug)]
pub(super) struct UnsupportedMcpElicitationHandler;

#[async_trait]
impl McpElicitationHandler for UnsupportedMcpElicitationHandler {}

pub fn unsupported_mcp_elicitation_handler() -> Arc<dyn McpElicitationHandler> {
    Arc::new(UnsupportedMcpElicitationHandler)
}

pub(super) fn mcp_elicitation_request(
    server_name: &str,
    message: &Value,
) -> Result<McpElicitationRequest> {
    let params = message
        .get("params")
        .and_then(Value::as_object)
        .ok_or_else(|| anyhow!("MCP elicitation/create missing params object"))?;
    let message = normalize_mcp_form_message(
        params
            .get("message")
            .and_then(Value::as_str)
            .unwrap_or("MCP server requested input"),
    )?;
    let requested_schema = params
        .get("requestedSchema")
        .cloned()
        .unwrap_or_else(|| json!({ "type": "object", "properties": {} }));
    normalize_mcp_form_schema(&requested_schema)?;
    Ok(McpElicitationRequest {
        server_name: server_name.to_owned(),
        message,
        requested_schema,
    })
}
