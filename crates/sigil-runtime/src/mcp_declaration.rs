use std::{
    collections::{BTreeMap, BTreeSet},
    ffi::OsStr,
    path::{Path, PathBuf},
    sync::OnceLock,
};

use serde::{Deserialize, Serialize};
use serde_json::json;
use sha2::{Digest, Sha256};
use sigil_kernel::{
    McpServerConfig, PLUGIN_MANIFEST_DIGEST_PREFIX, PluginManifest, PluginManifestSnapshot,
    PluginTrustDecision, PluginTrustEntry, plugin_manifest_digests_match,
    resolve_extension_process_environment, validate_plugin_id,
};
use sigil_mcp::McpDeclarationLaunchMetadata;
use sigil_mcp::McpProcessClass;
use uuid::Uuid;

use crate::plugin_manifest_io::{BoundedPluginManifestReadError, read_bounded_plugin_manifest};

const BUILTIN_MCP_NAMESPACE_PREFIX: &str = "builtin:";
const MAX_SAFE_DECLARATION_LABEL_CHARS: usize = 256;
const REDACTED_DECLARATION_LABEL: &str = "[redacted]";

/// Stable classification for an MCP declaration admission failure.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum McpRegistrationErrorCode {
    ReservedMcpNamespace,
    DuplicateMcpServerName,
    PluginMcpEnvironmentGrantNotSupported,
    PluginOriginAttestationMismatch,
    PluginAttestationReviewRequired,
    McpExecutionBaseUnavailable,
    McpCommandResolutionFailed,
    McpCommandSymlinkEscape,
    McpDeclarationBindingChanged,
}

impl McpRegistrationErrorCode {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::ReservedMcpNamespace => "reserved_mcp_namespace",
            Self::DuplicateMcpServerName => "duplicate_mcp_server_name",
            Self::PluginMcpEnvironmentGrantNotSupported => {
                "plugin_mcp_environment_grant_not_supported"
            }
            Self::PluginOriginAttestationMismatch => "plugin_origin_attestation_mismatch",
            Self::PluginAttestationReviewRequired => "plugin_mcp_attestation_review_required",
            Self::McpExecutionBaseUnavailable => "mcp_execution_base_unavailable",
            Self::McpCommandResolutionFailed => "mcp_command_resolution_failed",
            Self::McpCommandSymlinkEscape => "mcp_command_symlink_escape",
            Self::McpDeclarationBindingChanged => "mcp_declaration_binding_changed",
        }
    }
}

/// Typed declaration error exposed to registry, Doctor and product projections.
#[derive(Clone, PartialEq, Eq)]
pub struct McpRegistrationError {
    pub code: McpRegistrationErrorCode,
    pub declared_name: String,
    pub reason: String,
    pub safe_projection: Option<Box<McpServerDeclarationProjection>>,
}

impl std::fmt::Debug for McpRegistrationError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("McpRegistrationError")
            .field("code", &self.code)
            .field("declared_name", &bounded_safe_label(&self.declared_name))
            .field("reason", &self.reason)
            .field("safe_projection", &self.safe_projection)
            .finish()
    }
}

impl McpRegistrationError {
    fn new(
        code: McpRegistrationErrorCode,
        declared_name: impl Into<String>,
        reason: impl Into<String>,
    ) -> Self {
        Self {
            code,
            declared_name: declared_name.into(),
            reason: reason.into(),
            safe_projection: None,
        }
    }

    pub(crate) fn with_safe_projection(
        mut self,
        safe_projection: McpServerDeclarationProjection,
    ) -> Self {
        self.safe_projection = Some(Box::new(safe_projection));
        self
    }

    #[must_use]
    pub fn code(&self) -> &'static str {
        self.code.as_str()
    }
}

impl std::fmt::Display for McpRegistrationError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            formatter,
            "{} for MCP server {}: {}",
            self.code(),
            bounded_safe_label(&self.declared_name),
            self.reason
        )
    }
}

impl std::error::Error for McpRegistrationError {}

/// Runtime source of one MCP configuration declaration.
///
/// This carrier is intentionally not serializable. Durable and product surfaces must use
/// [`McpServerDeclarationProjection`].
#[derive(Clone, PartialEq, Eq)]
pub enum McpConfigOrigin {
    UserRoot,
    PluginManifest {
        plugin_id: String,
        manifest_hash: String,
        manifest_version: String,
        capability_digest: String,
        trust: PluginTrustDecision,
    },
    BuiltinReleaseProfile {
        profile_id: String,
        release_digest: String,
    },
}

impl std::fmt::Debug for McpConfigOrigin {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::UserRoot => formatter.write_str("McpConfigOrigin::UserRoot"),
            Self::PluginManifest { trust, .. } => formatter
                .debug_struct("McpConfigOrigin::PluginManifest")
                .field("identity", &"[hidden]")
                .field("trust", trust)
                .finish(),
            Self::BuiltinReleaseProfile { .. } => formatter
                .debug_struct("McpConfigOrigin::BuiltinReleaseProfile")
                .field("identity", &"[hidden]")
                .finish(),
        }
    }
}

/// Live filesystem base used to resolve an MCP stdio process.
///
/// Paths remain runtime-only and are never copied into the safe projection.
#[derive(Clone, PartialEq, Eq)]
pub enum McpExecutionBase {
    WorkspaceRoot(PathBuf),
    PluginRoot(PathBuf),
    None,
}

