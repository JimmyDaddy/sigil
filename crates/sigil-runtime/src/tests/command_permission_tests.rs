use std::{fs, path::Path};

use anyhow::Result;
use sigil_kernel::RootConfig;

use super::append_command_allow_pattern;

fn config_fixture(directory: &Path) -> Result<()> {
    fs::write(
        directory.join("sigil.toml"),
        r#"
config_version = 2

[agent]
connection = "deepseek-default"
model = "deepseek-v4-flash"

[connections.deepseek-default]
label = "DeepSeek"
provider = "deepseek"
protocol = "deepseek"
base_url = "https://api.deepseek.com"
credential = { source = "environment", name = "SIGIL_API_KEY" }

[permission.commands]
allow = ["git status*"]
ask = ["git push*"]
"#,
    )?;
    Ok(())
}

#[test]
fn append_command_allow_pattern_round_trips() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let path = temp.path().join("sigil.toml");
    config_fixture(temp.path())?;

    append_command_allow_pattern(&path, "cargo test*")?;

    let config = RootConfig::load(&path)?;
    assert!(
        config
            .permission
            .commands
            .allow
            .iter()
            .any(|p| p == "cargo test*")
    );
    assert!(
        config
            .permission
            .commands
            .ask
            .iter()
            .any(|p| p == "git push*")
    );
    assert!(
        config
            .permission
            .commands
            .allow
            .iter()
            .any(|p| p == "git status*")
    );
    Ok(())
}

#[test]
fn append_command_allow_pattern_normalizes_and_dedups() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let path = temp.path().join("sigil.toml");
    config_fixture(temp.path())?;

    // Whitespace normalization matches the kernel matcher semantics.
    append_command_allow_pattern(&path, "  cargo   test  * ")?;
    append_command_allow_pattern(&path, "cargo test*")?;

    let config = RootConfig::load(&path)?;
    assert_eq!(
        config
            .permission
            .commands
            .allow
            .iter()
            .filter(|p| p.as_str() == "cargo test*")
            .count(),
        1
    );
    Ok(())
}

#[test]
fn append_command_allow_pattern_rejects_cross_group_conflict() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let path = temp.path().join("sigil.toml");
    config_fixture(temp.path())?;

    let error = append_command_allow_pattern(&path, "git push*").expect_err("must conflict");
    assert!(matches!(
        error,
        super::CommandPermissionPersistError::Conflict { .. }
    ));
    // The failed append must not mutate the persisted config.
    let config = RootConfig::load(&path)?;
    assert!(
        config
            .permission
            .commands
            .allow
            .iter()
            .all(|p| p != "git push*")
    );
    Ok(())
}
