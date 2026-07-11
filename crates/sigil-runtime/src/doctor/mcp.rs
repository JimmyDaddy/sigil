use super::*;

pub(super) fn check_mcp_servers(
    report: &mut DoctorReport,
    root_config: &RootConfig,
    workspace_root: &Path,
) {
    let servers = &root_config.mcp_servers;
    if servers.is_empty() {
        report.push(DoctorStatus::Ok, "mcp", "no servers configured");
        return;
    }

    for server in servers {
        let declaration =
            crate::ResolvedMcpServerDeclaration::user_root(server.clone(), workspace_root);
        let projection = declaration
            .as_ref()
            .ok()
            .map(crate::ResolvedMcpServerDeclaration::safe_projection);
        let command_status =
            match declaration
                .as_ref()
                .map_err(ToString::to_string)
                .and_then(|declaration| {
                    declaration
                        .resolve_stdio_launch(&[])
                        .map_err(|error| error.to_string())
                }) {
                Ok(_) => CommandStatus::Available,
                Err(_) if server.command.trim().is_empty() => CommandStatus::Empty,
                Err(_) => CommandStatus::Missing,
            };
        let environment = sigil_kernel::resolve_extension_process_environment(&server.inherit_env);
        let status = if environment.is_err() {
            DoctorStatus::Error
        } else {
            match command_status {
                CommandStatus::Available => DoctorStatus::Ok,
                CommandStatus::Empty => DoctorStatus::Error,
                CommandStatus::Missing
                    if server.required && server.startup == McpServerStartup::Eager =>
                {
                    DoctorStatus::Error
                }
                CommandStatus::Missing => DoctorStatus::Warn,
            }
        };
        let environment_summary = match &environment {
            Ok(environment) => format!(
                "isolated grants={} missing=none live={}",
                environment_grant_names_summary(environment.grant_names()),
                short_environment_fingerprint(environment.live_fingerprint())
            ),
            Err(error) => format!(
                "isolated grants={} missing error={}",
                environment_grant_names_summary(&server.inherit_env),
                error.code.as_str()
            ),
        };
        let remediation = environment.as_ref().err().map(|error| {
            format!(
                "{}; set every inherit_env variable before starting Sigil, or remove the unused grant",
                error.message
            )
        }).or_else(|| mcp_remediation(server, command_status).map(ToOwned::to_owned));
        report.push_with_remediation(
            status,
            format!("mcp:{}", server.name),
            format!(
                "{} required={} command={} declaration=(declared={} effective={} origin={} base={} fingerprint={}) facets=(local=execute declared_network=unknown effective_network=runtime_preflight source_trust={} source_approval={}) secrets={} pin={} environment=({}) network_admission=run_scoped boundary={}",
                server.startup.as_str(),
                server.required,
                command_status.as_str(),
                projection
                    .as_ref()
                    .map(|projection| projection.declared_name.as_str())
                    .unwrap_or("invalid"),
                projection
                    .as_ref()
                    .map(|projection| projection.effective_name.as_str())
                    .unwrap_or("invalid"),
                projection
                    .as_ref()
                    .map(|projection| projection.origin_kind.as_str())
                    .unwrap_or("invalid"),
                projection
                    .as_ref()
                    .map(|projection| projection.execution_base_kind.as_str())
                    .unwrap_or("invalid"),
                projection
                    .as_ref()
                    .map(|projection| projection.declaration_fingerprint.as_str())
                    .unwrap_or("invalid"),
                server.trust.trust_class.as_str(),
                server.trust.approval_default.as_str(),
                if server.trust.allow_secrets {
                    "allowed"
                } else {
                    "blocked"
                },
                if server.trust.pin_version {
                    "required"
                } else {
                    "off"
                },
                environment_summary,
                crate::mcp_stdio_boundary_summary(root_config, workspace_root, server),
            ),
            remediation,
        );
    }
}

fn environment_grant_names_summary(names: &[String]) -> String {
    if names.is_empty() {
        "none".to_owned()
    } else {
        names.join(",")
    }
}

