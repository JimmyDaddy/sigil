use std::path::Path;

use anyhow::Result;
use sigil_kernel::{
    AgentProfileKind, AgentProfileSource, AgentTrustState, RootConfig, SessionLogEntry,
    SkillRunMode, SkillTrustState, default_user_config_dir, safe_persistence_text,
};

use crate::{AgentProfileRegistry, discover_skill_index_with_user_dir};

const MAX_CATALOG_ENTRIES_PER_KIND: usize = 80;
const MAX_CATALOG_TEXT_BYTES: usize = 512;

/// Shared metadata for one application slash command.
///
/// The TUI and graphical clients consume this single catalog instead of maintaining competing
/// trigger, alias, and completion tables.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ApplicationCommandSpec {
    pub canonical: &'static str,
    pub aliases: &'static [&'static str],
    pub label: &'static str,
    pub description: &'static str,
    pub argument_hint: Option<&'static str>,
    pub completes_with_space: bool,
    pub client_action: Option<ApplicationClientAction>,
}

/// Client-owned action selected by a shared application command.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ApplicationClientAction {
    PreviewCompaction,
    NewSession,
    FocusEffort,
    FocusModel,
    OpenSessionPicker,
    OpenAgentWorkbench,
    OpenSettings,
    OpenSupport,
}

