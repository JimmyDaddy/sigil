use super::*;

pub(super) fn check_terminal(report: &mut DoctorReport, config: Option<&TerminalConfig>) {
    let environment = TerminalEnvironment::from_env();
    check_terminal_with_env(report, config, &environment);
}

pub(super) fn check_terminal_with_env(
    report: &mut DoctorReport,
    config: Option<&TerminalConfig>,
    environment: &TerminalEnvironment,
) {
    match environment
        .term
        .as_deref()
        .filter(|value| !value.trim().is_empty())
    {
        Some("dumb") => report.push_with_remediation(
            DoctorStatus::Warn,
            "terminal",
            "TERM=dumb; TUI rendering may be limited",
            Some("launch Sigil from a terminal that sets TERM, such as xterm-256color"),
        ),
        Some(term) => report.push(DoctorStatus::Ok, "terminal", format!("TERM={term}")),
        None => report.push_with_remediation(
            DoctorStatus::Warn,
            "terminal",
            "TERM is not set",
            Some("set TERM in the shell before launching the TUI"),
        ),
    }

    report.push(
        DoctorStatus::Ok,
        "terminal:profile",
        environment.profile_summary(),
    );

    if let Some(config) = config {
        report.push(
            DoctorStatus::Ok,
            "terminal:config",
            format!(
                "mouse_capture={} osc52_clipboard={} scroll_sensitivity={}",
                config.mouse_capture, config.osc52_clipboard, config.scroll_sensitivity
            ),
        );
        check_terminal_mouse(report, config, environment);
        check_terminal_clipboard(report, config, environment);
        report.push(
            DoctorStatus::Ok,
            "terminal:smoke",
            "run checklist: click, scroll, drag transcript, Ctrl-C copy; see docs/en/terminal-compatibility.md",
        );
    }
}

fn check_terminal_mouse(
    report: &mut DoctorReport,
    config: &TerminalConfig,
    environment: &TerminalEnvironment,
) {
    if !config.mouse_capture {
        report.push(
            DoctorStatus::Ok,
            "terminal:mouse",
            "mouse capture disabled by config; keyboard controls remain available",
        );
        return;
    }

    if environment.term_is_missing_or_dumb() {
        report.push_with_remediation(
            DoctorStatus::Warn,
            "terminal:mouse",
            "mouse capture enabled but TERM is missing or dumb",
            Some("fix TERM, or set [terminal].mouse_capture = false if this terminal cannot pass mouse events"),
        );
        return;
    }

    if environment.iterm_mouse_reporting == Some(false) {
        let profile = environment
            .iterm_profile
            .as_deref()
            .unwrap_or("current profile");
        report.push_with_remediation(
            DoctorStatus::Warn,
            "terminal:mouse",
            format!("mouse capture enabled but iTerm profile {profile} disables Mouse Reporting"),
            Some(
                "enable iTerm Settings > Profiles > Terminal > Mouse Reporting for this profile, or set [terminal].mouse_capture = false",
            ),
        );
        return;
    }

    if environment.has_multiplexer() {
        report.push_with_remediation(
            DoctorStatus::Warn,
            "terminal:mouse",
            format!(
                "mouse capture enabled through {}; verify multiplexer mouse pass-through",
                environment.multiplexer_label()
            ),
            Some(
                "enable mouse support in the multiplexer, or set [terminal].mouse_capture = false",
            ),
        );
        return;
    }

    report.push(
        DoctorStatus::Ok,
        "terminal:mouse",
        "mouse capture enabled; smoke: click controls, scroll transcript, drag-select text",
    );
}

