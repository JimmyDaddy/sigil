use super::*;

const KNOWN_DRAFTS: [&str; 4] = [
    "http://json-schema.org/draft-07/schema#",
    "https://json-schema.org/draft/2020-12/schema",
    "https://json-schema.org/draft/2019-09/schema",
    "https://json-schema.org/draft-07/schema",
];

#[derive(Debug, Clone)]
pub struct CompiledMcpSchema {
    schema: Value,
}

impl CompiledMcpSchema {
    pub fn compile(schema: &Value) -> Result<Self, McpStreamableHttpError> {
        compile_bounded_schema(schema, MAX_SCHEMA_BYTES, MAX_SCHEMA_DEPTH, MAX_SCHEMA_NODES)
    }

    pub fn validate(&self, value: &Value) -> Result<(), McpStreamableHttpError> {
        validate_schema_value(&self.schema, &self.schema, value, 0, &mut Vec::new())
    }
}

pub(super) fn compile_bounded_schema(
    schema: &Value,
    max_bytes: usize,
    max_depth: usize,
    max_nodes: usize,
) -> Result<CompiledMcpSchema, McpStreamableHttpError> {
    let bytes = serde_json::to_vec(schema).map_err(|_| McpStreamableHttpError::SchemaDrift)?;
    if bytes.len() > max_bytes {
        return Err(McpStreamableHttpError::SchemaDrift);
    }
    let mut nodes = 0usize;
    measure_json(schema, 0, max_depth, max_nodes, &mut nodes)?;
    validate_schema_shape(schema, schema, 0, max_depth, &mut Vec::new())?;
    Ok(CompiledMcpSchema {
        schema: schema.clone(),
    })
}

fn measure_json(
    value: &Value,
    depth: usize,
    max_depth: usize,
    max_nodes: usize,
    nodes: &mut usize,
) -> Result<(), McpStreamableHttpError> {
    if depth > max_depth {
        return Err(McpStreamableHttpError::SchemaDrift);
    }
    *nodes = nodes
        .checked_add(1)
        .ok_or(McpStreamableHttpError::SchemaDrift)?;
    if *nodes > max_nodes {
        return Err(McpStreamableHttpError::SchemaDrift);
    }
    match value {
        Value::Array(values) => {
            for child in values {
                measure_json(child, depth + 1, max_depth, max_nodes, nodes)?;
            }
        }
        Value::Object(values) => {
            for child in values.values() {
                measure_json(child, depth + 1, max_depth, max_nodes, nodes)?;
            }
        }
        _ => {}
    }
    Ok(())
}

