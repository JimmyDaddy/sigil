use super::*;

#[test]
fn bearer_debug_is_redacted_and_has_256_bit_material() {
    let bearer = DesktopBearerToken::generate().expect("CSPRNG should be available");

    assert_eq!(bearer.expose().len(), 43);
    assert_eq!(format!("{bearer:?}"), "DesktopBearerToken(<redacted>)");
    assert!(!format!("{bearer:?}").contains(bearer.expose()));
}
