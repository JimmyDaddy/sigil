use std::{collections::BTreeSet, fmt};

use serde::{Deserialize, Serialize};

use super::composition_document::DeferredModuleConfig;

/// Product capability selection. Core always retains the provider, ordinary tools, permission,
/// durable session and application authority contracts.
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum RuntimeCompositionProfile {
    Core,
    #[default]
    Standard,
}

/// Optional runtime owners that may be selected before their configuration is interpreted.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[serde(rename_all = "snake_case")]
pub enum OptionalCapability {
    TaskOrchestration,
    Memory,
    Skills,
    CodeIntelligence,
    RepositoryContext,
    Web,
    Mcp,
    Terminal,
    ChangeSets,
    SessionTitles,
    Compaction,
    Updater,
}

impl OptionalCapability {
    /// Closed set used to construct an auditable capability selection.
    pub const ALL: [Self; 12] = [
        Self::TaskOrchestration,
        Self::Memory,
        Self::Skills,
        Self::CodeIntelligence,
        Self::RepositoryContext,
        Self::Web,
        Self::Mcp,
        Self::Terminal,
        Self::ChangeSets,
        Self::SessionTitles,
        Self::Compaction,
        Self::Updater,
    ];
}

/// Root composition selection. Module enable flags can restrict this selection further.
///
/// Deferred module payloads belong only to root-config roundtripping and are never serialized or
/// included in debug output for this selection contract. Use `selection_only` at audit boundaries.
#[derive(Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct RuntimeCompositionConfig {
    #[serde(default)]
    pub profile: RuntimeCompositionProfile,
    #[serde(default)]
    pub enhancements: BTreeSet<OptionalCapability>,
    #[serde(skip)]
    pub(super) deferred: DeferredModuleConfig,
}

impl RuntimeCompositionConfig {
    /// Selects the provider/tool/session core with no optional owners.
    #[must_use]
    pub fn core() -> Self {
        Self::new(RuntimeCompositionProfile::Core, [])
    }

    /// Selects the complete product composition, subject to each module's own enable flag.
    #[must_use]
    pub fn standard() -> Self {
        Self::default()
    }

    /// Builds an explicit selection without any root-config payloads.
    #[must_use]
    pub fn new(
        profile: RuntimeCompositionProfile,
        enhancements: impl IntoIterator<Item = OptionalCapability>,
    ) -> Self {
        Self {
            profile,
            enhancements: enhancements.into_iter().collect(),
            deferred: DeferredModuleConfig::default(),
        }
    }

    /// Tests only composition selection; it does not enable a disabled module or grant authority.
    #[must_use]
    pub fn allows(&self, capability: OptionalCapability) -> bool {
        self.profile == RuntimeCompositionProfile::Standard
            || self.enhancements.contains(&capability)
    }

    /// Lists the selected capability names without interpreting module configuration.
    #[must_use]
    pub fn selected_capabilities(&self) -> BTreeSet<OptionalCapability> {
        OptionalCapability::ALL
            .into_iter()
            .filter(|capability| self.allows(*capability))
            .collect()
    }

    /// Copies the selection without carrying deferred root-config payloads into another owner.
    #[must_use]
    pub fn selection_only(&self) -> Self {
        Self::new(self.profile, self.enhancements.iter().copied())
    }
}

impl fmt::Debug for RuntimeCompositionConfig {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("RuntimeCompositionConfig")
            .field("profile", &self.profile)
            .field("enhancements", &self.enhancements)
            .finish()
    }
}

impl PartialEq for RuntimeCompositionConfig {
    fn eq(&self, other: &Self) -> bool {
        self.profile == other.profile && self.enhancements == other.enhancements
    }
}

impl Eq for RuntimeCompositionConfig {}
