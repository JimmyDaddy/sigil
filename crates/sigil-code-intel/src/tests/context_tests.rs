use sigil_kernel::{ContextInclusionReason, ContextSensitivity, ContextSource, ContextTrustLevel};

use super::*;

fn range() -> CodeRange {
    CodeRange {
        start_line: 10,
        start_character: 2,
        end_line: 10,
        end_character: 12,
    }
}

#[test]
fn context_code_symbol_diagnostic_and_reference_hits_keep_lsp_provenance() {
    let builder = CodeContextBuilder::new().source_event_id("event-code-context");
    let symbol = CodeSymbol {
        name: "parse_config".to_owned(),
        kind: "function".to_owned(),
        path: "src/config.rs".to_owned(),
        range: range(),
        container_name: Some("config".to_owned()),
    };
    let diagnostic = CodeDiagnostic {
        path: "src/config.rs".to_owned(),
        range: range(),
        severity: "warning".to_owned(),
        message: "unused result".to_owned(),
        source: Some("rust-analyzer".to_owned()),
    };
    let reference = CodeLocation {
        path: "src/main.rs".to_owned(),
        range: range(),
        preview: Some("parse_config()".to_owned()),
    };

    let symbol_hit = builder.symbol_hit(&symbol);
    let diagnostic_hit = builder.diagnostic_hit(&diagnostic);
    let reference_hit = builder.reference_hit(&reference);

    assert_eq!(symbol_hit.item.source, ContextSource::LspSymbol);
    assert_eq!(diagnostic_hit.item.source, ContextSource::LspDiagnostic);
    assert_eq!(reference_hit.item.source, ContextSource::LspReference);
    assert_eq!(
        symbol_hit.item.source_event_id.as_deref(),
        Some("event-code-context")
    );
    assert_eq!(
        symbol_hit.item.trust_level,
        ContextTrustLevel::UntrustedRepositoryData
    );
    assert_eq!(
        diagnostic_hit.item.sensitivity,
        ContextSensitivity::Repository
    );
    assert_eq!(
        reference_hit.item.inclusion_reason,
        ContextInclusionReason::RetrievalHit
    );
    assert!(symbol_hit.snippet.contains("parse_config"));
    assert!(diagnostic_hit.snippet.contains("unused result"));
    assert!(reference_hit.snippet.contains("parse_config()"));
    symbol_hit
        .item
        .validate()
        .expect("symbol context item is valid");
    diagnostic_hit
        .item
        .validate()
        .expect("diagnostic context item is valid");
    reference_hit
        .item
        .validate()
        .expect("reference context item is valid");
}

#[test]
fn context_repo_file_and_diff_hits_apply_secret_egress_filtering() {
    let blocked_secret = CodeContextBuilder::new()
        .sensitivity(ContextSensitivity::Secret)
        .repo_file_hit(".env", "OPENAI_API_KEY=secret");
    let approved_secret = CodeContextBuilder::new()
        .sensitivity(ContextSensitivity::Secret)
        .egress_decision("egress-approved-1")
        .repo_file_hit(".env", "OPENAI_API_KEY=secret");
    let diff = CodeContextBuilder::new().current_diff_hit("src/lib.rs", "+fn new_api() {}");

    assert_eq!(blocked_secret.item.source, ContextSource::RepositoryFile);
    assert_eq!(
        blocked_secret.item.inclusion_reason,
        ContextInclusionReason::ExcludedSecret
    );
    blocked_secret
        .item
        .validate()
        .expect("excluded secret can be represented");

    assert_eq!(
        approved_secret.item.inclusion_reason,
        ContextInclusionReason::RetrievalHit
    );
    assert_eq!(
        approved_secret.item.egress_decision.as_deref(),
        Some("egress-approved-1")
    );
    approved_secret
        .item
        .validate()
        .expect("approved secret has egress decision");

    assert_eq!(diff.item.source, ContextSource::CurrentDiff);
    assert_eq!(
        diff.item.inclusion_reason,
        ContextInclusionReason::RetrievalHit
    );
    assert!(diff.snippet.contains("+fn new_api"));
}
