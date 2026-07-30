use std::str::FromStr;

use semver::Version;

use super::UpdateChannel;

#[test]
fn stable_rejects_prereleases_and_beta_accepts_stable_releases()
-> Result<(), Box<dyn std::error::Error>> {
    let current = Version::parse("1.2.3-beta.1")?;
    let prerelease = Version::parse("1.2.3-beta.2")?;
    let stable = Version::parse("1.2.3")?;

    assert!(!UpdateChannel::Stable.accepts(&current, &prerelease, true));
    assert!(UpdateChannel::Beta.accepts(&current, &stable, false));
    Ok(())
}

#[test]
fn beta_rejects_other_prerelease_channels() -> Result<(), Box<dyn std::error::Error>> {
    let current = Version::parse("1.2.3-beta.1")?;
    for version in ["1.2.3-alpha.9", "1.2.3-rc.1", "1.2.3-preview.1"] {
        let candidate = Version::parse(version)?;
        assert!(!UpdateChannel::Beta.accepts(&current, &candidate, true));
    }
    Ok(())
}

#[test]
fn current_follows_the_installed_prerelease_channel() -> Result<(), Box<dyn std::error::Error>> {
    let current = Version::parse("1.2.3-alpha.1")?;
    let next_alpha = Version::parse("1.2.3-alpha.2")?;
    let beta = Version::parse("1.2.3-beta.1")?;
    let release_candidate = Version::parse("1.2.3-rc.1")?;
    let stable = Version::parse("1.2.3")?;

    assert!(UpdateChannel::Current.accepts(&current, &next_alpha, true));
    assert!(!UpdateChannel::Current.accepts(&current, &beta, true));
    assert!(!UpdateChannel::Current.accepts(&current, &release_candidate, true));
    assert!(UpdateChannel::Current.accepts(&current, &stable, false));
    Ok(())
}

#[test]
fn channel_parser_is_explicit() -> Result<(), Box<dyn std::error::Error>> {
    assert_eq!(UpdateChannel::from_str("beta")?, UpdateChannel::Beta);
    assert!(UpdateChannel::from_str("nightly").is_err());
    Ok(())
}
