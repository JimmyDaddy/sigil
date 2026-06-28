use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use sigil_kernel::{
    ContextBodyRef, ContextEgressDecisionId, ContextInclusionReason, ContextItem, ContextItemId,
    ContextSensitivity, ContextSource, ContextTrustLevel, EventId, estimate_context_token_cost,
};

use crate::service::{CodeDiagnostic, CodeLocation, CodeRange, CodeSymbol};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub struct CodeContextHit {
    pub item: ContextItem,
    pub path: PathBuf,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub range: Option<CodeRange>,
    pub snippet: String,
}

#[derive(Debug, Clone)]
pub struct CodeContextBuilder {
    trust_level: ContextTrustLevel,
    sensitivity: ContextSensitivity,
    egress_decision: Option<ContextEgressDecisionId>,
    source_event_id: Option<EventId>,
}

impl Default for CodeContextBuilder {
    fn default() -> Self {
        Self {
            trust_level: ContextTrustLevel::UntrustedRepositoryData,
            sensitivity: ContextSensitivity::Repository,
            egress_decision: None,
            source_event_id: None,
        }
    }
}

#[cfg(test)]
#[path = "tests/context_tests.rs"]
mod tests;

impl CodeContextBuilder {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    #[must_use]
    pub fn trust_level(mut self, trust_level: ContextTrustLevel) -> Self {
        self.trust_level = trust_level;
        self
    }

    #[must_use]
    pub fn sensitivity(mut self, sensitivity: ContextSensitivity) -> Self {
        self.sensitivity = sensitivity;
        self
    }

    #[must_use]
    pub fn egress_decision(mut self, egress_decision: impl Into<ContextEgressDecisionId>) -> Self {
        self.egress_decision = Some(egress_decision.into());
        self
    }

    #[must_use]
    pub fn source_event_id(mut self, source_event_id: impl Into<EventId>) -> Self {
        self.source_event_id = Some(source_event_id.into());
        self
    }

    #[must_use]
    pub fn symbol_hit(&self, symbol: &CodeSymbol) -> CodeContextHit {
        let snippet = format!(
            "{} {} at {}:{}",
            symbol.kind, symbol.name, symbol.path, symbol.range.start_line
        );
        self.hit(
            format!("lsp-symbol:{}:{}", symbol.path, symbol.name),
            ContextSource::LspSymbol,
            PathBuf::from(&symbol.path),
            Some(symbol.range.clone()),
            snippet,
        )
    }

    #[must_use]
    pub fn diagnostic_hit(&self, diagnostic: &CodeDiagnostic) -> CodeContextHit {
        let snippet = format!(
            "{} diagnostic at {}:{}: {}",
            diagnostic.severity, diagnostic.path, diagnostic.range.start_line, diagnostic.message
        );
        self.hit(
            format!(
                "lsp-diagnostic:{}:{}:{}",
                diagnostic.path, diagnostic.range.start_line, diagnostic.message
            ),
            ContextSource::LspDiagnostic,
            PathBuf::from(&diagnostic.path),
            Some(diagnostic.range.clone()),
            snippet,
        )
    }

    #[must_use]
    pub fn reference_hit(&self, location: &CodeLocation) -> CodeContextHit {
        let preview = location.preview.as_deref().unwrap_or("reference");
        let snippet = format!(
            "reference at {}:{}: {}",
            location.path, location.range.start_line, preview
        );
        self.hit(
            format!(
                "lsp-reference:{}:{}",
                location.path, location.range.start_line
            ),
            ContextSource::LspReference,
            PathBuf::from(&location.path),
            Some(location.range.clone()),
            snippet,
        )
    }

    #[must_use]
    pub fn repo_file_hit(
        &self,
        path: impl Into<PathBuf>,
        body: impl Into<String>,
    ) -> CodeContextHit {
        let path = path.into();
        let snippet = body.into();
        self.hit(
            format!("repo-file:{}", path.display()),
            ContextSource::RepositoryFile,
            path,
            None,
            snippet,
        )
    }

    #[must_use]
    pub fn current_diff_hit(
        &self,
        path: impl Into<PathBuf>,
        diff: impl Into<String>,
    ) -> CodeContextHit {
        let path = path.into();
        let snippet = diff.into();
        self.hit(
            format!("current-diff:{}", path.display()),
            ContextSource::CurrentDiff,
            path,
            None,
            snippet,
        )
    }

    fn hit(
        &self,
        id: ContextItemId,
        source: ContextSource,
        path: PathBuf,
        range: Option<CodeRange>,
        snippet: String,
    ) -> CodeContextHit {
        let inclusion_reason =
            if self.sensitivity == ContextSensitivity::Secret && self.egress_decision.is_none() {
                ContextInclusionReason::ExcludedSecret
            } else {
                ContextInclusionReason::RetrievalHit
            };
        let item = ContextItem {
            id,
            source,
            source_event_id: self.source_event_id.clone(),
            trust_level: self.trust_level,
            sensitivity: self.sensitivity,
            egress_decision: self.egress_decision.clone(),
            repo_revision: None,
            token_cost: estimate_context_token_cost(&snippet),
            score: None,
            inclusion_reason,
            body_ref: ContextBodyRef::inline(&snippet),
        };
        CodeContextHit {
            item,
            path,
            range,
            snippet,
        }
    }
}