fn validate_schema_shape(
    root: &Value,
    schema: &Value,
    depth: usize,
    max_depth: usize,
    ref_stack: &mut Vec<String>,
) -> Result<(), McpStreamableHttpError> {
    if depth > max_depth {
        return Err(McpStreamableHttpError::SchemaDrift);
    }
    let object = schema
        .as_object()
        .ok_or(McpStreamableHttpError::SchemaDrift)?;
    for forbidden in [
        "$dynamicRef",
        "allOf",
        "anyOf",
        "not",
        "if",
        "then",
        "else",
        "dependentSchemas",
        "unevaluatedProperties",
        "patternProperties",
        "propertyNames",
        "contains",
    ] {
        if object.contains_key(forbidden) {
            return Err(McpStreamableHttpError::SchemaDrift);
        }
    }
    for keyword in object.keys() {
        if !matches!(
            keyword.as_str(),
            "$schema"
                | "$ref"
                | "$defs"
                | "definitions"
                | "type"
                | "properties"
                | "required"
                | "additionalProperties"
                | "items"
                | "enum"
                | "enumNames"
                | "oneOf"
                | "const"
                | "title"
                | "description"
                | "default"
                | "pattern"
                | "minimum"
                | "maximum"
                | "minLength"
                | "maxLength"
                | "minItems"
                | "maxItems"
                | "uniqueItems"
        ) {
            return Err(McpStreamableHttpError::SchemaDrift);
        }
    }
    if let Some(draft) = object.get("$schema") {
        let draft = draft.as_str().ok_or(McpStreamableHttpError::SchemaDrift)?;
        if !KNOWN_DRAFTS.contains(&draft) {
            return Err(McpStreamableHttpError::SchemaDrift);
        }
    }
    if let Some(reference) = object.get("$ref") {
        if object.keys().any(|key| {
            !matches!(
                key.as_str(),
                "$ref" | "$schema" | "title" | "description" | "default"
            )
        }) {
            return Err(McpStreamableHttpError::SchemaDrift);
        }
        let reference = reference
            .as_str()
            .ok_or(McpStreamableHttpError::SchemaDrift)?;
        let target = resolve_local_ref(root, reference)?;
        if ref_stack.iter().any(|entry| entry == reference) {
            return Err(McpStreamableHttpError::SchemaDrift);
        }
        ref_stack.push(reference.to_owned());
        let result = validate_schema_shape(root, target, depth + 1, max_depth, ref_stack);
        ref_stack.pop();
        result?;
    }
    for key in ["title", "description"] {
        if object
            .get(key)
            .is_some_and(|value| value.as_str().is_none_or(|text| text.len() > 8 * 1024))
        {
            return Err(McpStreamableHttpError::SchemaDrift);
        }
    }
    if let Some(pattern) = object.get("pattern") {
        let pattern = pattern
            .as_str()
            .ok_or(McpStreamableHttpError::SchemaDrift)?;
        if pattern.len() > 1024 {
            return Err(McpStreamableHttpError::SchemaDrift);
        }
        Regex::new(pattern).map_err(|_| McpStreamableHttpError::SchemaDrift)?;
    }
    if let Some(schema_type) = object.get("type")
        && !matches!(
            schema_type.as_str(),
            Some("object" | "array" | "string" | "number" | "integer" | "boolean" | "null")
        )
    {
        return Err(McpStreamableHttpError::SchemaDrift);
    }
    let schema_type = object.get("type").and_then(Value::as_str);
    if (["properties", "required", "additionalProperties"]
        .iter()
        .any(|key| object.contains_key(*key))
        && schema_type != Some("object"))
        || (object.contains_key("items") && schema_type != Some("array"))
        || (["pattern", "minLength", "maxLength"]
            .iter()
            .any(|key| object.contains_key(*key))
            && schema_type != Some("string"))
        || (["minimum", "maximum"]
            .iter()
            .any(|key| object.contains_key(*key))
            && !matches!(schema_type, Some("number" | "integer")))
    {
        return Err(McpStreamableHttpError::SchemaDrift);
    }
    validate_nonnegative_integer_keywords(object)?;
    for key in ["minimum", "maximum"] {
        if object.get(key).is_some_and(|value| !value.is_number()) {
            return Err(McpStreamableHttpError::SchemaDrift);
        }
    }
    if let (Some(min), Some(max)) = (
        object.get("minimum").and_then(Value::as_f64),
        object.get("maximum").and_then(Value::as_f64),
    ) && min > max
    {
        return Err(McpStreamableHttpError::SchemaDrift);
    }
    if let Some(required) = object.get("required") {
        let required = required
            .as_array()
            .ok_or(McpStreamableHttpError::SchemaDrift)?;
        if required.len() > MAX_SCHEMA_PROPERTIES {
            return Err(McpStreamableHttpError::SchemaDrift);
        }
        let mut seen = BTreeSet::new();
        for name in required {
            let name = name.as_str().ok_or(McpStreamableHttpError::SchemaDrift)?;
            if name.is_empty() || name.len() > 512 || !seen.insert(name) {
                return Err(McpStreamableHttpError::SchemaDrift);
            }
        }
    }
    if let Some(additional) = object.get("additionalProperties")
        && !additional.is_boolean()
    {
        return Err(McpStreamableHttpError::SchemaDrift);
    }
    if let Some(properties) = object.get("properties") {
        let properties = properties
            .as_object()
            .ok_or(McpStreamableHttpError::SchemaDrift)?;
        if properties.len() > MAX_SCHEMA_PROPERTIES {
            return Err(McpStreamableHttpError::SchemaDrift);
        }
        for (name, child) in properties {
            if name.is_empty() || name.len() > 512 || name.chars().any(char::is_control) {
                return Err(McpStreamableHttpError::SchemaDrift);
            }
            validate_schema_shape(root, child, depth + 1, max_depth, ref_stack)?;
        }
    }
    for definitions_key in ["$defs", "definitions"] {
        if let Some(definitions) = object.get(definitions_key) {
            let definitions = definitions
                .as_object()
                .ok_or(McpStreamableHttpError::SchemaDrift)?;
            for child in definitions.values() {
                validate_schema_shape(root, child, depth + 1, max_depth, ref_stack)?;
            }
        }
    }
    if let Some(items) = object.get("items") {
        validate_schema_shape(root, items, depth + 1, max_depth, ref_stack)?;
    }
    if let Some(one_of) = object.get("oneOf") {
        let choices = one_of
            .as_array()
            .ok_or(McpStreamableHttpError::SchemaDrift)?;
        if choices.is_empty() || choices.len() > 64 {
            return Err(McpStreamableHttpError::SchemaDrift);
        }
        for choice in choices {
            validate_schema_shape(root, choice, depth + 1, max_depth, ref_stack)?;
        }
    }
    if let Some(enum_values) = object.get("enum") {
        let enum_values = enum_values
            .as_array()
            .ok_or(McpStreamableHttpError::SchemaDrift)?;
        if enum_values.is_empty() || enum_values.len() > 512 {
            return Err(McpStreamableHttpError::SchemaDrift);
        }
    }
    if let Some(enum_names) = object.get("enumNames") {
        let names = enum_names
            .as_array()
            .ok_or(McpStreamableHttpError::SchemaDrift)?;
        if names.iter().any(|name| name.as_str().is_none()) {
            return Err(McpStreamableHttpError::SchemaDrift);
        }
    }
    Ok(())
}