impl std::fmt::Debug for McpExecutionBase {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(match self {
            Self::WorkspaceRoot(_) => "McpExecutionBase::WorkspaceRoot([hidden])",
            Self::PluginRoot(_) => "McpExecutionBase::PluginRoot([hidden])",
            Self::None => "McpExecutionBase::None",
        })
    }
}

/// Runtime-only evidence binding one plugin MCP declaration to the reviewed static manifest.
#[derive(Clone, PartialEq, Eq)]
pub struct PluginManifestAttestation {
    canonical_plugin_root: PathBuf,
    canonical_manifest_path: PathBuf,
    expected_trust_manifest_path: PathBuf,
    expected_manifest_hash: String,
    expected_manifest_version: String,
    expected_capability_digest: String,
    expected_trust: PluginTrustDecision,
}

impl std::fmt::Debug for PluginManifestAttestation {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("PluginManifestAttestation")
            .field("paths", &"[hidden]")
            .field("expected_manifest_hash", &self.expected_manifest_hash)
            .field("expected_manifest_version", &self.expected_manifest_version)
            .field(
                "expected_capability_digest",
                &self.expected_capability_digest,
            )
            .field("expected_trust", &self.expected_trust)
            .finish()
    }
}

impl PluginManifestAttestation {
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn capture(
        declared_name: &str,
        canonical_plugin_root: PathBuf,
        canonical_manifest_path: PathBuf,
        expected_trust_manifest_path: PathBuf,
        expected_manifest_hash: String,
        expected_manifest_version: String,
        expected_capability_digest: String,
        expected_trust: PluginTrustDecision,
    ) -> Result<Self, McpRegistrationError> {
        let plugin_root = canonical_plugin_root.canonicalize().map_err(|_| {
            McpRegistrationError::new(
                McpRegistrationErrorCode::PluginOriginAttestationMismatch,
                declared_name,
                "plugin root is unavailable",
            )
        })?;
        let manifest_path = canonical_manifest_path.canonicalize().map_err(|_| {
            McpRegistrationError::new(
                McpRegistrationErrorCode::PluginOriginAttestationMismatch,
                declared_name,
                "plugin manifest is unavailable",
            )
        })?;
        if !manifest_path.starts_with(&plugin_root) {
            return Err(McpRegistrationError::new(
                McpRegistrationErrorCode::PluginOriginAttestationMismatch,
                declared_name,
                "plugin manifest escapes the canonical plugin root",
            ));
        }
        Ok(Self {
            canonical_plugin_root: plugin_root,
            canonical_manifest_path: manifest_path,
            expected_trust_manifest_path,
            expected_manifest_hash,
            expected_manifest_version,
            expected_capability_digest,
            expected_trust,
        })
    }
}

/// Runtime-only MCP declaration that preserves source and execution identity through registration.
#[derive(Clone, PartialEq, Eq)]
pub struct ResolvedMcpServerDeclaration {
    declared_name: String,
    config: McpServerConfig,
    origin: McpConfigOrigin,
    execution_base: McpExecutionBase,
    plugin_attestation: Option<PluginManifestAttestation>,
}

impl std::fmt::Debug for ResolvedMcpServerDeclaration {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ResolvedMcpServerDeclaration")
            .field("safe_projection", &self.safe_projection())
            .finish()
    }
}

impl ResolvedMcpServerDeclaration {
    /// Promotes one legacy root-config stdio entry into the runtime declaration carrier.
    pub fn user_root(
        config: McpServerConfig,
        workspace_root: impl AsRef<Path>,
    ) -> Result<Self, McpRegistrationError> {
        reject_reserved_namespace(&config.name)?;
        let declared_name = config.name.clone();
        let workspace_root = canonical_execution_base(&declared_name, workspace_root.as_ref())?;
        Self::new(
            declared_name,
            config,
            McpConfigOrigin::UserRoot,
            McpExecutionBase::WorkspaceRoot(workspace_root),
            None,
        )
    }

    /// Constructs one runtime-private release-profile declaration.
    ///
    /// A built-in origin does not imply `None`: a private release profile may explicitly supply a
    /// local execution base. Only `McpExecutionBase::None` blocks stdio launch.
    pub fn builtin_release_profile(
        config: McpServerConfig,
        profile_id: impl Into<String>,
        release_digest: impl Into<String>,
        execution_base: McpExecutionBase,
    ) -> Result<Self, McpRegistrationError> {
        let declared_name = config.name.clone();
        if !declared_name.starts_with(BUILTIN_MCP_NAMESPACE_PREFIX) {
            return Err(McpRegistrationError::new(
                McpRegistrationErrorCode::ReservedMcpNamespace,
                &declared_name,
                "built-in release profiles must use the reserved builtin: namespace",
            ));
        }
        let execution_base = canonicalize_optional_execution_base(&declared_name, execution_base)?;
        Self::new(
            declared_name,
            config,
            McpConfigOrigin::BuiltinReleaseProfile {
                profile_id: profile_id.into(),
                release_digest: release_digest.into(),
            },
            execution_base,
            None,
        )
    }

