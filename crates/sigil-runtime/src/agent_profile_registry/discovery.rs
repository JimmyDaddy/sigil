use std::{collections::BTreeMap, fs, path::Path};

use anyhow::Result;
use sigil_kernel::{
    AgentProfileId, PluginStateProjection, RootConfig, SessionLogEntry, SkillRunMode,
};

use crate::{
    plugins::discover_workspace_plugins, resolve_sigil_paths,
    skills::discover_skill_index_with_user_dir,
};

use super::{
    ResolvedAgentProfile, agent_profile_source_label, child_session_skill_profile, display_path,
    fallback_plugin_agent_id, native_agent_entrypoint, path_stays_in_workspace,
    plugin_agent_profile_format, plugin_agent_profile_from_raw, sorted_dir_entries,
    tool_scope_is_empty, workspace_agent_profile_from_raw, workspace_path,
};

pub(super) fn discover_workspace_agent_profiles(
    root_config: &RootConfig,
    workspace_root: &Path,
    profiles: &mut Vec<ResolvedAgentProfile>,
    warnings: &mut Vec<String>,
) -> Result<()> {
    if !root_config.skills.enabled {
        return Ok(());
    }
    let paths = resolve_sigil_paths(&root_config.storage, &root_config.session, workspace_root);
    let agents_dir = paths.workspace_agents_dir;
    if !agents_dir.exists() {
        return Ok(());
    }
    if !agents_dir.is_dir() {
        warnings.push(format!(
            "workspace agent discovery path is not a directory: {}",
            agents_dir.display()
        ));
        return Ok(());
    }
    let canonical_workspace = workspace_root
        .canonicalize()
        .unwrap_or_else(|_| workspace_root.to_path_buf());
    if !path_stays_in_workspace(&canonical_workspace, &agents_dir) {
        warnings.push(format!(
            "workspace agent discovery path escapes workspace root: {}",
            agents_dir.display()
        ));
        return Ok(());
    }

    let mut claimed_ids = profiles
        .iter()
        .map(|profile| {
            (
                profile.profile.id.as_str().to_owned(),
                agent_profile_source_label(&profile.source).to_owned(),
            )
        })
        .collect::<BTreeMap<_, _>>();
    for entry in sorted_dir_entries(&agents_dir, warnings) {
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        let fallback_id = path
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_default();
        if let Err(error) = AgentProfileId::new(fallback_id.clone()) {
            warnings.push(format!(
                "invalid workspace agent directory name {}: {error}",
                path.display()
            ));
            continue;
        }
        if !path_stays_in_workspace(&canonical_workspace, &path) {
            warnings.push(format!(
                "workspace agent profile path escapes workspace root: {}",
                path.display()
            ));
            continue;
        }
        let Some((entrypoint, format)) = native_agent_entrypoint(&path) else {
            continue;
        };
        if !path_stays_in_workspace(&canonical_workspace, &entrypoint) {
            warnings.push(format!(
                "workspace agent profile entrypoint escapes workspace root: {}",
                entrypoint.display()
            ));
            continue;
        }
        let raw = match fs::read_to_string(&entrypoint) {
            Ok(raw) => raw,
            Err(error) => {
                warnings.push(format!(
                    "failed to read workspace agent profile {}: {error}",
                    entrypoint.display()
                ));
                continue;
            }
        };
        let resolved = match workspace_agent_profile_from_raw(
            root_config,
            workspace_root,
            &path,
            &entrypoint,
            &fallback_id,
            &raw,
            format,
        ) {
            Ok(profile) => profile,
            Err(error) => {
                warnings.push(format!(
                    "invalid workspace agent profile {}: {error}",
                    entrypoint.display()
                ));
                continue;
            }
        };
        let id = resolved.profile.id.as_str().to_owned();
        if let Some(existing) = claimed_ids.get(&id) {
            warnings.push(format!(
                "workspace agent profile id {id:?} from {} is shadowed by {existing}",
                entrypoint.display()
            ));
            continue;
        }
        claimed_ids.insert(
            id,
            display_path(workspace_root, &entrypoint)
                .display()
                .to_string(),
        );
        profiles.push(resolved);
    }
    Ok(())
}