fn validate_nonnegative_integer_keywords(
    object: &Map<String, Value>,
) -> Result<(), McpStreamableHttpError> {
    for key in ["minLength", "maxLength", "minItems", "maxItems"] {
        if object
            .get(key)
            .is_some_and(|value| value.as_u64().is_none())
        {
            return Err(McpStreamableHttpError::SchemaDrift);
        }
    }
    for (min_key, max_key) in [("minLength", "maxLength"), ("minItems", "maxItems")] {
        if let (Some(min), Some(max)) = (
            object.get(min_key).and_then(Value::as_u64),
            object.get(max_key).and_then(Value::as_u64),
        ) && min > max
        {
            return Err(McpStreamableHttpError::SchemaDrift);
        }
    }
    if object
        .get("uniqueItems")
        .is_some_and(|value| !value.is_boolean())
    {
        return Err(McpStreamableHttpError::SchemaDrift);
    }
    Ok(())
}

fn resolve_local_ref<'a>(
    root: &'a Value,
    reference: &str,
) -> Result<&'a Value, McpStreamableHttpError> {
    if reference == "#" {
        return Ok(root);
    }
    let pointer = reference
        .strip_prefix('#')
        .filter(|pointer| pointer.starts_with('/'))
        .ok_or(McpStreamableHttpError::SchemaDrift)?;
    root.pointer(pointer)
        .ok_or(McpStreamableHttpError::SchemaDrift)
}

