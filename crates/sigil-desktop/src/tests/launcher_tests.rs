use super::*;

#[test]
fn launch_request_debug_redacts_local_paths() {
    let canary = "/private/canary/workspace";
    let request = DesktopLaunchRequest::new(
        "/private/canary/sigil",
        "/private/canary/sigil.toml",
        canary,
    );
    let debug = format!("{request:?}");

    assert!(!debug.contains(canary));
    assert!(!debug.contains("sigil.toml"));
    assert!(debug.contains("<local path>"));
}

#[tokio::test]
async fn startup_line_reader_enforces_single_record_cap() {
    let mut valid = &b"{\"schema_version\":1}\nignored"[..];
    assert_eq!(
        read_startup_line(&mut valid)
            .await
            .expect("line should decode"),
        br#"{"schema_version":1}"#
    );

    let oversized = vec![b'x'; MAX_BOOTSTRAP_BYTES + 1];
    let mut oversized = oversized.as_slice();
    assert!(matches!(
        read_startup_line(&mut oversized).await,
        Err(DesktopLaunchError::ReadinessTooLarge)
    ));
}