/// Exact shared slash-command catalog.
pub const APPLICATION_COMMANDS: &[ApplicationCommandSpec] = &[
    ApplicationCommandSpec {
        canonical: "/compact",
        aliases: &[],
        label: "Compact context",
        description: "preview V2 context compaction",
        argument_hint: None,
        completes_with_space: false,
        client_action: Some(ApplicationClientAction::PreviewCompaction),
    },
    ApplicationCommandSpec {
        canonical: "/config",
        aliases: &[],
        label: "Open settings",
        description: "edit config",
        argument_hint: None,
        completes_with_space: false,
        client_action: Some(ApplicationClientAction::OpenSettings),
    },
    ApplicationCommandSpec {
        canonical: "/doctor",
        aliases: &[],
        label: "Run diagnostics",
        description: "run local diagnostics",
        argument_hint: None,
        completes_with_space: false,
        client_action: Some(ApplicationClientAction::OpenSupport),
    },
    ApplicationCommandSpec {
        canonical: "/feedback",
        aliases: &[],
        label: "Send feedback",
        description: "review and export a private support report",
        argument_hint: None,
        completes_with_space: false,
        client_action: Some(ApplicationClientAction::OpenSupport),
    },
    ApplicationCommandSpec {
        canonical: "/effort",
        aliases: &["/e"],
        label: "Change effort",
        description: "set reasoning effort",
        argument_hint: Some("<low|medium|high|max>"),
        completes_with_space: true,
        client_action: Some(ApplicationClientAction::FocusEffort),
    },
    ApplicationCommandSpec {
        canonical: "/model",
        aliases: &["/m"],
        label: "Change model",
        description: "set the model for the next run in this conversation",
        argument_hint: Some("<model>"),
        completes_with_space: true,
        client_action: Some(ApplicationClientAction::FocusModel),
    },
    ApplicationCommandSpec {
        canonical: "/new",
        aliases: &[],
        label: "New conversation",
        description: "start a fresh session",
        argument_hint: None,
        completes_with_space: false,
        client_action: Some(ApplicationClientAction::NewSession),
    },
    ApplicationCommandSpec {
        canonical: "/plan",
        aliases: &[],
        label: "Plan",
        description: "enter plan mode or run one plan prompt",
        argument_hint: Some("[prompt]"),
        completes_with_space: true,
        client_action: Some(ApplicationClientAction::OpenAgentWorkbench),
    },
    ApplicationCommandSpec {
        canonical: "/task",
        aliases: &[],
        label: "Durable task",
        description: "start or continue a durable task",
        argument_hint: Some("<objective>"),
        completes_with_space: true,
        client_action: None,
    },
    ApplicationCommandSpec {
        canonical: "/quit",
        aliases: &["/q", "/exit"],
        label: "Quit",
        description: "quit TUI",
        argument_hint: None,
        completes_with_space: false,
        client_action: None,
    },
    ApplicationCommandSpec {
        canonical: "/queue",
        aliases: &[],
        label: "Queue controls",
        description: "advanced follow-up controls",
        argument_hint: Some("<action>"),
        completes_with_space: true,
        client_action: None,
    },
    ApplicationCommandSpec {
        canonical: "/resume",
        aliases: &[],
        label: "Open conversation",
        description: "choose a saved session",
        argument_hint: Some("[query]"),
        completes_with_space: true,
        client_action: Some(ApplicationClientAction::OpenSessionPicker),
    },
    ApplicationCommandSpec {
        canonical: "/agent",
        aliases: &[],
        label: "Agents",
        description: "open the agent workbench",
        argument_hint: Some("[profile]"),
        completes_with_space: true,
        client_action: Some(ApplicationClientAction::OpenAgentWorkbench),
    },
];

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ApplicationSkillBinding {
    pub skill_id: String,
    pub skill_sha256: String,
    pub index_fingerprint: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ApplicationAgentBinding {
    pub profile_id: String,
    pub snapshot_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ApplicationCommandCatalogEntry {
    pub canonical: String,
    pub aliases: Vec<String>,
    pub label: String,
    pub description: String,
    pub argument_hint: Option<String>,
    pub completes_with_space: bool,
    pub client_action: Option<ApplicationClientAction>,
    pub available: bool,
    pub unavailable_reason: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ApplicationSkillCatalogEntry {
    pub id: String,
    pub invocation_token: String,
    pub name: String,
    pub description: String,
    pub source: String,
    pub run_mode: String,
    pub trust: String,
    pub available: bool,
    pub unavailable_reason: Option<String>,
    pub binding: Option<ApplicationSkillBinding>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ApplicationAgentCatalogEntry {
    pub id: String,
    pub invocation_token: String,
    pub description: String,
    pub source: String,
    pub kind: String,
    pub trust: String,
    pub enabled: bool,
    pub user_invocable: bool,
    pub available: bool,
    pub unavailable_reason: Option<String>,
    pub snapshot_id: Option<String>,
    pub binding: Option<ApplicationAgentBinding>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ApplicationExtensionCatalogView {
    pub commands: Vec<ApplicationCommandCatalogEntry>,
    pub skills: Vec<ApplicationSkillCatalogEntry>,
    pub agents: Vec<ApplicationAgentCatalogEntry>,
}

/// Builds the bounded, path-free extension catalog consumed by application adapters.
///
/// Agent profiles expose an exact snapshot binding only when they are enabled, trusted, and
/// user-invocable. Child-session skills remain unavailable until their distinct execution contract
/// is admitted by the application run owner.
pub fn application_extension_catalog_view(
    root_config: &RootConfig,
    workspace_root: &Path,
    entries: &[SessionLogEntry],
) -> Result<ApplicationExtensionCatalogView> {
    let commands = APPLICATION_COMMANDS
        .iter()
        .map(|spec| ApplicationCommandCatalogEntry {
            canonical: spec.canonical.to_owned(),
            aliases: spec
                .aliases
                .iter()
                .map(|alias| (*alias).to_owned())
                .collect(),
            label: spec.label.to_owned(),
            description: spec.description.to_owned(),
            argument_hint: spec.argument_hint.map(str::to_owned),
            completes_with_space: spec.completes_with_space,
            client_action: spec.client_action,
            available: spec.client_action.is_some(),
            unavailable_reason: spec
                .client_action
                .is_none()
                .then(|| "This command does not yet have a desktop application route.".to_owned()),
        })
        .collect();

    let user_config_dir = default_user_config_dir().ok();
    let skill_report = discover_skill_index_with_user_dir(
        workspace_root,
        user_config_dir.as_deref(),
        &root_config.skills,
    )?;
    let fingerprint = skill_report.snapshot.fingerprint.clone();
    let skills = skill_report
        .snapshot
        .descriptors
        .iter()
        .take(MAX_CATALOG_ENTRIES_PER_KIND)
        .map(|descriptor| {
            let unavailable_reason = skill_unavailable_reason(descriptor);
            let available = unavailable_reason.is_none();
            ApplicationSkillCatalogEntry {
                id: descriptor.id.clone(),
                invocation_token: format!("${}", descriptor.id),
                name: bounded_catalog_text(if descriptor.name.trim().is_empty() {
                    &descriptor.id
                } else {
                    &descriptor.name
                }),
                description: bounded_catalog_text(&descriptor.description),
                source: descriptor.source.as_str().to_owned(),
                run_mode: descriptor.run_as.as_str().to_owned(),
                trust: descriptor.trust.as_str().to_owned(),
                available,
                unavailable_reason,
                binding: available.then(|| ApplicationSkillBinding {
                    skill_id: descriptor.id.clone(),
                    skill_sha256: descriptor.sha256.clone(),
                    index_fingerprint: fingerprint.clone(),
                }),
            }
        })
        .collect();

    let registry = AgentProfileRegistry::from_root_config_with_workspace_and_entries(
        root_config,
        workspace_root,
        entries,
    )?;
    let agents = registry
        .profiles()
        .iter()
        .filter(|profile| profile.profile.kind != AgentProfileKind::System)
        .take(MAX_CATALOG_ENTRIES_PER_KIND)
        .map(|profile| {
            let snapshot = registry.capture_snapshot(profile.id()).ok();
            let enabled = profile.effective_enabled();
            let user_invocable = profile.effective_user_invocation_allowed();
            let unavailable_reason = if !enabled {
                Some("This agent profile is disabled.".to_owned())
            } else if profile.trust_state != AgentTrustState::Trusted {
                Some("Review and trust this agent profile before invoking it.".to_owned())
            } else if !user_invocable {
                Some("This agent profile cannot be invoked directly by a user.".to_owned())
            } else if snapshot.is_none() {
                Some("The exact agent profile snapshot could not be captured.".to_owned())
            } else {
                None
            };
            let snapshot_id = snapshot.map(|snapshot| snapshot.snapshot_id.as_str().to_owned());
            let binding = unavailable_reason
                .is_none()
                .then(|| ApplicationAgentBinding {
                    profile_id: profile.id().as_str().to_owned(),
                    snapshot_id: snapshot_id
                        .clone()
                        .expect("available agent profiles have an exact snapshot"),
                });
            ApplicationAgentCatalogEntry {
                id: profile.id().as_str().to_owned(),
                invocation_token: format!("@{}", profile.id().as_str()),
                description: bounded_catalog_text(&profile.profile.description),
                source: agent_source_label(&profile.source).to_owned(),
                kind: agent_kind_label(profile.profile.kind).to_owned(),
                trust: agent_trust_label(profile.trust_state).to_owned(),
                enabled,
                user_invocable,
                available: unavailable_reason.is_none(),
                unavailable_reason,
                snapshot_id,
                binding,
            }
        })
        .collect();

    Ok(ApplicationExtensionCatalogView {
        commands,
        skills,
        agents,
    })
}

fn skill_unavailable_reason(descriptor: &sigil_kernel::SkillDescriptor) -> Option<String> {
    if !descriptor.enabled || descriptor.trust == SkillTrustState::Disabled {
        return Some("This skill is disabled.".to_owned());
    }
    if descriptor.trust != SkillTrustState::Trusted {
        return Some("Review and trust this skill before invoking it.".to_owned());
    }
    if !descriptor.user_invocable {
        return Some("This skill cannot be invoked directly by a user.".to_owned());
    }
    if descriptor.run_as != SkillRunMode::Inline {
        return Some("Child-session skills require the supervised desktop agent owner.".to_owned());
    }
    None
}

fn bounded_catalog_text(value: &str) -> String {
    let safe = safe_persistence_text(value);
    if safe.len() <= MAX_CATALOG_TEXT_BYTES {
        return safe;
    }
    let mut end = MAX_CATALOG_TEXT_BYTES.saturating_sub('…'.len_utf8());
    while !safe.is_char_boundary(end) {
        end = end.saturating_sub(1);
    }
    format!("{}…", &safe[..end])
}

const fn agent_source_label(source: &AgentProfileSource) -> &'static str {
    match source {
        AgentProfileSource::Workspace => "workspace",
        AgentProfileSource::User => "user",
        AgentProfileSource::Plugin { .. } => "plugin",
        AgentProfileSource::Compatibility { .. } => "compatibility",
        AgentProfileSource::System => "system",
        AgentProfileSource::Unknown => "unknown",
    }
}

const fn agent_kind_label(kind: AgentProfileKind) -> &'static str {
    match kind {
        AgentProfileKind::Primary => "primary",
        AgentProfileKind::Subagent => "subagent",
        AgentProfileKind::System => "system",
        AgentProfileKind::Unknown => "unknown",
    }
}

const fn agent_trust_label(trust: AgentTrustState) -> &'static str {
    match trust {
        AgentTrustState::Trusted => "trusted",
        AgentTrustState::NeedsReview => "needs_review",
        AgentTrustState::Disabled => "disabled",
        AgentTrustState::Unknown => "unknown",
    }
}
