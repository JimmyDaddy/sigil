#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PaneFocus {
    Composer,
    Activity,
}

impl PaneFocus {
    pub fn label(self) -> &'static str {
        match self {
            Self::Composer => "composer",
            Self::Activity => "activity",
        }
    }
}

#[cfg(all(test, not(sigil_tui_test_slice_app_input_flow)))]
#[path = "tests/input_tests.rs"]
mod tests;