    pub(crate) fn plugin_manifest(
        declared_name: String,
        config: McpServerConfig,
        origin: McpConfigOrigin,
        execution_base: McpExecutionBase,
        attestation: PluginManifestAttestation,
    ) -> Result<Self, McpRegistrationError> {
        reject_reserved_namespace(&declared_name)?;
        if !config.inherit_env.is_empty() {
            return Err(McpRegistrationError::new(
                McpRegistrationErrorCode::PluginMcpEnvironmentGrantNotSupported,
                &declared_name,
                "plugin MCP declarations cannot inherit parent environment values",
            ));
        }
        let execution_base = canonicalize_optional_execution_base(&declared_name, execution_base)?;
        Self::new(
            declared_name,
            config,
            origin,
            execution_base,
            Some(attestation),
        )
    }

    fn new(
        declared_name: String,
        config: McpServerConfig,
        origin: McpConfigOrigin,
        execution_base: McpExecutionBase,
        plugin_attestation: Option<PluginManifestAttestation>,
    ) -> Result<Self, McpRegistrationError> {
        if declared_name.trim().is_empty() || config.name.trim().is_empty() {
            return Err(McpRegistrationError::new(
                McpRegistrationErrorCode::McpCommandResolutionFailed,
                declared_name,
                "MCP declaration name cannot be empty",
            ));
        }
        match (&origin, &plugin_attestation, &execution_base) {
            (
                McpConfigOrigin::PluginManifest {
                    manifest_hash,
                    manifest_version,
                    capability_digest,
                    trust,
                    ..
                },
                Some(attestation),
                McpExecutionBase::PluginRoot(plugin_root),
            ) if plugin_manifest_digests_match(
                manifest_hash,
                &attestation.expected_manifest_hash,
            ) && manifest_version == &attestation.expected_manifest_version
                && plugin_manifest_digests_match(
                    capability_digest,
                    &attestation.expected_capability_digest,
                )
                && trust == &attestation.expected_trust
                && plugin_root == &attestation.canonical_plugin_root => {}
            (McpConfigOrigin::PluginManifest { .. }, _, _) => {
                return Err(McpRegistrationError::new(
                    McpRegistrationErrorCode::PluginOriginAttestationMismatch,
                    declared_name,
                    "plugin origin, attestation and plugin execution base do not match",
                ));
            }
            (
                McpConfigOrigin::UserRoot | McpConfigOrigin::BuiltinReleaseProfile { .. },
                None,
                _,
            ) => {}
            _ => {
                return Err(McpRegistrationError::new(
                    McpRegistrationErrorCode::PluginOriginAttestationMismatch,
                    declared_name,
                    "non-plugin declarations cannot carry plugin attestation",
                ));
            }
        }
        Ok(Self {
            declared_name,
            config,
            origin,
            execution_base,
            plugin_attestation,
        })
    }

    #[must_use]
    pub fn declared_name(&self) -> &str {
        &self.declared_name
    }

    #[must_use]
    pub fn effective_name(&self) -> &str {
        &self.config.name
    }

    #[must_use]
    pub fn config(&self) -> &McpServerConfig {
        &self.config
    }

    #[must_use]
    pub fn origin(&self) -> &McpConfigOrigin {
        &self.origin
    }

    #[must_use]
    pub fn execution_base(&self) -> &McpExecutionBase {
        &self.execution_base
    }

    #[must_use]
    pub fn plugin_attestation(&self) -> Option<&PluginManifestAttestation> {
        self.plugin_attestation.as_ref()
    }

    pub(crate) fn with_effective_name(mut self, effective_name: String) -> Self {
        self.config.name = effective_name;
        self
    }

    #[must_use]
    pub(crate) fn uses_declaration_static_binding(&self) -> bool {
        !matches!(&self.origin, McpConfigOrigin::UserRoot)
    }

