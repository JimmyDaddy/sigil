use super::*;

pub(super) fn render_section(lines: &mut Vec<String>, config_state: &ConfigState) {
    lines.push("[web data tools]".to_owned());
    for field in [
        ConfigField::WebEnabled,
        ConfigField::WebNetworkMode,
        ConfigField::WebSearchRoute,
        ConfigField::WebBundledSearchEnabled,
    ] {
        lines.push(render_config_value_row(config_state, field));
    }
    lines.push(String::new());
    lines.push("[privacy boundary]".to_owned());
    lines.push(render_config_readonly_row(
        "Query destination",
        "provider, configured MCP, or mcp.exa.ai according to route",
    ));
    lines.push(render_config_readonly_row(
        "Fetched content",
        "external/untrusted; exact URL capability remains session-local",
    ));
    lines.extend(render_config_selection_details(config_state));
}