fn check_terminal_clipboard(
    report: &mut DoctorReport,
    config: &TerminalConfig,
    environment: &TerminalEnvironment,
) {
    if !config.osc52_clipboard {
        report.push(
            DoctorStatus::Ok,
            "terminal:clipboard",
            "OSC52 clipboard disabled by config",
        );
        return;
    }

    if environment.term_is_missing_or_dumb() {
        report.push_with_remediation(
            DoctorStatus::Warn,
            "terminal:clipboard",
            "OSC52 clipboard enabled but TERM is missing or dumb",
            Some(
                "fix TERM, or set [terminal].osc52_clipboard = false if copy sequences are blocked",
            ),
        );
        return;
    }

    if environment.has_clipboard_bridge_risk() {
        report.push_with_remediation(
            DoctorStatus::Warn,
            "terminal:clipboard",
            format!(
                "OSC52 clipboard enabled through {}; verify clipboard pass-through",
                environment.clipboard_bridge_label()
            ),
            Some("smoke test Ctrl-C copy and paste; if blocked, set [terminal].osc52_clipboard = false"),
        );
        return;
    }

    report.push(
        DoctorStatus::Ok,
        "terminal:clipboard",
        "OSC52 clipboard enabled; smoke: drag-select transcript, press Ctrl-C, paste elsewhere",
    );
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(super) struct TerminalEnvironment {
    pub(super) term: Option<String>,
    pub(super) term_program: Option<String>,
    pub(super) term_program_version: Option<String>,
    pub(super) iterm_profile: Option<String>,
    pub(super) iterm_mouse_reporting: Option<bool>,
    pub(super) tmux: bool,
    pub(super) screen: bool,
    pub(super) ssh: bool,
    pub(super) wsl: bool,
    pub(super) wezterm: bool,
    pub(super) kitty: bool,
    pub(super) windows_terminal: bool,
}

impl TerminalEnvironment {
    pub(super) fn from_env() -> Self {
        let term = non_empty_env("TERM");
        let term_program = non_empty_env("TERM_PROGRAM");
        let term_program_version = non_empty_env("TERM_PROGRAM_VERSION");
        let iterm_profile = non_empty_env("ITERM_PROFILE");
        let iterm_mouse_reporting = if term_program.as_deref() == Some("iTerm.app") {
            iterm_profile
                .as_deref()
                .and_then(iterm_mouse_reporting_for_profile)
        } else {
            None
        };
        Self {
            wezterm: non_empty_env("WEZTERM_EXECUTABLE").is_some()
                || term_program.as_deref() == Some("WezTerm"),
            kitty: non_empty_env("KITTY_WINDOW_ID").is_some()
                || term.as_deref().is_some_and(|term| term.contains("kitty")),
            windows_terminal: non_empty_env("WT_SESSION").is_some(),
            tmux: non_empty_env("TMUX").is_some(),
            screen: non_empty_env("STY").is_some()
                || term
                    .as_deref()
                    .is_some_and(|term| term.starts_with("screen")),
            ssh: non_empty_env("SSH_TTY").is_some() || non_empty_env("SSH_CONNECTION").is_some(),
            wsl: non_empty_env("WSL_DISTRO_NAME").is_some()
                || non_empty_env("WSL_INTEROP").is_some(),
            term,
            term_program,
            term_program_version,
            iterm_profile,
            iterm_mouse_reporting,
        }
    }

    pub(super) fn term_is_missing_or_dumb(&self) -> bool {
        self.term
            .as_deref()
            .is_none_or(|term| term.trim().is_empty() || term == "dumb")
    }

    pub(super) fn has_multiplexer(&self) -> bool {
        self.tmux || self.screen
    }

    pub(super) fn has_clipboard_bridge_risk(&self) -> bool {
        self.has_multiplexer() || self.ssh || self.wsl
    }

    pub(super) fn multiplexer_label(&self) -> &'static str {
        if self.tmux {
            "tmux"
        } else if self.screen {
            "screen"
        } else {
            "multiplexer"
        }
    }

    pub(super) fn clipboard_bridge_label(&self) -> String {
        let mut layers = Vec::new();
        if self.tmux {
            layers.push("tmux");
        }
        if self.screen {
            layers.push("screen");
        }
        if self.ssh {
            layers.push("ssh");
        }
        if self.wsl {
            layers.push("wsl");
        }
        if layers.is_empty() {
            "terminal bridge".to_owned()
        } else {
            layers.join("+")
        }
    }

    pub(super) fn profile_summary(&self) -> String {
        let mut parts = Vec::new();
        if let Some(term_program) = self.term_program.as_deref() {
            parts.push(format!("TERM_PROGRAM={term_program}"));
        }
        if let Some(version) = self.term_program_version.as_deref() {
            parts.push(format!("TERM_PROGRAM_VERSION={version}"));
        }
        if let Some(profile) = self.iterm_profile.as_deref() {
            parts.push(format!("ITERM_PROFILE={profile}"));
        }
        if let Some(mouse_reporting) = self.iterm_mouse_reporting {
            parts.push(format!("iterm_mouse_reporting={mouse_reporting}"));
        }
        if self.wezterm {
            parts.push("profile=wezterm".to_owned());
        }
        if self.kitty {
            parts.push("profile=kitty".to_owned());
        }
        if self.windows_terminal {
            parts.push("profile=windows_terminal".to_owned());
        }
        if self.tmux {
            parts.push("layer=tmux".to_owned());
        }
        if self.screen {
            parts.push("layer=screen".to_owned());
        }
        if self.ssh {
            parts.push("layer=ssh".to_owned());
        }
        if self.wsl {
            parts.push("layer=wsl".to_owned());
        }
        if parts.is_empty() {
            "profile=unknown".to_owned()
        } else {
            parts.join(" ")
        }
    }
}