    /// Re-reads plugin manifest and trust evidence before any command lookup or process spawn.
    pub fn verify_activation(
        &self,
        current_plugin_trust: &[PluginTrustEntry],
    ) -> Result<(), McpRegistrationError> {
        let McpConfigOrigin::PluginManifest {
            plugin_id,
            manifest_hash,
            manifest_version,
            capability_digest,
            trust,
        } = &self.origin
        else {
            return Ok(());
        };
        let Some(attestation) = &self.plugin_attestation else {
            return Err(self.review_required("plugin attestation is missing"));
        };
        if *trust != PluginTrustDecision::Trusted
            || attestation.expected_trust != PluginTrustDecision::Trusted
        {
            return Err(self.review_required("plugin trust is no longer active"));
        }
        let latest_trust = current_plugin_trust
            .iter()
            .rev()
            .find(|entry| entry.plugin_id == *plugin_id)
            .ok_or_else(|| self.review_required("current plugin trust decision is missing"))?;

        let canonical_manifest_path = attestation
            .canonical_manifest_path
            .canonicalize()
            .map_err(|_| self.review_required("plugin manifest is unavailable"))?;
        if canonical_manifest_path != attestation.canonical_manifest_path
            || !canonical_manifest_path.starts_with(&attestation.canonical_plugin_root)
        {
            return Err(self.review_required("plugin manifest path identity changed"));
        }
        let bytes = read_bounded_plugin_manifest(&canonical_manifest_path).map_err(|error| {
            self.review_required(match error {
                BoundedPluginManifestReadError::Unavailable => "plugin manifest cannot be read",
                BoundedPluginManifestReadError::TooLarge => {
                    "plugin manifest exceeds the review size limit"
                }
            })
        })?;
        let observed_manifest_hash = format!(
            "{}{:x}",
            PLUGIN_MANIFEST_DIGEST_PREFIX,
            Sha256::digest(&bytes)
        );
        if !plugin_manifest_digests_match(manifest_hash, &observed_manifest_hash)
            || !plugin_manifest_digests_match(
                &attestation.expected_manifest_hash,
                &observed_manifest_hash,
            )
        {
            return Err(self.review_required("plugin manifest content changed"));
        }
        let raw = std::str::from_utf8(&bytes)
            .map_err(|_| self.review_required("plugin manifest is no longer valid UTF-8"))?;
        let mut manifest = toml::from_str::<PluginManifest>(raw)
            .map_err(|_| self.review_required("plugin manifest no longer parses"))?;
        manifest.root = attestation.canonical_plugin_root.clone();
        if manifest.id != *plugin_id || validate_plugin_id(&manifest.id).is_err() {
            return Err(self.review_required("plugin manifest identity changed"));
        }
        manifest
            .validate()
            .map_err(|_| self.review_required("plugin manifest validation changed"))?;
        let matching_servers = manifest
            .mcp_servers
            .iter()
            .filter(|server| server.name == self.declared_name)
            .collect::<Vec<_>>();
        if matching_servers.len() != 1 {
            return Err(self.review_required(
                "plugin MCP declaration is missing or duplicated in the current manifest",
            ));
        }
        let mut expected_effective_server = (*matching_servers[0]).clone();
        expected_effective_server.name = self.config.name.clone();
        if expected_effective_server != self.config {
            return Err(self.review_required("plugin MCP declaration fields changed"));
        }
        if manifest.version != *manifest_version
            || manifest.version != attestation.expected_manifest_version
        {
            return Err(self.review_required("plugin manifest version changed"));
        }
        let snapshot = PluginManifestSnapshot {
            plugin_id: manifest.id.clone(),
            name: manifest.name.clone(),
            version: manifest.version.clone(),
            description: manifest.description.clone(),
            manifest_path: attestation.expected_trust_manifest_path.clone(),
            manifest_hash: observed_manifest_hash,
            capabilities: manifest.capabilities(),
            trust: PluginTrustDecision::NeedsReview,
        };
        let observed_capability_digest = snapshot
            .capability_digest()
            .map_err(|_| self.review_required("plugin capability digest cannot be recomputed"))?;
        if !plugin_manifest_digests_match(capability_digest, &observed_capability_digest)
            || !plugin_manifest_digests_match(
                &attestation.expected_capability_digest,
                &observed_capability_digest,
            )
        {
            return Err(self.review_required("plugin capabilities changed"));
        }
        if latest_trust.decision != *trust || !latest_trust.matches_snapshot(&snapshot) {
            return Err(self.review_required("plugin trust decision changed or became stale"));
        }
        Ok(())
    }

    /// Resolves an exact stdio executable and cwd after plugin re-attestation.
    pub fn resolve_stdio_launch(
        &self,
        current_plugin_trust: &[PluginTrustEntry],
    ) -> Result<ResolvedMcpStdioLaunch, McpRegistrationError> {
        self.verify_activation(current_plugin_trust)?;
        let expected_cwd = match &self.execution_base {
            McpExecutionBase::WorkspaceRoot(path) | McpExecutionBase::PluginRoot(path) => path,
            McpExecutionBase::None => {
                return Err(McpRegistrationError::new(
                    McpRegistrationErrorCode::McpExecutionBaseUnavailable,
                    &self.declared_name,
                    "stdio launch requires an explicit execution base",
                )
                .with_safe_projection(self.safe_projection()));
            }
        };
        let cwd = expected_cwd.canonicalize().map_err(|_| {
            McpRegistrationError::new(
                McpRegistrationErrorCode::McpExecutionBaseUnavailable,
                &self.declared_name,
                "stdio execution base is unavailable",
            )
            .with_safe_projection(self.safe_projection())
        })?;
        if &cwd != expected_cwd {
            return Err(McpRegistrationError::new(
                McpRegistrationErrorCode::McpExecutionBaseUnavailable,
                &self.declared_name,
                "stdio execution base identity changed after declaration resolution",
            )
            .with_safe_projection(self.safe_projection()));
        }
        if !cwd.is_dir() {
            return Err(McpRegistrationError::new(
                McpRegistrationErrorCode::McpExecutionBaseUnavailable,
                &self.declared_name,
                "stdio execution base is not a directory",
            )
            .with_safe_projection(self.safe_projection()));
        }
        let executable = resolve_stdio_executable(&self.declared_name, &self.config, &cwd)?;
        let classification = match &self.origin {
            McpConfigOrigin::PluginManifest { .. } => McpProcessClass::LocalStdioPluginDeclared,
            McpConfigOrigin::UserRoot | McpConfigOrigin::BuiltinReleaseProfile { .. } => {
                McpProcessClass::LocalStdioConfigured
            }
        };
        Ok(ResolvedMcpStdioLaunch {
            executable,
            cwd,
            classification,
        })
    }

