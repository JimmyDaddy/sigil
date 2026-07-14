use super::*;

pub(super) fn check_provider(report: &mut DoctorReport, root_config: &RootConfig) {
    match provider_config_key(&root_config.agent.provider) {
        "deepseek" => check_deepseek_provider(report, root_config),
        "openai_compat" => check_openai_compat_provider(report, root_config),
        "openai_responses" => check_openai_responses_provider(report, root_config),
        "anthropic" => check_anthropic_provider(report, root_config),
        "gemini" => check_gemini_provider(report, root_config),
        other => report.push_with_remediation(
            DoctorStatus::Error,
            "provider",
            format!("unsupported provider {other}"),
            Some("set [agent].provider to \"deepseek\", \"openai_compat\", \"openai_responses\", \"anthropic\", or \"gemini\""),
        ),
    }
}

pub(super) fn check_execution_backend(report: &mut DoctorReport, root_config: &RootConfig) {
    let config = &root_config.execution;
    match sigil_tools_builtin::build_execution_backend(config) {
        Ok(backend) => {
            let capabilities = backend.capabilities();
            let capability_summary = execution_capability_summary(capabilities);
            let image = config
                .container_image()
                .map(|image| format!(", image={image}"))
                .unwrap_or_default();
            let message = format!(
                "backend={}, profile={:?}, fallback={}, capabilities={}{}",
                backend.kind().as_str(),
                config.profile(),
                config.fallback().as_str(),
                capability_summary,
                image
            );
            let local_relaxed = backend.kind() == sigil_kernel::ExecutionBackendKind::Local
                && config.requires_sandbox();
            let status = if local_relaxed {
                DoctorStatus::Warn
            } else {
                DoctorStatus::Ok
            };
            report.push_with_remediation(
                status,
                "execution:sandbox",
                message,
                local_relaxed
                    .then_some("fallback relaxed sandbox enforcement to local execution; choose a sandbox backend to enforce isolation"),
            );
        }
        Err(error) => {
            report.push_with_remediation(
                DoctorStatus::Error,
                "execution:sandbox",
                error.to_string(),
                Some("check [execution].strategy, [execution.sandbox], and installed backend dependencies"),
            );
        }
    }
}

fn execution_capability_summary(
    capabilities: sigil_kernel::ExecutionBackendCapabilities,
) -> String {
    let mut labels = Vec::new();
    if capabilities.filesystem_isolation {
        labels.push("filesystem");
    }
    if capabilities.network_isolation {
        labels.push("network");
    }
    if capabilities.process_isolation {
        labels.push("process");
    }
    if capabilities.resource_limits {
        labels.push("resource");
    }
    if capabilities.persistent_pty {
        labels.push("persistent_pty");
    }
    if capabilities.workspace_snapshot {
        labels.push("workspace_snapshot");
    }
    if labels.is_empty() {
        "none".to_owned()
    } else {
        labels.join(",")
    }
}

fn check_deepseek_provider(report: &mut DoctorReport, root_config: &RootConfig) {
    let config = match load_deepseek_config(root_config).and_then(|config| config.resolved()) {
        Ok(config) => config,
        Err(error) => {
            report.push_with_remediation(
                DoctorStatus::Error,
                "provider:deepseek",
                error.to_string(),
                Some("add a valid [providers.deepseek] block, or rerun Quick Setup"),
            );
            return;
        }
    };
    report.push(
        DoctorStatus::Ok,
        "provider:deepseek",
        format!("model={} base_url={}", config.model, config.base_url),
    );

    push_provider_auth_check(
        report,
        resolve_deepseek_api_key(&config),
        SIGIL_API_KEY_ENV,
        "[providers.deepseek].api_key",
    );
    push_provider_capability_checks(report, "deepseek");
}

fn check_openai_compat_provider(report: &mut DoctorReport, root_config: &RootConfig) {
    let config = match load_openai_compat_config(root_config).and_then(|config| config.resolved()) {
        Ok(config) => config,
        Err(error) => {
            report.push_with_remediation(
                DoctorStatus::Error,
                "provider:openai_compat",
                error.to_string(),
                Some("add a valid [providers.openai_compat] block"),
            );
            return;
        }
    };
    report.push(
        DoctorStatus::Ok,
        "provider:openai_compat",
        format!("model={} base_url={}", config.model, config.base_url),
    );

    push_provider_auth_check(
        report,
        resolve_openai_compat_api_key(&config),
        OPENAI_COMPATIBLE_API_KEY_ENV,
        "[providers.openai_compat].api_key",
    );
    push_provider_capability_checks(report, "openai_compat");
}