pub(super) fn discover_plugin_agent_profiles(
    root_config: &RootConfig,
    workspace_root: &Path,
    entries: &[SessionLogEntry],
    profiles: &mut Vec<ResolvedAgentProfile>,
    warnings: &mut Vec<String>,
) -> Result<()> {
    let plugin_projection = PluginStateProjection::from_entries(entries);
    let trust_entries = plugin_projection
        .trust_entries
        .into_values()
        .collect::<Vec<_>>();
    let report = discover_workspace_plugins(workspace_root, &trust_entries)?;
    warnings.extend(report.warnings.into_iter().map(|warning| {
        format!(
            "plugin discovery warning while projecting agent profiles: {}: {}",
            warning.path.display(),
            warning.message
        )
    }));
    let mut claimed_ids = profiles
        .iter()
        .map(|profile| {
            (
                profile.profile.id.as_str().to_owned(),
                agent_profile_source_label(&profile.source).to_owned(),
            )
        })
        .collect::<BTreeMap<_, _>>();

    for registration in report.registrations.agents {
        let entrypoint = registration.plugin_root.join(&registration.agent.path);
        let format = match plugin_agent_profile_format(&entrypoint) {
            Ok(format) => format,
            Err(error) => {
                warnings.push(format!(
                    "invalid plugin agent profile {} from plugin {}: {error}",
                    registration.agent.path.display(),
                    registration.plugin_id
                ));
                continue;
            }
        };
        let fallback_id = match fallback_plugin_agent_id(&registration.agent.path) {
            Ok(id) => id,
            Err(error) => {
                warnings.push(format!(
                    "invalid plugin agent profile {} from plugin {}: {error}",
                    registration.agent.path.display(),
                    registration.plugin_id
                ));
                continue;
            }
        };
        let raw = match fs::read_to_string(&entrypoint) {
            Ok(raw) => raw,
            Err(error) => {
                warnings.push(format!(
                    "failed to read plugin agent profile {} from plugin {}: {error}",
                    registration.agent.path.display(),
                    registration.plugin_id
                ));
                continue;
            }
        };
        let resolved = match plugin_agent_profile_from_raw(
            root_config,
            workspace_root,
            &registration.plugin_id,
            &registration.plugin_root,
            &entrypoint,
            &fallback_id,
            &raw,
            format,
        ) {
            Ok(profile) => profile,
            Err(error) => {
                warnings.push(format!(
                    "invalid plugin agent profile {} from plugin {}: {error}",
                    registration.agent.path.display(),
                    registration.plugin_id
                ));
                continue;
            }
        };
        let id = resolved.profile.id.as_str().to_owned();
        if let Some(existing) = claimed_ids.get(&id) {
            warnings.push(format!(
                "plugin agent profile id {id:?} from {} is shadowed by {existing}",
                display_path(workspace_root, &entrypoint).display()
            ));
            continue;
        }
        claimed_ids.insert(
            id,
            display_path(workspace_root, &entrypoint)
                .display()
                .to_string(),
        );
        profiles.push(resolved);
    }
    Ok(())
}

pub(super) fn discover_child_session_skill_profiles(
    root_config: &RootConfig,
    workspace_root: &Path,
    profiles: &mut Vec<ResolvedAgentProfile>,
    warnings: &mut Vec<String>,
) -> Result<()> {
    let report = discover_skill_index_with_user_dir(workspace_root, None, &root_config.skills)?;
    warnings.extend(report.warnings.into_iter().map(|warning| {
        format!(
            "skill discovery warning while projecting agent profiles: {}: {}",
            warning.path.display(),
            warning.message
        )
    }));
    let mut claimed_ids = profiles
        .iter()
        .map(|profile| {
            (
                profile.profile.id.as_str().to_owned(),
                agent_profile_source_label(&profile.source).to_owned(),
            )
        })
        .collect::<BTreeMap<_, _>>();
    for descriptor in report
        .snapshot
        .descriptors
        .iter()
        .filter(|descriptor| descriptor.run_as == SkillRunMode::ChildSession)
    {
        if !tool_scope_is_empty(&descriptor.disallowed_tools) {
            warnings.push(format!(
                "child-session skill {:?} cannot be projected as an agent profile because disallowed_tools cannot be represented safely",
                descriptor.id
            ));
            continue;
        }
        let resolved = match child_session_skill_profile(root_config, workspace_root, descriptor) {
            Ok(profile) => profile,
            Err(error) => {
                warnings.push(format!(
                    "invalid child-session skill agent profile {:?}: {error:#}",
                    descriptor.id
                ));
                continue;
            }
        };
        let id = resolved.profile.id.as_str().to_owned();
        if let Some(existing) = claimed_ids.get(&id) {
            warnings.push(format!(
                "child-session skill agent profile id {id:?} from {} is shadowed by {existing}",
                descriptor.entrypoint.display()
            ));
            continue;
        }
        claimed_ids.insert(
            id,
            display_path(
                workspace_root,
                &workspace_path(workspace_root, &descriptor.entrypoint),
            )
            .display()
            .to_string(),
        );
        profiles.push(resolved);
    }
    Ok(())
}