    #[must_use]
    pub fn safe_projection(&self) -> McpServerDeclarationProjection {
        let (
            origin_kind,
            origin_id,
            manifest_hash,
            manifest_version,
            capability_digest,
            release_digest,
            trust,
        ) = match &self.origin {
            McpConfigOrigin::UserRoot => (
                McpConfigOriginKind::UserRoot,
                None,
                None,
                None,
                None,
                None,
                None,
            ),
            McpConfigOrigin::PluginManifest {
                plugin_id,
                manifest_hash,
                manifest_version,
                capability_digest,
                trust,
            } => (
                McpConfigOriginKind::PluginManifest,
                Some(bounded_safe_label(plugin_id)),
                Some(bounded_safe_integrity_label(manifest_hash)),
                Some(bounded_safe_label(manifest_version)),
                Some(bounded_safe_integrity_label(capability_digest)),
                None,
                Some(*trust),
            ),
            McpConfigOrigin::BuiltinReleaseProfile {
                profile_id,
                release_digest,
            } => (
                McpConfigOriginKind::BuiltinReleaseProfile,
                Some(bounded_safe_label(profile_id)),
                None,
                None,
                None,
                Some(bounded_safe_integrity_label(release_digest)),
                None,
            ),
        };
        McpServerDeclarationProjection {
            declared_name: bounded_safe_label(&self.declared_name),
            effective_name: bounded_safe_label(&self.config.name),
            origin_kind,
            origin_id,
            execution_base_kind: match &self.execution_base {
                McpExecutionBase::WorkspaceRoot(_) => McpExecutionBaseKind::WorkspaceRoot,
                McpExecutionBase::PluginRoot(_) => McpExecutionBaseKind::PluginRoot,
                McpExecutionBase::None => McpExecutionBaseKind::None,
            },
            manifest_hash,
            manifest_version,
            capability_digest,
            release_digest,
            trust,
            declaration_fingerprint: safe_declaration_fingerprint(self),
        }
    }

    pub(crate) fn launch_metadata(
        &self,
        launch: &ResolvedMcpStdioLaunch,
        transport_static_fingerprint: &str,
        environment_live_fingerprint: &str,
    ) -> McpDeclarationLaunchMetadata {
        let projection = self.safe_projection();
        McpDeclarationLaunchMetadata {
            declared_name: projection.declared_name,
            effective_name: projection.effective_name,
            origin_kind: origin_kind_label(projection.origin_kind).to_owned(),
            origin_id: projection.origin_id,
            execution_base_kind: execution_base_kind_label(projection.execution_base_kind)
                .to_owned(),
            manifest_hash: projection.manifest_hash,
            manifest_version: projection.manifest_version,
            capability_digest: projection.capability_digest,
            release_digest: projection.release_digest,
            trust: projection.trust.map(|trust| trust.as_str().to_owned()),
            projection_fingerprint: projection.declaration_fingerprint,
            authorization_fingerprint: keyed_launch_authorization_fingerprint(
                self,
                launch,
                transport_static_fingerprint,
                environment_live_fingerprint,
            ),
        }
    }

    fn review_required(&self, reason: impl Into<String>) -> McpRegistrationError {
        McpRegistrationError::new(
            McpRegistrationErrorCode::PluginAttestationReviewRequired,
            &self.declared_name,
            reason,
        )
        .with_safe_projection(self.safe_projection())
    }
}

/// Exact live stdio launch material produced from a resolved declaration.
#[derive(Clone, PartialEq, Eq)]
pub struct ResolvedMcpStdioLaunch {
    pub executable: PathBuf,
    pub cwd: PathBuf,
    pub classification: McpProcessClass,
}

impl std::fmt::Debug for ResolvedMcpStdioLaunch {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ResolvedMcpStdioLaunch")
            .field("executable", &"[hidden]")
            .field("cwd", &"[hidden]")
            .field("classification", &self.classification)
            .finish()
    }
}

/// Safe origin kind for durable audit and product surfaces.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum McpConfigOriginKind {
    UserRoot,
    PluginManifest,
    BuiltinReleaseProfile,
}

impl McpConfigOriginKind {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        origin_kind_label(self)
    }
}

/// Safe execution-base kind for durable audit and product surfaces.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum McpExecutionBaseKind {
    WorkspaceRoot,
    PluginRoot,
    None,
}

impl McpExecutionBaseKind {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        execution_base_kind_label(self)
    }
}

/// Serializable declaration projection that deliberately excludes live filesystem paths.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub struct McpServerDeclarationProjection {
    pub declared_name: String,
    pub effective_name: String,
    pub origin_kind: McpConfigOriginKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub origin_id: Option<String>,
    pub execution_base_kind: McpExecutionBaseKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub manifest_hash: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub manifest_version: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub capability_digest: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub release_digest: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub trust: Option<PluginTrustDecision>,
    pub declaration_fingerprint: String,
}

