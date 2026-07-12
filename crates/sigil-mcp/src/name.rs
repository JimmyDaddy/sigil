use super::*;

pub struct McpToolName {
    pub provider_name: String,
    pub server_name: String,
    pub original_name: String,
}

pub fn mcp_provider_tool_name_prefix(server_name: &str) -> String {
    format!("mcp__{}__", sanitize_provider_name_part(server_name))
}

/// Returns the collision-free first provider-visible candidate for one exact MCP identity.
///
/// Registries may choose a different hashed suffix only when another tool already occupies this
/// candidate. Callers must therefore verify the returned name exists before dispatching it.
#[must_use]
pub fn mcp_provider_tool_name_candidate(
    server_name: &str,
    original_name: &str,
    max_provider_name_chars: usize,
) -> String {
    let mut used = BTreeSet::new();
    McpToolName::new(
        server_name,
        original_name,
        max_provider_name_chars,
        &mut used,
    )
    .provider_name
}

impl McpToolName {
    /// Builds a collision-safe provider-visible name for one exact MCP server/tool identity.
    pub fn new(
        server_name: &str,
        original_name: &str,
        max_provider_name_chars: usize,
        used_provider_names: &mut BTreeSet<String>,
    ) -> Self {
        let base = format!(
            "mcp__{}__{}",
            sanitize_provider_name_part(server_name),
            sanitize_provider_name_part(original_name)
        );
        let identity = format!("{server_name}\0{original_name}");
        let mut provider_name =
            fit_provider_name_with_hash(&base, &identity, max_provider_name_chars);
        let mut attempt = 0usize;
        while used_provider_names.contains(&provider_name) {
            attempt += 1;
            provider_name = provider_name_with_hash(
                &base,
                &format!("{identity}\0{attempt}"),
                max_provider_name_chars,
            );
        }
        used_provider_names.insert(provider_name.clone());

        Self {
            provider_name,
            server_name: server_name.to_owned(),
            original_name: original_name.to_owned(),
        }
    }
}

pub(super) fn sanitize_provider_name_part(value: &str) -> String {
    let mut sanitized = String::new();
    let mut previous_underscore = false;
    for ch in value.chars() {
        let safe = ch.is_ascii_alphanumeric() || ch == '_';
        if safe {
            sanitized.push(ch);
            previous_underscore = false;
        } else if !previous_underscore {
            sanitized.push('_');
            previous_underscore = true;
        }
    }
    let trimmed = sanitized.trim_matches('_').to_owned();
    if trimmed.is_empty() {
        "tool".to_owned()
    } else {
        trimmed
    }
}

pub(super) fn fit_provider_name_with_hash(base: &str, identity: &str, max_chars: usize) -> String {
    if base.len() <= max_chars {
        return base.to_owned();
    }
    provider_name_with_hash(base, identity, max_chars)
}

pub(super) fn provider_name_with_hash(base: &str, identity: &str, max_chars: usize) -> String {
    let suffix = format!("__{:08x}", stable_hash(identity));
    let prefix_len = max_chars.saturating_sub(suffix.len()).max(1);
    let mut output = base.chars().take(prefix_len).collect::<String>();
    output.push_str(&suffix);
    output
}

pub(super) fn stable_hash(value: &str) -> u64 {
    let mut hash = 0xcbf2_9ce4_8422_2325u64;
    for byte in value.as_bytes() {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    hash
}
