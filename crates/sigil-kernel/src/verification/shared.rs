use super::*;

pub(super) fn stable_hash_parts<'a>(
    check_spec_id: &'a str,
    command: &'a str,
    args: impl IntoIterator<Item = &'a str>,
    cwd: &'a str,
    scope_hash: &'a str,
    effect: &'a str,
) -> String {
    let mut digest = Sha256::new();
    for part in [check_spec_id, command] {
        digest.update(part.as_bytes());
        digest.update([0]);
    }
    for arg in args {
        digest.update(arg.as_bytes());
        digest.update([0]);
    }
    for part in [cwd, scope_hash, effect] {
        digest.update(part.as_bytes());
        digest.update([0]);
    }
    format!("sha256:{:x}", digest.finalize())
}

pub(super) fn canonical_json_bytes(value: &serde_json::Value) -> Result<Vec<u8>> {
    let canonical = canonicalize_value(value)?;
    serde_json::to_vec(&canonical)
        .map_err(|error| anyhow!("failed to serialize canonical json: {error}"))
}

fn canonicalize_value(value: &serde_json::Value) -> Result<serde_json::Value> {
    match value {
        serde_json::Value::Array(values) => values
            .iter()
            .map(canonicalize_value)
            .collect::<Result<Vec<_>>>()
            .map(serde_json::Value::Array),
        serde_json::Value::Object(object) => {
            let ordered = object
                .iter()
                .map(|(key, value)| canonicalize_value(value).map(|value| (key.clone(), value)))
                .collect::<Result<BTreeMap<_, _>>>()?;
            Ok(serde_json::Value::Object(ordered.into_iter().collect()))
        }
        serde_json::Value::Number(number) => Ok(serde_json::Value::Number(number.clone())),
        serde_json::Value::Null | serde_json::Value::Bool(_) | serde_json::Value::String(_) => {
            Ok(value.clone())
        }
    }
}