/// Promotes existing root-config MCP entries while retaining their original declared names.
pub fn resolve_user_root_mcp_declarations(
    servers: &[McpServerConfig],
    workspace_root: impl AsRef<Path>,
) -> Result<Vec<ResolvedMcpServerDeclaration>, McpRegistrationError> {
    let mut declarations = Vec::with_capacity(servers.len());
    let mut names = BTreeSet::new();
    for server in servers {
        if !names.insert(server.name.clone()) {
            return Err(McpRegistrationError::new(
                McpRegistrationErrorCode::DuplicateMcpServerName,
                &server.name,
                "root MCP server names must be unique",
            ));
        }
        declarations.push(ResolvedMcpServerDeclaration::user_root(
            server.clone(),
            workspace_root.as_ref(),
        )?);
    }
    Ok(declarations)
}

pub(crate) fn declarations_by_effective_name(
    declarations: &[ResolvedMcpServerDeclaration],
) -> Result<BTreeMap<String, ResolvedMcpServerDeclaration>, McpRegistrationError> {
    let mut result = BTreeMap::new();
    for declaration in declarations {
        if result
            .insert(declaration.effective_name().to_owned(), declaration.clone())
            .is_some()
        {
            return Err(McpRegistrationError::new(
                McpRegistrationErrorCode::DuplicateMcpServerName,
                declaration.declared_name(),
                "effective MCP server names must be unique before registration",
            ));
        }
    }
    Ok(result)
}

fn reject_reserved_namespace(name: &str) -> Result<(), McpRegistrationError> {
    if name.starts_with(BUILTIN_MCP_NAMESPACE_PREFIX) {
        return Err(McpRegistrationError::new(
            McpRegistrationErrorCode::ReservedMcpNamespace,
            name,
            "the builtin: MCP namespace is reserved for runtime release profiles",
        ));
    }
    Ok(())
}

fn canonical_execution_base(
    declared_name: &str,
    path: &Path,
) -> Result<PathBuf, McpRegistrationError> {
    let canonical = path.canonicalize().map_err(|_| {
        McpRegistrationError::new(
            McpRegistrationErrorCode::McpExecutionBaseUnavailable,
            declared_name,
            "execution base is unavailable",
        )
    })?;
    if !canonical.is_dir() {
        return Err(McpRegistrationError::new(
            McpRegistrationErrorCode::McpExecutionBaseUnavailable,
            declared_name,
            "execution base is not a directory",
        ));
    }
    Ok(canonical)
}

fn canonicalize_optional_execution_base(
    declared_name: &str,
    execution_base: McpExecutionBase,
) -> Result<McpExecutionBase, McpRegistrationError> {
    match execution_base {
        McpExecutionBase::WorkspaceRoot(path) => Ok(McpExecutionBase::WorkspaceRoot(
            canonical_execution_base(declared_name, &path)?,
        )),
        McpExecutionBase::PluginRoot(path) => Ok(McpExecutionBase::PluginRoot(
            canonical_execution_base(declared_name, &path)?,
        )),
        McpExecutionBase::None => Ok(McpExecutionBase::None),
    }
}

fn resolve_stdio_executable(
    declared_name: &str,
    config: &McpServerConfig,
    cwd: &Path,
) -> Result<PathBuf, McpRegistrationError> {
    if config.command.is_empty() {
        return Err(McpRegistrationError::new(
            McpRegistrationErrorCode::McpCommandResolutionFailed,
            declared_name,
            "stdio command cannot be empty",
        ));
    }
    let command_path = Path::new(&config.command);
    if command_path.is_absolute() {
        return canonical_command(declared_name, command_path.to_path_buf());
    }
    if config.command.contains('/')
        || config.command.contains('\\')
        || command_path.components().count() > 1
    {
        let executable = canonical_command(declared_name, cwd.join(command_path))?;
        if !executable.starts_with(cwd) {
            return Err(McpRegistrationError::new(
                McpRegistrationErrorCode::McpCommandSymlinkEscape,
                declared_name,
                "relative stdio command escapes its execution base",
            ));
        }
        return Ok(executable);
    }

    let environment = resolve_extension_process_environment(&config.inherit_env).map_err(|_| {
        McpRegistrationError::new(
            McpRegistrationErrorCode::McpCommandResolutionFailed,
            declared_name,
            "isolated MCP environment could not be resolved",
        )
    })?;
    let path = environment.variable("PATH").ok_or_else(|| {
        McpRegistrationError::new(
            McpRegistrationErrorCode::McpCommandResolutionFailed,
            declared_name,
            "isolated MCP environment has no controlled PATH",
        )
    })?;
    resolve_bare_stdio_executable(
        declared_name,
        &config.command,
        cwd,
        OsStr::new(path.expose_secret()),
        environment
            .variable("PATHEXT")
            .map(|value| value.expose_secret()),
    )
}

fn resolve_bare_stdio_executable(
    declared_name: &str,
    command: &str,
    cwd: &Path,
    search_path: &OsStr,
    path_extensions: Option<&str>,
) -> Result<PathBuf, McpRegistrationError> {
    for entry in std::env::split_paths(search_path) {
        let directory = if entry.is_absolute() {
            entry
        } else {
            cwd.join(entry)
        };
        for candidate in executable_candidates(&directory, command, path_extensions) {
            if candidate.is_file() {
                return canonical_command(declared_name, candidate);
            }
        }
    }
    Err(McpRegistrationError::new(
        McpRegistrationErrorCode::McpCommandResolutionFailed,
        declared_name,
        "bare stdio command was not found on the controlled PATH",
    ))
}

