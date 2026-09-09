use super::*;

fn environment_from(source: &[(&str, &str)]) -> BTreeMap<String, String> {
    controlled_shell_environment_with(|name| {
        source
            .iter()
            .find_map(|(key, value)| (*key == name).then(|| (*value).to_owned()))
    })
}

#[test]
fn explicit_toolchain_roots_take_precedence_over_home_fallback() {
    let environment = environment_from(&[
        ("HOME", "/fixture-home"),
        ("CARGO_HOME", "/explicit cargo/store"),
        ("RUSTUP_HOME", "/explicit rustup/store"),
    ]);

    assert_eq!(environment["CARGO_HOME"], "/explicit cargo/store");
    assert_eq!(environment["RUSTUP_HOME"], "/explicit rustup/store");
    assert!(!environment.contains_key("HOME"));
}

#[test]
fn only_the_missing_toolchain_root_uses_home_fallback() {
    for (explicit_name, missing_name, relative) in [
        ("CARGO_HOME", "RUSTUP_HOME", ".rustup"),
        ("RUSTUP_HOME", "CARGO_HOME", ".cargo"),
    ] {
        let environment = environment_from(&[
            ("HOME", "/fixture-home"),
            (explicit_name, "/explicit-store"),
        ]);

        assert_eq!(environment[explicit_name], "/explicit-store");
        assert_eq!(
            environment[missing_name],
            Path::new("/fixture-home")
                .join(relative)
                .to_string_lossy()
                .as_ref()
        );
        assert!(!environment.contains_key("HOME"));
    }
}

#[test]
fn toolchain_fallback_requires_home_and_never_invents_roots() {
    let from_home = environment_from(&[("HOME", "/fixture-home")]);
    for (name, relative) in [("CARGO_HOME", ".cargo"), ("RUSTUP_HOME", ".rustup")] {
        assert_eq!(
            from_home[name],
            Path::new("/fixture-home")
                .join(relative)
                .to_string_lossy()
                .as_ref()
        );
    }
    let absent = environment_from(&[]);
    assert!(!absent.contains_key("CARGO_HOME"));
    assert!(!absent.contains_key("RUSTUP_HOME"));
    assert!(!absent.contains_key("HOME"));
    assert!(absent.get("PATH").is_some_and(|value| !value.is_empty()));
}

#[test]
fn controlled_environment_preserves_only_baseline_and_toolchain_roots() {
    let baseline = [
        ("PATH", "/fixture-bin:/usr/bin:/bin"),
        ("LANG", "en_US.UTF-8"),
        ("LC_ALL", "C"),
        ("LC_CTYPE", "UTF-8"),
        ("TZ", "UTC"),
        ("CARGO_HOME", "/fixture-cargo"),
        ("RUSTUP_HOME", "/fixture-rustup"),
    ];
    let mut source = baseline.to_vec();
    source.extend([
        ("HOME", "/fixture-home"),
        ("TOKEN", "fixture-secret"),
        ("SIGIL_API_KEY", "fixture-secret"),
        ("BASH_ENV", "/fixture-shell-init"),
        ("ENV", "/fixture-shell-init"),
        ("RUSTUP_TOOLCHAIN", "unselected-toolchain"),
        ("CARGO_REGISTRIES_TOKEN", "fixture-secret"),
    ]);
    let environment = environment_from(&source);
    let expected = baseline
        .into_iter()
        .map(|(name, value)| (name.to_owned(), value.to_owned()))
        .collect::<BTreeMap<_, _>>();

    assert_eq!(environment, expected);
}

#[test]
fn each_toolchain_root_changes_the_v2_environment_binding() -> Result<()> {
    let environment = environment_from(&[
        ("CARGO_HOME", "/fixture-cargo"),
        ("RUSTUP_HOME", "/fixture-rustup"),
    ]);
    let original = shell_environment_binding_for_environment("/bin/sh", true, &environment)?;
    assert!(original.starts_with("shell-env-v2:"));

    for name in ["CARGO_HOME", "RUSTUP_HOME"] {
        let mut changed = environment.clone();
        changed.insert(name.to_owned(), format!("/changed-{name}"));
        let binding = shell_environment_binding_for_environment("/bin/sh", true, &changed)?;
        assert!(binding.starts_with("shell-env-v2:"));
        assert_ne!(
            binding, original,
            "{name} must participate in admission binding"
        );
    }
    Ok(())
}

#[test]
fn environment_binding_is_stable_across_map_insertion_order() -> Result<()> {
    let entries = [
        ("PATH", "/fixture-bin"),
        ("CARGO_HOME", "/fixture-cargo"),
        ("RUSTUP_HOME", "/fixture-rustup"),
    ];
    let forward = entries
        .iter()
        .map(|(name, value)| ((*name).to_owned(), (*value).to_owned()))
        .collect::<BTreeMap<_, _>>();
    let reverse = entries
        .iter()
        .rev()
        .map(|(name, value)| ((*name).to_owned(), (*value).to_owned()))
        .collect::<BTreeMap<_, _>>();

    assert_eq!(
        shell_environment_binding_for_environment("/bin/sh", true, &forward)?,
        shell_environment_binding_for_environment("/bin/sh", true, &reverse)?
    );
    Ok(())
}