pub(super) fn validate_schema_value(
    root: &Value,
    schema: &Value,
    value: &Value,
    depth: usize,
    ref_stack: &mut Vec<String>,
) -> Result<(), McpStreamableHttpError> {
    if depth > MAX_SCHEMA_DEPTH {
        return Err(McpStreamableHttpError::SchemaDrift);
    }
    let object = schema
        .as_object()
        .ok_or(McpStreamableHttpError::SchemaDrift)?;
    if let Some(reference) = object.get("$ref").and_then(Value::as_str) {
        if ref_stack.iter().any(|entry| entry == reference) {
            return Err(McpStreamableHttpError::SchemaDrift);
        }
        ref_stack.push(reference.to_owned());
        let result = validate_schema_value(
            root,
            resolve_local_ref(root, reference)?,
            value,
            depth + 1,
            ref_stack,
        );
        ref_stack.pop();
        result?;
    }
    if let Some(constant) = object.get("const")
        && constant != value
    {
        return Err(McpStreamableHttpError::SchemaDrift);
    }
    if let Some(enum_values) = object.get("enum").and_then(Value::as_array)
        && !enum_values.contains(value)
    {
        return Err(McpStreamableHttpError::SchemaDrift);
    }
    if let Some(one_of) = object.get("oneOf").and_then(Value::as_array) {
        let matches = one_of
            .iter()
            .filter(|choice| {
                validate_schema_value(root, choice, value, depth + 1, &mut ref_stack.clone())
                    .is_ok()
            })
            .count();
        if matches != 1 {
            return Err(McpStreamableHttpError::SchemaDrift);
        }
    }
    match object.get("type").and_then(Value::as_str) {
        Some("object") => {
            let instance = value
                .as_object()
                .ok_or(McpStreamableHttpError::SchemaDrift)?;
            let properties = object
                .get("properties")
                .and_then(Value::as_object)
                .cloned()
                .unwrap_or_default();
            if let Some(required) = object.get("required").and_then(Value::as_array)
                && required
                    .iter()
                    .filter_map(Value::as_str)
                    .any(|name| !instance.contains_key(name))
            {
                return Err(McpStreamableHttpError::SchemaDrift);
            }
            if object.get("additionalProperties") == Some(&Value::Bool(false))
                && instance.keys().any(|name| !properties.contains_key(name))
            {
                return Err(McpStreamableHttpError::SchemaDrift);
            }
            for (name, child) in &properties {
                if let Some(value) = instance.get(name) {
                    validate_schema_value(root, child, value, depth + 1, ref_stack)?;
                }
            }
        }
        Some("array") => {
            let values = value
                .as_array()
                .ok_or(McpStreamableHttpError::SchemaDrift)?;
            validate_length(values.len(), object, "minItems", "maxItems")?;
            if object.get("uniqueItems") == Some(&Value::Bool(true)) {
                let mut seen = BTreeSet::new();
                for value in values {
                    let encoded = serde_json::to_string(value)
                        .map_err(|_| McpStreamableHttpError::SchemaDrift)?;
                    if !seen.insert(encoded) {
                        return Err(McpStreamableHttpError::SchemaDrift);
                    }
                }
            }
            if let Some(items) = object.get("items") {
                for value in values {
                    validate_schema_value(root, items, value, depth + 1, ref_stack)?;
                }
            }
        }
        Some("string") => {
            let text = value.as_str().ok_or(McpStreamableHttpError::SchemaDrift)?;
            validate_length(text.chars().count(), object, "minLength", "maxLength")?;
            if let Some(pattern) = object.get("pattern").and_then(Value::as_str) {
                let regex = Regex::new(pattern).map_err(|_| McpStreamableHttpError::SchemaDrift)?;
                if !regex.is_match(text) {
                    return Err(McpStreamableHttpError::SchemaDrift);
                }
            }
        }
        Some("number") if !value.is_number() => return Err(McpStreamableHttpError::SchemaDrift),
        Some("integer") if value.as_i64().is_none() && value.as_u64().is_none() => {
            return Err(McpStreamableHttpError::SchemaDrift);
        }
        Some("number" | "integer") => {
            let number = value.as_f64().ok_or(McpStreamableHttpError::SchemaDrift)?;
            if object
                .get("minimum")
                .and_then(Value::as_f64)
                .is_some_and(|minimum| number < minimum)
                || object
                    .get("maximum")
                    .and_then(Value::as_f64)
                    .is_some_and(|maximum| number > maximum)
            {
                return Err(McpStreamableHttpError::SchemaDrift);
            }
        }
        Some("boolean") if !value.is_boolean() => {
            return Err(McpStreamableHttpError::SchemaDrift);
        }
        Some("null") if !value.is_null() => return Err(McpStreamableHttpError::SchemaDrift),
        Some("boolean" | "null") | None => {}
        _ => return Err(McpStreamableHttpError::SchemaDrift),
    }
    Ok(())
}

fn validate_length(
    actual: usize,
    schema: &Map<String, Value>,
    min_key: &str,
    max_key: &str,
) -> Result<(), McpStreamableHttpError> {
    if schema
        .get(min_key)
        .and_then(Value::as_u64)
        .is_some_and(|minimum| actual < minimum as usize)
        || schema
            .get(max_key)
            .and_then(Value::as_u64)
            .is_some_and(|maximum| actual > maximum as usize)
    {
        return Err(McpStreamableHttpError::SchemaDrift);
    }
    Ok(())
}