fn iterm_mouse_reporting_for_profile(profile: &str) -> Option<bool> {
    let home = env::var_os("HOME")?;
    let plist = PathBuf::from(home)
        .join("Library")
        .join("Preferences")
        .join("com.googlecode.iterm2.plist");
    let bookmarks = plistbuddy_print(&plist, "Print :\"New Bookmarks\"")?;
    iterm_mouse_reporting_from_bookmarks(&bookmarks, profile)
}

pub(super) fn iterm_mouse_reporting_from_bookmarks(bookmarks: &str, profile: &str) -> Option<bool> {
    let mut depth = 0usize;
    let mut in_profile = false;
    let mut profile_name = None;
    let mut mouse_reporting = None;

    for raw_line in bookmarks.lines() {
        let line = raw_line.trim();
        if line.ends_with('{') {
            if line == "Dict {" && depth == 1 {
                in_profile = true;
                profile_name = None;
                mouse_reporting = None;
            }
            depth = depth.saturating_add(1);
            continue;
        }
        if line == "}" {
            if in_profile && depth == 2 {
                if profile_name.as_deref() == Some(profile) {
                    return mouse_reporting;
                }
                in_profile = false;
            }
            depth = depth.saturating_sub(1);
            continue;
        }
        if !in_profile || depth != 2 {
            continue;
        }
        if let Some(value) = plistbuddy_line_value(line, "Name") {
            profile_name = Some(value.to_owned());
        }
        if let Some(value) = plistbuddy_line_value(line, "Mouse Reporting") {
            mouse_reporting = parse_plist_bool(value);
        }
    }
    None
}

fn plistbuddy_line_value<'a>(line: &'a str, key: &str) -> Option<&'a str> {
    let value = line
        .strip_prefix(&format!("{key} = "))
        .or_else(|| line.strip_prefix(&format!("\"{key}\" = ")))?;
    Some(value.trim().trim_matches('"'))
}

fn parse_plist_bool(value: &str) -> Option<bool> {
    match value.trim() {
        "true" => Some(true),
        "false" => Some(false),
        _ => None,
    }
}

fn plistbuddy_print(plist: &Path, command: &str) -> Option<String> {
    let output = Command::new("/usr/libexec/PlistBuddy")
        .args(["-c", command])
        .arg(plist)
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    String::from_utf8(output.stdout).ok()
}

fn non_empty_env(name: &str) -> Option<String> {
    env::var(name)
        .ok()
        .map(|value| value.trim().to_owned())
        .filter(|value| !value.is_empty())
}
