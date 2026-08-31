use std::{
    fs, io,
    path::{Path, PathBuf},
    process::Command,
};

pub struct IsolatedChildEnvironment {
    home: PathBuf,
    config_home: PathBuf,
    cache_home: PathBuf,
    state_home: PathBuf,
    runtime_home: PathBuf,
    app_data: PathBuf,
    local_app_data: PathBuf,
}

pub fn isolated_child_environment(workspace: &Path) -> io::Result<IsolatedChildEnvironment> {
    isolated_child_environment_for_home(&workspace.join(".process-home"))
}

pub fn isolated_child_environment_for_home(home: &Path) -> io::Result<IsolatedChildEnvironment> {
    let environment = IsolatedChildEnvironment {
        home: home.to_path_buf(),
        config_home: home.join(".config"),
        cache_home: home.join(".cache"),
        state_home: home.join(".local/state"),
        runtime_home: home.join(".runtime"),
        app_data: home.join("AppData/Roaming"),
        local_app_data: home.join("AppData/Local"),
    };
    for path in [
        &environment.home,
        &environment.config_home,
        &environment.cache_home,
        &environment.state_home,
        &environment.runtime_home,
        &environment.app_data,
        &environment.local_app_data,
    ] {
        fs::create_dir_all(path)?;
    }
    Ok(environment)
}

impl IsolatedChildEnvironment {
    // Some integration targets only start PTY children, while others only start stdio children.
    #[allow(dead_code)]
    pub fn apply_to_command(&self, command: &mut Command) {
        command
            .env("HOME", &self.home)
            .env("USERPROFILE", &self.home)
            .env("XDG_CONFIG_HOME", &self.config_home)
            .env("XDG_CACHE_HOME", &self.cache_home)
            .env("XDG_STATE_HOME", &self.state_home)
            .env("XDG_RUNTIME_DIR", &self.runtime_home)
            .env("APPDATA", &self.app_data)
            .env("LOCALAPPDATA", &self.local_app_data)
            .env_remove("HOMEDRIVE")
            .env_remove("HOMEPATH")
            .env_remove("SIGIL_STATE_HOME")
            .env_remove("SIGIL_CACHE_HOME")
            .env_remove("SIGIL_SCRATCH_DIR")
            .env_remove("SIGIL_CONFIG")
            .env_remove("SIGIL_CONFIG_PATH");
    }

    // See `apply_to_command`: the shared module is compiled separately by each integration target.
    #[allow(dead_code)]
    pub fn apply_to_command_builder(&self, command: &mut portable_pty::CommandBuilder) {
        command.env("HOME", self.home.as_os_str());
        command.env("USERPROFILE", self.home.as_os_str());
        command.env("XDG_CONFIG_HOME", self.config_home.as_os_str());
        command.env("XDG_CACHE_HOME", self.cache_home.as_os_str());
        command.env("XDG_STATE_HOME", self.state_home.as_os_str());
        command.env("XDG_RUNTIME_DIR", self.runtime_home.as_os_str());
        command.env("APPDATA", self.app_data.as_os_str());
        command.env("LOCALAPPDATA", self.local_app_data.as_os_str());
        command.env_remove("HOMEDRIVE");
        command.env_remove("HOMEPATH");
        command.env_remove("SIGIL_STATE_HOME");
        command.env_remove("SIGIL_CACHE_HOME");
        command.env_remove("SIGIL_SCRATCH_DIR");
        command.env_remove("SIGIL_CONFIG");
        command.env_remove("SIGIL_CONFIG_PATH");
    }
}

#[cfg(test)]
mod tests {
    use std::{
        collections::BTreeMap,
        ffi::{OsStr, OsString},
        io,
    };

    use super::*;

    #[test]
    fn child_environment_uses_fixture_identity_and_clears_sigil_root_overrides() -> io::Result<()> {
        let workspace = tempfile::tempdir()?;
        let environment = isolated_child_environment(workspace.path())?;
        let mut command = Command::new("unused-child");
        command
            .env("SIGIL_STATE_HOME", "/inherited/sigil-state")
            .env("SIGIL_CACHE_HOME", "/inherited/sigil-cache")
            .env("SIGIL_SCRATCH_DIR", "/inherited/scratch")
            .env("SIGIL_CONFIG", "/inherited/sigil.toml")
            .env("SIGIL_CONFIG_PATH", "/inherited/sigil-path.toml")
            .env("HOMEDRIVE", "C:")
            .env("HOMEPATH", r"\inherited\profile");
        environment.apply_to_command(&mut command);
        let values = command
            .get_envs()
            .map(|(key, value)| (key.to_os_string(), value.map(OsStr::to_os_string)))
            .collect::<BTreeMap<OsString, Option<OsString>>>();

        let home = workspace.path().join(".process-home");
        let home_value = Some(home.clone().into_os_string());
        assert_eq!(values.get(OsStr::new("HOME")), Some(&home_value));
        assert_eq!(values.get(OsStr::new("USERPROFILE")), Some(&home_value));
        assert_eq!(values.get(OsStr::new("SIGIL_STATE_HOME")), Some(&None));
        assert_eq!(values.get(OsStr::new("SIGIL_CACHE_HOME")), Some(&None));
        assert_eq!(values.get(OsStr::new("SIGIL_SCRATCH_DIR")), Some(&None));
        assert_eq!(values.get(OsStr::new("SIGIL_CONFIG")), Some(&None));
        assert_eq!(values.get(OsStr::new("SIGIL_CONFIG_PATH")), Some(&None));
        assert_eq!(values.get(OsStr::new("HOMEDRIVE")), Some(&None));
        assert_eq!(values.get(OsStr::new("HOMEPATH")), Some(&None));
        let local_app_data = Some(home.join("AppData/Local").into_os_string());
        assert_eq!(
            values.get(OsStr::new("LOCALAPPDATA")),
            Some(&local_app_data)
        );
        Ok(())
    }
}
