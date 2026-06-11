#[path = "common.rs"]
pub mod common;

use sigil_kernel::{CodeIntelStartup, CodeIntelligenceConfig};

use super::*;

#[test]
fn configured_status_line_matches_disabled_and_enabled_config() {
    assert_eq!(
        CodeIntelligenceService::configured_status_line(&CodeIntelligenceConfig::default()),
        "off"
    );

    let config = CodeIntelligenceConfig {
        enabled: true,
        startup: CodeIntelStartup::Lazy,
        ..CodeIntelligenceConfig::default()
    };
    assert_eq!(
        CodeIntelligenceService::configured_status_line(&config),
        "lazy"
    );
}
