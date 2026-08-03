use anyhow::{Result, anyhow};
use sigil_kernel::{
    HostedCitationFidelity, HostedConstraintEnforcement, HostedCustomToolCompatibility,
    HostedQueryVisibility, HostedSourceFidelity, HostedToolKind, HostedToolRequest,
    HostedToolSupport, HostedWebSearchCapability,
};

/// The DeepSeek Anthropic-compatible Messages API implements the same hosted
/// web-search tool type as Anthropic (server-side search, page fetch, decryption
/// and answer synthesis). One tool call covers the whole pipeline.
pub(crate) const DEEPSEEK_WEB_SEARCH_TOOL_TYPE: &str = "web_search_20250305";

/// DeepSeek V4 models admitted to the hosted web-search capability. The list is
/// pinned to the models documented on the Anthropic-compatible endpoint and must
/// not be widened from an unverified model identifier.
pub(crate) fn is_hosted_web_search_model(model_name: &str) -> bool {
    matches!(model_name, "deepseek-v4-flash" | "deepseek-v4-pro")
}

/// Returns the provider-hosted web-search capability for one model on the exact
/// configured route.
///
/// Fails closed unless the Anthropic Messages client path is enabled for the
/// configured route. Never claims a capability the provider cannot actually serve.
pub(crate) fn hosted_web_search_capability(
    model_name: &str,
    messages_path_enabled: bool,
) -> HostedWebSearchCapability {
    if !messages_path_enabled || !is_hosted_web_search_model(model_name) {
        return HostedWebSearchCapability::default();
    }
    HostedWebSearchCapability {
        support: HostedToolSupport::ServerManaged,
        query_visibility: HostedQueryVisibility::ProviderReportedPostExecution,
        source_fidelity: HostedSourceFidelity::UrlAndTitle,
        citation_fidelity: HostedCitationFidelity::OutputSpan,
        max_uses_enforcement: HostedConstraintEnforcement::Hard,
        domain_filter_enforcement: HostedConstraintEnforcement::Hard,
        custom_tool_compatibility: HostedCustomToolCompatibility::Supported,
    }
}

/// Returns the single hosted web-search declaration carried by a request.
///
/// # Errors
///
/// Returns an error when the request carries more than one hosted web-search
/// declaration or the declaration fails provider-neutral validation.
pub(crate) fn hosted_web_search_request(
    hosted_tools: &[HostedToolRequest],
) -> Result<Option<&HostedToolRequest>> {
    let mut matches = hosted_tools
        .iter()
        .filter(|request| request.kind == HostedToolKind::WebSearch);
    let request = matches.next();
    if matches.next().is_some() {
        return Err(anyhow!(
            "DeepSeek request contains more than one hosted web-search declaration"
        ));
    }
    if let Some(request) = request {
        request.validate()?;
    }
    Ok(request)
}

#[cfg(test)]
#[path = "tests/hosted_search_tests.rs"]
mod tests;