fn canonical_command(
    declared_name: &str,
    candidate: PathBuf,
) -> Result<PathBuf, McpRegistrationError> {
    let canonical = candidate.canonicalize().map_err(|_| {
        McpRegistrationError::new(
            McpRegistrationErrorCode::McpCommandResolutionFailed,
            declared_name,
            "stdio command does not resolve to an existing file",
        )
    })?;
    if !canonical.is_file() {
        return Err(McpRegistrationError::new(
            McpRegistrationErrorCode::McpCommandResolutionFailed,
            declared_name,
            "stdio command does not resolve to a file",
        ));
    }
    if !is_launchable_executable(&canonical) {
        return Err(McpRegistrationError::new(
            McpRegistrationErrorCode::McpCommandResolutionFailed,
            declared_name,
            "stdio command is not executable on this platform",
        ));
    }
    Ok(canonical)
}

#[cfg(unix)]
fn is_launchable_executable(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;

    path.metadata()
        .map(|metadata| metadata.permissions().mode() & 0o111 != 0)
        .unwrap_or(false)
}

#[cfg(windows)]
fn is_launchable_executable(path: &Path) -> bool {
    path.extension()
        .and_then(OsStr::to_str)
        .is_some_and(|extension| {
            matches!(
                extension.to_ascii_lowercase().as_str(),
                "exe" | "com" | "bat" | "cmd"
            )
        })
}

#[cfg(not(any(unix, windows)))]
fn is_launchable_executable(path: &Path) -> bool {
    path.is_file()
}

#[cfg(not(windows))]
fn executable_candidates(
    directory: &Path,
    command: &str,
    _path_extensions: Option<&str>,
) -> Vec<PathBuf> {
    vec![directory.join(command)]
}

#[cfg(windows)]
fn executable_candidates(
    directory: &Path,
    command: &str,
    path_extensions: Option<&str>,
) -> Vec<PathBuf> {
    let command_path = Path::new(command);
    if command_path.extension().is_some() {
        return vec![directory.join(command_path)];
    }
    let path_extensions = path_extensions.unwrap_or(".COM;.EXE;.BAT;.CMD");
    path_extensions
        .split(';')
        .filter(|extension| !extension.is_empty())
        .map(|extension| directory.join(format!("{command}{extension}")))
        .collect()
}

fn safe_declaration_fingerprint(declaration: &ResolvedMcpServerDeclaration) -> String {
    let (origin, execution_base_kind) = match &declaration.origin {
        McpConfigOrigin::UserRoot => (
            json!({ "kind": "user_root" }),
            match &declaration.execution_base {
                McpExecutionBase::WorkspaceRoot(_) => "workspace_root",
                McpExecutionBase::PluginRoot(_) => "plugin_root",
                McpExecutionBase::None => "none",
            },
        ),
        McpConfigOrigin::PluginManifest {
            plugin_id,
            manifest_hash,
            manifest_version,
            capability_digest,
            trust,
        } => (
            json!({
                "kind": "plugin_manifest",
                "plugin_id": bounded_safe_label(plugin_id),
                "manifest_hash": bounded_safe_integrity_label(manifest_hash),
                "manifest_version": bounded_safe_label(manifest_version),
                "capability_digest": bounded_safe_integrity_label(capability_digest),
                "trust": trust.as_str(),
            }),
            match &declaration.execution_base {
                McpExecutionBase::WorkspaceRoot(_) => "workspace_root",
                McpExecutionBase::PluginRoot(_) => "plugin_root",
                McpExecutionBase::None => "none",
            },
        ),
        McpConfigOrigin::BuiltinReleaseProfile {
            profile_id,
            release_digest,
        } => (
            json!({
                "kind": "builtin_release_profile",
                "profile_id": bounded_safe_label(profile_id),
                "release_digest": bounded_safe_integrity_label(release_digest),
            }),
            match &declaration.execution_base {
                McpExecutionBase::WorkspaceRoot(_) => "workspace_root",
                McpExecutionBase::PluginRoot(_) => "plugin_root",
                McpExecutionBase::None => "none",
            },
        ),
    };
    // This is a projection identity, not a launch authorization fingerprint. It intentionally
    // excludes command, args and canonical paths because their plain digest can disclose
    // low-entropy secrets or enable offline path inference.
    let material = serde_json::to_vec(&json!({
        "declared_name": bounded_safe_label(&declaration.declared_name),
        "effective_name": bounded_safe_label(&declaration.config.name),
        "origin": origin,
        "execution_base_kind": execution_base_kind,
    }))
    .unwrap_or_default();
    format!("sha256:{:x}", Sha256::digest(material))
}

fn origin_kind_label(kind: McpConfigOriginKind) -> &'static str {
    match kind {
        McpConfigOriginKind::UserRoot => "user_root",
        McpConfigOriginKind::PluginManifest => "plugin_manifest",
        McpConfigOriginKind::BuiltinReleaseProfile => "builtin_release_profile",
    }
}

fn execution_base_kind_label(kind: McpExecutionBaseKind) -> &'static str {
    match kind {
        McpExecutionBaseKind::WorkspaceRoot => "workspace_root",
        McpExecutionBaseKind::PluginRoot => "plugin_root",
        McpExecutionBaseKind::None => "none",
    }
}