fn check_openai_responses_provider(report: &mut DoctorReport, root_config: &RootConfig) {
    let config =
        match load_openai_responses_config(root_config).and_then(|config| config.resolved()) {
            Ok(config) => config,
            Err(error) => {
                report.push_with_remediation(
                    DoctorStatus::Error,
                    "provider:openai_responses",
                    error.to_string(),
                    Some("add a valid [providers.openai_responses] block"),
                );
                return;
            }
        };
    report.push(
        DoctorStatus::Ok,
        "provider:openai_responses",
        format!("model={} base_url={}", config.model, config.base_url),
    );

    push_provider_auth_check(
        report,
        resolve_openai_responses_api_key(&config),
        OPENAI_RESPONSES_API_KEY_ENV,
        "[providers.openai_responses].api_key",
    );
    push_provider_capability_checks(report, "openai_responses");
}

fn check_anthropic_provider(report: &mut DoctorReport, root_config: &RootConfig) {
    let config = match load_anthropic_config(root_config).and_then(|config| config.resolved()) {
        Ok(config) => config,
        Err(error) => {
            report.push_with_remediation(
                DoctorStatus::Error,
                "provider:anthropic",
                error.to_string(),
                Some("add a valid [providers.anthropic] block"),
            );
            return;
        }
    };
    report.push(
        DoctorStatus::Ok,
        "provider:anthropic",
        format!(
            "model={} base_url={} version={} max_tokens={}",
            config.model, config.base_url, config.anthropic_version, config.max_tokens
        ),
    );

    push_provider_auth_check(
        report,
        resolve_anthropic_api_key(&config),
        SIGIL_ANTHROPIC_API_KEY_ENV,
        "[providers.anthropic].api_key",
    );
    push_provider_capability_checks(report, "anthropic");
}

fn check_gemini_provider(report: &mut DoctorReport, root_config: &RootConfig) {
    let config = match load_gemini_config(root_config).and_then(|config| config.resolved()) {
        Ok(config) => config,
        Err(error) => {
            report.push_with_remediation(
                DoctorStatus::Error,
                "provider:gemini",
                error.to_string(),
                Some("add a valid [providers.gemini] block"),
            );
            return;
        }
    };
    report.push(
        DoctorStatus::Ok,
        "provider:gemini",
        format!("model={} base_url={}", config.model, config.base_url),
    );

    push_provider_auth_check(
        report,
        resolve_gemini_api_key(&config),
        SIGIL_GEMINI_API_KEY_ENV,
        "[providers.gemini].api_key",
    );
    push_provider_capability_checks(report, "gemini");
}

fn push_provider_capability_checks(report: &mut DoctorReport, provider_name: &str) {
    let Some(capabilities) = provider_capabilities_for_name(provider_name) else {
        return;
    };
    let view = provider_capability_view(provider_name, &capabilities);
    let supported = view
        .rows
        .iter()
        .filter(|row| row.status.as_str() == "supported")
        .count();
    let advanced = view
        .rows
        .iter()
        .filter(|row| row.status.as_str() == "advanced")
        .count();
    report.push(
        DoctorStatus::Ok,
        format!("provider:{provider_name}:capabilities"),
        format!(
            "{} supported, {} advanced, {} total",
            supported,
            advanced,
            view.rows.len()
        ),
    );
    for row in view.rows {
        report.push(
            DoctorStatus::Ok,
            format!("provider:{provider_name}:capability:{}", row.key),
            format!("{}: {} ({})", row.label, row.status.as_str(), row.detail),
        );
    }
}

pub(super) fn push_provider_auth_check(
    report: &mut DoctorReport,
    secret: Option<SecretResolution>,
    preferred_env: &'static str,
    config_key: &'static str,
) {
    match secret {
        Some(secret) if secret.source == SecretSource::ConfigPlaintext => report.push_with_remediation(
            DoctorStatus::Warn,
            "provider:auth",
            "resolved from config plaintext",
            Some(format!(
                "prefer {preferred_env} for temporary use; if api_key stays in sigil.toml, keep the file private and never commit it",
            )),
        ),
        Some(secret) => report.push(
            DoctorStatus::Ok,
            "provider:auth",
            format!("resolved from {}", secret_source_label(secret.source)),
        ),
        None => report.push_with_remediation(
            DoctorStatus::Error,
            "provider:auth",
            format!(
                "missing api key; set {preferred_env} or {config_key}",
            ),
            Some(format!(
                "for temporary use, export {preferred_env}; if you save api_key in sigil.toml, it is plaintext",
            )),
        ),
    }
}

pub(super) fn secret_source_label(source: SecretSource) -> &'static str {
    match source {
        SecretSource::Environment(name) => name,
        SecretSource::ConfigPlaintext => "config plaintext",
        SecretSource::Session => "session",
    }
}