fn short_environment_fingerprint(fingerprint: &str) -> String {
    const MAX_CHARS: usize = 24;
    fingerprint.chars().take(MAX_CHARS).collect()
}

#[derive(Debug, Default)]
struct PluginHookDoctorSummary {
    hooks: usize,
    trusted: usize,
    needs_review: usize,
    disabled: usize,
    context: usize,
    compaction: usize,
    verification: usize,
    event: usize,
    read_only: usize,
    workspace_write: usize,
    external_write: usize,
    network: usize,
    unknown: usize,
}

impl PluginHookDoctorSummary {
    fn record(&mut self, trust: PluginTrustDecision, capability: &PluginCapability) {
        let PluginCapability::Hook {
            hook_kind,
            declared_effect,
            ..
        } = capability
        else {
            return;
        };
        self.hooks += 1;
        match trust {
            PluginTrustDecision::Trusted => self.trusted += 1,
            PluginTrustDecision::NeedsReview => self.needs_review += 1,
            PluginTrustDecision::Disabled => self.disabled += 1,
        }
        match hook_kind {
            PluginHookKind::Context => self.context += 1,
            PluginHookKind::Compaction => self.compaction += 1,
            PluginHookKind::Verification => self.verification += 1,
            PluginHookKind::Event => self.event += 1,
        }
        match declared_effect {
            ToolEffect::ReadOnly => self.read_only += 1,
            ToolEffect::WorkspaceWrite => self.workspace_write += 1,
            ToolEffect::ExternalWrite => self.external_write += 1,
            ToolEffect::Network => self.network += 1,
            ToolEffect::Unknown => self.unknown += 1,
        }
    }

    fn risky_effects(&self) -> usize {
        self.workspace_write + self.external_write + self.network + self.unknown
    }

    fn message(&self) -> String {
        format!(
            "hooks={} trusted={} review={} disabled={} process_facets=local_execute:{} declared_network_unknown:{} effective_network_preflight:{} source_trust=manifest kinds=context:{} compaction:{} verification:{} event:{} declared_effects=read_only:{} workspace_write:{} external_write:{} network:{} unknown:{} network_admission=run_scoped",
            self.hooks,
            self.trusted,
            self.needs_review,
            self.disabled,
            self.hooks,
            self.hooks,
            self.hooks,
            self.context,
            self.compaction,
            self.verification,
            self.event,
            self.read_only,
            self.workspace_write,
            self.external_write,
            self.network,
            self.unknown
        )
    }
}

