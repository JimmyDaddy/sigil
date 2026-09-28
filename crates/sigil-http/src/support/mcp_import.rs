use super::*;
use crate::dto::{
    HttpMcpImportApplyRequest, HttpMcpImportApplyResult, HttpMcpImportCandidate,
    HttpMcpImportPreview,
};
use sigil_resource_authority::configuration::{
    BORROWED_CONFIGURATION_SCHEMA_VERSION, BorrowedConfigurationOperationV1,
    BorrowedConfigurationReceiptV1, BorrowedConfigurationRequestV1,
};
use sigil_runtime::mcp_import::{McpConfigurationImport, preview_mcp_configuration_import};

/// An in-memory editing draft, never a process or permission grant.
pub(super) struct PreparedMcpImport {
    id: String,
    current: RootConfig,
    expected_hash: Option<CanonicalHash>,
    import: McpConfigurationImport,
}

#[derive(Debug, Clone, Copy)]
pub(crate) enum HttpMcpImportFailure {
    Invalid,
    Stale,
    Unavailable,
}

impl HttpSupportContext {
    pub(crate) fn preview_mcp_import(
        &self,
        bytes: &[u8],
    ) -> std::result::Result<HttpMcpImportPreview, HttpMcpImportFailure> {
        let import =
            preview_mcp_configuration_import(bytes).map_err(|_| HttpMcpImportFailure::Invalid)?;
        let (current, expected_hash) = match fs::read(&self.config_path) {
            Ok(bytes) => {
                let raw = std::str::from_utf8(&bytes).map_err(|_| HttpMcpImportFailure::Invalid)?;
                let current =
                    RootConfig::parse_persisted(raw).map_err(|_| HttpMcpImportFailure::Invalid)?;
                (current, Some(configuration_digest(&bytes)))
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                (default_setup_root_config(), None)
            }
            Err(_) => return Err(HttpMcpImportFailure::Unavailable),
        };
        let id = uuid::Uuid::new_v4().to_string();
        let preview = HttpMcpImportPreview {
            preview_id: id.clone(),
            root_fields_ignored: import.root_fields_ignored(),
            candidates: import
                .summaries()
                .iter()
                .map(|candidate| HttpMcpImportCandidate {
                    index: candidate.index,
                    name: candidate.name.clone(),
                    transport: candidate.transport.clone(),
                    description: candidate.description.clone(),
                    importable: candidate.importable,
                    issues: candidate
                        .issues
                        .iter()
                        .map(|issue| {
                            use sigil_runtime::mcp_import::McpImportIssue::*;
                            match issue {
                                InvalidServerObject => "invalid_server_object",
                                InvalidConfiguration => "invalid_configuration",
                                AmbiguousTransport => "ambiguous_transport",
                                UnsupportedTransport => "unsupported_transport",
                                IgnoredFields => "ignored_fields",
                                EnvironmentValuesNotImported => "environment_values_not_imported",
                                HeaderValuesNotImported => "header_values_not_imported",
                                CommandArgumentsWillBeSaved => "command_arguments_will_be_saved",
                            }
                            .to_owned()
                        })
                        .collect(),
                })
                .collect(),
        };
        *self
            .mcp_import
            .lock()
            .map_err(|_| HttpMcpImportFailure::Unavailable)? = Some(PreparedMcpImport {
            id,
            current,
            expected_hash,
            import,
        });
        Ok(preview)
    }

    pub(crate) fn apply_mcp_import(
        &self,
        request: HttpMcpImportApplyRequest,
        capsule_id: OpaqueRegistrationCapsuleId,
    ) -> std::result::Result<
        (HttpMcpImportApplyResult, BorrowedConfigurationReceiptV1),
        HttpMcpImportFailure,
    > {
        let mut draft = self
            .mcp_import
            .lock()
            .map_err(|_| HttpMcpImportFailure::Unavailable)?;
        let prepared = draft
            .as_ref()
            .filter(|draft| draft.id == request.preview_id)
            .ok_or(HttpMcpImportFailure::Stale)?;
        let mut next = prepared.current.clone();
        next.mcp_servers = prepared
            .import
            .selected_configurations(&request.selected_indices, &prepared.current.mcp_servers)
            .map_err(|_| HttpMcpImportFailure::Invalid)?;
        let imported_names = request
            .selected_indices
            .iter()
            .map(|index| prepared.import.summaries()[*index].name.clone())
            .collect();
        // Check the editing source before calling the authority's atomic versioned publisher.
        let current_hash = match fs::read(&self.config_path) {
            Ok(bytes) => Some(configuration_digest(&bytes)),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
            Err(_) => return Err(HttpMcpImportFailure::Unavailable),
        };
        if current_hash != prepared.expected_hash {
            return Err(HttpMcpImportFailure::Stale);
        }
        let receipt = self
            .borrowed_configuration_service
            .as_ref()
            .ok_or(HttpMcpImportFailure::Unavailable)?
            .publish(BorrowedConfigurationRequestV1 {
                schema_version: BORROWED_CONFIGURATION_SCHEMA_VERSION,
                capsule_id,
                operation: if prepared.expected_hash.is_some() {
                    BorrowedConfigurationOperationV1::VersionedReplace
                } else {
                    BorrowedConfigurationOperationV1::Bootstrap
                },
                expected_current_hash: prepared.expected_hash,
                config: next,
            })
            .map_err(|_| HttpMcpImportFailure::Unavailable)?;
        *draft = None;
        Ok((HttpMcpImportApplyResult { imported_names }, receipt))
    }
}

#[cfg(test)]
#[path = "../tests/mcp_import_support_tests.rs"]
mod tests;
