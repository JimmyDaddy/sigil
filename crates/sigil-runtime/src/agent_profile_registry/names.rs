use std::collections::{BTreeMap, BTreeSet};

use anyhow::{Context, Result};
use sigil_kernel::AgentProfileId;

use super::ResolvedAgentProfile;

pub(super) fn normalize_profile_name_list(values: Vec<String>, label: &str) -> Result<Vec<String>> {
    let mut names = BTreeSet::new();
    for value in values {
        let trimmed = value.trim();
        if trimmed.is_empty() {
            continue;
        }
        let name = trimmed
            .strip_prefix('@')
            .or_else(|| trimmed.strip_prefix('/'))
            .unwrap_or(trimmed)
            .trim();
        AgentProfileId::new(name.to_owned())
            .with_context(|| format!("invalid {label} {value:?}"))?;
        names.insert(name.to_owned());
    }
    Ok(names.into_iter().collect())
}

pub(super) fn disable_conflicting_profile_names(
    profiles: &mut [ResolvedAgentProfile],
    warnings: &mut Vec<String>,
) {
    disable_conflicting_profile_name_kind(
        profiles,
        warnings,
        ProfileNameKind::Alias,
        |profile| &profile.profile.aliases,
        |profile| &mut profile.profile.aliases,
    );
    disable_conflicting_profile_name_kind(
        profiles,
        warnings,
        ProfileNameKind::SlashName,
        |profile| &profile.profile.slash_names,
        |profile| &mut profile.profile.slash_names,
    );
}

#[derive(Debug, Clone, Copy)]
enum ProfileNameKind {
    Alias,
    SlashName,
}

impl ProfileNameKind {
    fn label(self) -> &'static str {
        match self {
            Self::Alias => "alias",
            Self::SlashName => "slash name",
        }
    }
}

fn disable_conflicting_profile_name_kind(
    profiles: &mut [ResolvedAgentProfile],
    warnings: &mut Vec<String>,
    kind: ProfileNameKind,
    names: fn(&ResolvedAgentProfile) -> &Vec<String>,
    names_mut: fn(&mut ResolvedAgentProfile) -> &mut Vec<String>,
) {
    let profile_ids = profiles
        .iter()
        .map(|profile| profile.profile.id.as_str().to_owned())
        .collect::<BTreeSet<_>>();
    let mut claims = BTreeMap::<String, BTreeSet<String>>::new();
    let mut blocked = BTreeSet::<String>::new();

    for profile in profiles.iter() {
        let profile_id = profile.profile.id.as_str();
        for name in names(profile) {
            if name == profile_id {
                continue;
            }
            if profile_ids.contains(name) {
                blocked.insert(name.clone());
            }
            claims
                .entry(name.clone())
                .or_default()
                .insert(profile_id.to_owned());
        }
    }

    for (name, owners) in &claims {
        if owners.len() > 1 {
            blocked.insert(name.clone());
        }
    }

    for name in &blocked {
        let mut owners = claims
            .get(name)
            .map(|owners| owners.iter().cloned().collect::<Vec<_>>())
            .unwrap_or_default();
        if profile_ids.contains(name) && !owners.iter().any(|owner| owner == name) {
            owners.push(name.clone());
            owners.sort();
        }
        warnings.push(format!(
            "agent profile {} {:?} is ambiguous across {}; {} disabled",
            kind.label(),
            name,
            owners.join(","),
            kind.label()
        ));
    }

    for profile in profiles.iter_mut() {
        let profile_id = profile.profile.id.as_str().to_owned();
        names_mut(profile).retain(|name| name == &profile_id || !blocked.contains(name));
    }
}