pub(super) fn check_plugin_hooks(
    report: &mut DoctorReport,
    workspace_root: &Path,
    plugin_trust_entries: &[PluginTrustEntry],
) {
    let discovery = match crate::discover_workspace_plugins(workspace_root, plugin_trust_entries) {
        Ok(discovery) => discovery,
        Err(error) => {
            report.push_with_remediation(
                DoctorStatus::Warn,
                "plugins:hooks",
                format!("plugin discovery failed: {error}"),
                Some("check .sigil/plugins manifests before trusting or running plugin hooks"),
            );
            return;
        }
    };
    if let Some(warning) = discovery.warnings.first() {
        report.push_with_remediation(
            DoctorStatus::Warn,
            "plugins:discovery",
            format!(
                "{} warnings; first={} {}",
                discovery.warnings.len(),
                warning.kind.code(),
                warning.path.display()
            ),
            Some("open /config Plugins to review manifest warnings before trusting plugins"),
        );
    }

    if !discovery.registrations.mcp_servers.is_empty() {
        match crate::merge_mcp_server_declarations(&[], &discovery.registrations.mcp_servers) {
            Ok(declarations) => {
                let declaration_rows = declarations
                    .iter()
                    .map(|declaration| {
                        let activation_status =
                            if declaration.verify_activation(plugin_trust_entries).is_ok() {
                                "attested"
                            } else {
                                "stale"
                            };
                        (declaration.safe_projection(), activation_status)
                    })
                    .collect::<Vec<_>>();
                let stale = declaration_rows
                    .iter()
                    .filter(|(_, activation_status)| *activation_status == "stale")
                    .count();
                let projections = declaration_rows
                    .into_iter()
                    .map(|(projection, activation_status)| {
                        format!(
                            "{}/{}:{}/{}:{}",
                            projection.declared_name,
                            projection.effective_name,
                            projection.origin_kind.as_str(),
                            projection.execution_base_kind.as_str(),
                            activation_status
                        )
                    })
                    .collect::<Vec<_>>()
                    .join(",");
                report.push_with_remediation(
                    if stale == 0 {
                        DoctorStatus::Ok
                    } else {
                        DoctorStatus::Warn
                    },
                    "plugins:mcp",
                    format!(
                        "declarations={} stale={} safe=[{}] paths=hidden",
                        declarations.len(),
                        stale,
                        projections
                    ),
                    (stale > 0).then_some(
                        "review the changed plugin manifest before activating its MCP server",
                    ),
                );
            }
            Err(error) => report.push_with_remediation(
                DoctorStatus::Warn,
                "plugins:mcp",
                format!("{}: {}", error.code(), error.reason),
                Some("review plugin MCP declarations before activation"),
            ),
        }
    }

    let mut summary = PluginHookDoctorSummary::default();
    for manifest in &discovery.manifests {
        for capability in &manifest.capabilities {
            summary.record(manifest.trust, capability);
        }
    }
    if summary.hooks == 0 {
        report.push(
            DoctorStatus::Ok,
            "plugins:hooks",
            "no hook commands discovered",
        );
        return;
    }

    let status = if summary.needs_review > 0 || summary.risky_effects() > 0 {
        DoctorStatus::Warn
    } else {
        DoctorStatus::Ok
    };
    let remediation = if summary.needs_review > 0 {
        Some("review plugin manifests in /config before hook commands can run")
    } else if summary.risky_effects() > 0 {
        Some(
            "source trust, declared effects and secret access do not authorize network; hooks require run-scoped network admission, backend isolation and mutation evidence",
        )
    } else {
        None
    };
    report.push_with_remediation(status, "plugins:hooks", summary.message(), remediation);
}

fn mcp_remediation(
    server: &McpServerConfig,
    command_status: CommandStatus,
) -> Option<&'static str> {
    match command_status {
        CommandStatus::Empty => {
            Some("set command to the stdio server executable, or remove this MCP server")
        }
        CommandStatus::Missing if server.required && server.startup == McpServerStartup::Eager => {
            Some(
                "install the command, use a valid absolute or workspace-relative path, switch startup to lazy, or set required = false",
            )
        }
        CommandStatus::Missing => Some(
            "install the command, use a valid path, or remove this MCP server until it is available",
        ),
        CommandStatus::Available => None,
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum CommandStatus {
    Available,
    Missing,
    Empty,
}

impl CommandStatus {
    pub(super) fn as_str(self) -> &'static str {
        match self {
            Self::Available => "available",
            Self::Missing => "missing",
            Self::Empty => "empty",
        }
    }
}

pub(super) fn command_status(command: &str, base_dir: &Path) -> CommandStatus {
    let trimmed = command.trim();
    if trimmed.is_empty() {
        return CommandStatus::Empty;
    }
    let command_path = Path::new(trimmed);
    if command_path.is_absolute() || command_path.components().count() > 1 {
        let candidate = if command_path.is_absolute() {
            command_path.to_path_buf()
        } else {
            base_dir.join(command_path)
        };
        return if candidate.exists() {
            CommandStatus::Available
        } else {
            CommandStatus::Missing
        };
    }
    let Some(paths) = env::var_os("PATH") else {
        return CommandStatus::Missing;
    };
    if env::split_paths(&paths).any(|path| path.join(trimmed).exists()) {
        CommandStatus::Available
    } else {
        CommandStatus::Missing
    }
}
