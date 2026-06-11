use super::*;

#[test]
fn setup_field_navigation_wraps() {
    assert_eq!(SetupField::TrustCurrentFolder.next(), SetupField::Model);
    assert_eq!(SetupField::Save.next(), SetupField::TrustCurrentFolder);
    assert_eq!(SetupField::TrustCurrentFolder.previous(), SetupField::Save);
}

#[test]
fn setup_state_masks_api_key() {
    let mut state = SetupState::new(PathBuf::from("/tmp/sigil.toml"), None);

    assert_eq!(state.masked_api_key(), "<empty>");

    state.api_key = "secret".to_owned();
    assert_eq!(state.masked_api_key(), "********");
}