fn bounded_safe_label(value: &str) -> String {
    let value = value.trim();
    if value.is_empty()
        || value.chars().any(char::is_control)
        || looks_path_like(value)
        || looks_like_secret(value)
    {
        return REDACTED_DECLARATION_LABEL.to_owned();
    }
    let mut sanitized = value
        .chars()
        .map(|character| {
            if character.is_alphanumeric() || matches!(character, '.' | '_' | '-' | ':') {
                character
            } else {
                '_'
            }
        })
        .take(MAX_SAFE_DECLARATION_LABEL_CHARS)
        .collect::<String>();
    if sanitized.is_empty() {
        sanitized.push_str(REDACTED_DECLARATION_LABEL);
    }
    sanitized
}

fn bounded_safe_integrity_label(value: &str) -> String {
    let Some(digest) = value.strip_prefix(PLUGIN_MANIFEST_DIGEST_PREFIX) else {
        return REDACTED_DECLARATION_LABEL.to_owned();
    };
    if digest.len() != 64 || !digest.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return REDACTED_DECLARATION_LABEL.to_owned();
    }
    format!(
        "{PLUGIN_MANIFEST_DIGEST_PREFIX}{}",
        digest.to_ascii_lowercase()
    )
}

fn looks_path_like(value: &str) -> bool {
    Path::new(value).is_absolute()
        || value.contains('/')
        || value.contains('\\')
        || value.starts_with('~')
        || value.contains("..")
        || (value.len() >= 2
            && value.as_bytes()[0].is_ascii_alphabetic()
            && value.as_bytes()[1] == b':')
}

fn looks_like_secret(value: &str) -> bool {
    let lowercase = value.to_ascii_lowercase();
    const SECRET_MARKERS: &[&str] = &[
        "api_key",
        "apikey",
        "access_token",
        "auth_token",
        "authorization",
        "bearer",
        "password",
        "private_key",
        "secret",
    ];
    const SECRET_PREFIXES: &[&str] = &[
        "sk-",
        "ghp_",
        "github_pat_",
        "xoxb-",
        "xoxp-",
        "akia",
        "aiza",
    ];
    SECRET_MARKERS
        .iter()
        .any(|marker| lowercase.contains(marker))
        || SECRET_PREFIXES
            .iter()
            .any(|prefix| lowercase.starts_with(prefix))
        || (value.len() >= 48 && !value.chars().any(char::is_whitespace))
}

fn keyed_launch_authorization_fingerprint(
    declaration: &ResolvedMcpServerDeclaration,
    launch: &ResolvedMcpStdioLaunch,
    transport_static_fingerprint: &str,
    environment_live_fingerprint: &str,
) -> String {
    static KEY: OnceLock<[u8; 32]> = OnceLock::new();
    let key = KEY.get_or_init(|| {
        let first = Uuid::new_v4();
        let second = Uuid::new_v4();
        let mut key = [0_u8; 32];
        key[..16].copy_from_slice(first.as_bytes());
        key[16..].copy_from_slice(second.as_bytes());
        key
    });
    let execution_base_path = match &declaration.execution_base {
        McpExecutionBase::WorkspaceRoot(path) | McpExecutionBase::PluginRoot(path) => {
            Some(path.to_string_lossy())
        }
        McpExecutionBase::None => None,
    };
    let exact_origin = match &declaration.origin {
        McpConfigOrigin::UserRoot => json!({
            "kind": "user_root",
        }),
        McpConfigOrigin::PluginManifest {
            plugin_id,
            manifest_hash,
            manifest_version,
            capability_digest,
            trust,
        } => json!({
            "kind": "plugin_manifest",
            "plugin_id": plugin_id,
            "manifest_hash": manifest_hash,
            "manifest_version": manifest_version,
            "capability_digest": capability_digest,
            "trust": trust.as_str(),
        }),
        McpConfigOrigin::BuiltinReleaseProfile {
            profile_id,
            release_digest,
        } => json!({
            "kind": "builtin_release_profile",
            "profile_id": profile_id,
            "release_digest": release_digest,
        }),
    };
    let material = serde_json::to_vec(&json!({
        "declared_name": declaration.declared_name,
        "effective_name": declaration.config.name,
        "origin": exact_origin,
        "execution_base_path": execution_base_path,
        "command": declaration.config.command,
        "args": declaration.config.args,
        "resolved_executable": launch.executable.to_string_lossy(),
        "resolved_cwd": launch.cwd.to_string_lossy(),
        "transport_static_fingerprint": transport_static_fingerprint,
        "environment_live_fingerprint": environment_live_fingerprint,
    }))
    .unwrap_or_default();
    let mut inner_pad = [0x36_u8; 64];
    let mut outer_pad = [0x5c_u8; 64];
    for (index, byte) in key.iter().enumerate() {
        inner_pad[index] ^= byte;
        outer_pad[index] ^= byte;
    }
    let mut inner = Sha256::new();
    inner.update(inner_pad);
    inner.update(&material);
    let inner_digest = inner.finalize();
    let mut outer = Sha256::new();
    outer.update(outer_pad);
    outer.update(inner_digest);
    format!("hmac-sha256:{:x}", outer.finalize())
}

#[cfg(test)]
#[path = "tests/mcp_declaration_tests.rs"]
mod tests;
