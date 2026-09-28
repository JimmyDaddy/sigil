use super::*;

#[test]
fn plugin_cleanup_ipc_keeps_trust_and_process_facts_separate() {
    for (status, wire) in [
        (
            sigil_desktop::DesktopPluginCleanupStatus::Confirmed,
            "confirmed",
        ),
        (
            sigil_desktop::DesktopPluginCleanupStatus::Unknown,
            "unknown",
        ),
        (
            sigil_desktop::DesktopPluginCleanupStatus::Unconfirmed,
            "unconfirmed",
        ),
    ] {
        let catalog = DesktopPluginCatalog::from(sigil_desktop::DesktopPluginCatalog {
            warning_count: 0,
            plugins: vec![sigil_desktop::DesktopPluginReview {
                plugin_id: "review".to_owned(),
                name: "Review".to_owned(),
                version: "1".to_owned(),
                manifest_hash: "manifest".to_owned(),
                capability_digest: "capabilities".to_owned(),
                trust: "trusted".to_owned(),
                process_cleanup: Some(status),
                capabilities: Vec::new(),
            }],
        });
        let value = serde_json::to_value(catalog).expect("catalog serializes");
        assert_eq!(value["plugins"][0]["trust"], "trusted");
        assert_eq!(value["plugins"][0]["processCleanup"], wire);
        let receipt = DesktopPluginReviewReceipt::from(sigil_desktop::DesktopPluginReviewReceipt {
            plugin_id: "review".to_owned(),
            enabled: false,
            process_cleanup: Some(status),
        });
        let value = serde_json::to_value(receipt).expect("receipt serializes");
        assert_eq!(value["enabled"], false);
        assert_eq!(value["processCleanup"], wire);
    }
    let receipt = DesktopPluginReviewReceipt::from(sigil_desktop::DesktopPluginReviewReceipt {
        plugin_id: "review".to_owned(),
        enabled: false,
        process_cleanup: None,
    });
    assert!(serde_json::to_value(receipt).expect("receipt serializes")["processCleanup"].is_null());
}
