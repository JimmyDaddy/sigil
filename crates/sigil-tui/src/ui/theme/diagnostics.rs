use ratatui::style::Color;
use sigil_kernel::{AppearanceConfig, ThemeId};

use super::{Theme, ThemePalette, contrast};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ThemeDiagnosticKind {
    ContrastPair,
    SemanticSeparation,
    StructuralCue,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ThemeDiagnosticTier {
    Safety,
    Core,
    Surface,
    Semantic,
    Structural,
    Advisory,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ThemeDiagnosticMetric {
    ContrastRatio,
    SrgbDistance,
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct ThemeDiagnostic {
    pub(crate) kind: ThemeDiagnosticKind,
    pub(crate) tier: ThemeDiagnosticTier,
    pub(crate) name: &'static str,
    pub(crate) tokens: [&'static str; 2],
    pub(crate) metric: ThemeDiagnosticMetric,
    pub(crate) actual: f32,
    pub(crate) minimum: f32,
    pub(crate) remediation: &'static str,
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct ThemeDiagnosticReport {
    pub(crate) theme_id: ThemeId,
    pub(crate) override_count: usize,
    pub(crate) checked: usize,
    pub(crate) diagnostics: Vec<ThemeDiagnostic>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ThemeDiagnosticError {
    InvalidAppearance(String),
}

impl ThemeDiagnosticError {
    pub(crate) fn message(&self) -> &str {
        match self {
            Self::InvalidAppearance(message) => message,
        }
    }
}

#[derive(Debug, Clone, Copy)]
struct ThemeRule {
    kind: ThemeDiagnosticKind,
    tier: ThemeDiagnosticTier,
    name: &'static str,
    tokens: [&'static str; 2],
    minimum: f32,
    remediation: &'static str,
}

pub(crate) fn diagnose_appearance(
    appearance: &AppearanceConfig,
) -> Result<ThemeDiagnosticReport, ThemeDiagnosticError> {
    let theme = Theme::try_from_config(appearance)
        .map_err(|error| ThemeDiagnosticError::InvalidAppearance(error.to_string()))?;
    let mut diagnostics = Vec::new();
    let mut checked = 0;

    for rule in all_rules() {
        let Some(actual) = evaluate_rule(rule, &theme.palette) else {
            continue;
        };
        checked += 1;
        if rule_has_override(rule, appearance) && actual < rule.minimum {
            diagnostics.push(ThemeDiagnostic {
                kind: rule.kind,
                tier: rule.tier,
                name: rule.name,
                tokens: rule.tokens,
                metric: metric_for_kind(rule.kind),
                actual,
                minimum: rule.minimum,
                remediation: rule.remediation,
            });
        }
    }

    Ok(ThemeDiagnosticReport {
        theme_id: theme.id,
        override_count: appearance.colors.len(),
        checked,
        diagnostics,
    })
}

fn rule_has_override(rule: &ThemeRule, appearance: &AppearanceConfig) -> bool {
    rule.tokens
        .iter()
        .any(|token| appearance.colors.get(token).is_some())
}

fn all_rules() -> impl Iterator<Item = &'static ThemeRule> {
    CONTRAST_RULES
        .iter()
        .chain(SURFACE_RULES)
        .chain(SEMANTIC_RULES)
        .chain(STRUCTURAL_RULES)
}

fn evaluate_rule(rule: &ThemeRule, palette: &ThemePalette) -> Option<f32> {
    let first = color_for_token(palette, rule.tokens[0])?;
    let second = color_for_token(palette, rule.tokens[1])?;
    match rule.kind {
        ThemeDiagnosticKind::ContrastPair | ThemeDiagnosticKind::StructuralCue => {
            contrast::contrast_ratio(first, second)
        }
        ThemeDiagnosticKind::SemanticSeparation => srgb_distance(first, second),
    }
}

fn metric_for_kind(kind: ThemeDiagnosticKind) -> ThemeDiagnosticMetric {
    match kind {
        ThemeDiagnosticKind::ContrastPair | ThemeDiagnosticKind::StructuralCue => {
            ThemeDiagnosticMetric::ContrastRatio
        }
        ThemeDiagnosticKind::SemanticSeparation => ThemeDiagnosticMetric::SrgbDistance,
    }
}

fn srgb_distance(first: Color, second: Color) -> Option<f32> {
    let (first_red, first_green, first_blue) = contrast::rgb(first)?;
    let (second_red, second_green, second_blue) = contrast::rgb(second)?;
    let red_delta = first_red - second_red;
    let green_delta = first_green - second_green;
    let blue_delta = first_blue - second_blue;
    Some(
        ((red_delta * red_delta + green_delta * green_delta + blue_delta * blue_delta) / 3.0)
            .sqrt(),
    )
}

fn color_for_token(palette: &ThemePalette, token: &str) -> Option<Color> {
    Some(match token {
        "surface_base" => palette.surface_base,
        "surface_rail" => palette.surface_rail,
        "surface_panel" => palette.surface_panel,
        "surface_panel_alt" => palette.surface_panel_alt,
        "surface_input" => palette.surface_input,
        "surface_agent_panel" => palette.surface_agent_panel,
        "surface_badge" => palette.surface_badge,
        "surface_selection" => palette.surface_selection,
        "surface_user_message" => palette.surface_user_message,
        "surface_code" => palette.surface_code,
        "border_subtle" => palette.border_subtle,
        "border_strong" => palette.border_strong,
        "border_focus" => palette.border_focus,
        "border_danger" => palette.border_danger,
        "text_primary" => palette.text_primary,
        "text_secondary" => palette.text_secondary,
        "text_muted" => palette.text_muted,
        "text_inverse" => palette.text_inverse,
        "accent_info" => palette.accent_info,
        "config_primary" => palette.config_primary,
        "config_detail" => palette.config_detail,
        "config_warning" => palette.config_warning,
        "config_danger" => palette.config_danger,
        "config_bg" => palette.config_bg,
        "config_border" => palette.config_border,
        "config_selected_bg" => palette.config_selected_bg,
        "setup_bg" => palette.setup_bg,
        "selection_fg" => palette.selection_fg,
        "selection_bg" => palette.selection_bg,
        "button_selected_fg" => palette.button_selected_fg,
        "button_selected_bg" => palette.button_selected_bg,
        "diff_header_fg" => palette.diff_header_fg,
        "diff_hunk_fg" => palette.diff_hunk_fg,
        "diff_added_fg" => palette.diff_added_fg,
        "diff_added_bg" => palette.diff_added_bg,
        "diff_removed_fg" => palette.diff_removed_fg,
        "diff_removed_bg" => palette.diff_removed_bg,
        "diff_context_fg" => palette.diff_context_fg,
        "diff_gutter_fg" => palette.diff_gutter_fg,
        "approval_bg" => palette.approval_bg,
        "approval_border" => palette.approval_border,
        "approval_allow_bg" => palette.approval_allow_bg,
        "approval_deny_bg" => palette.approval_deny_bg,
        "approval_selected_bg" => palette.approval_selected_bg,
        "risk_low" => palette.risk_low,
        "risk_medium" => palette.risk_medium,
        "risk_high" => palette.risk_high,
        "markdown_heading" => palette.markdown_heading,
        "markdown_quote_text" => palette.markdown_quote_text,
        "markdown_code_fg" => palette.markdown_code_fg,
        "markdown_code_bg" => palette.markdown_code_bg,
        "markdown_link" => palette.markdown_link,
        "markdown_quote_bar" => palette.markdown_quote_bar,
        "markdown_rule" => palette.markdown_rule,
        "modal_bg" => palette.modal_bg,
        "modal_border" => palette.modal_border,
        "overlay_bg" => palette.overlay_bg,
        "status_success" => palette.status_success,
        "status_warning" => palette.status_warning,
        "status_error" => palette.status_error,
        "status_pending" => palette.status_pending,
        _ => return None,
    })
}

const TEXT_REMEDIATION: &str =
    "adjust the listed [appearance.colors] foreground or background token";
const SAFETY_REMEDIATION: &str =
    "adjust the listed [appearance.colors] tokens so safety-critical UI stays readable";
const SEMANTIC_REMEDIATION: &str =
    "adjust the listed [appearance.colors] tokens so state colors remain visually distinct";
const STRUCTURAL_REMEDIATION: &str =
    "adjust border or background override tokens so structural cues remain visible";

const CONTRAST_RULES: &[ThemeRule] = &[
    ThemeRule {
        kind: ThemeDiagnosticKind::ContrastPair,
        tier: ThemeDiagnosticTier::Core,
        name: "text-base",
        tokens: ["text_primary", "surface_base"],
        minimum: 4.5,
        remediation: TEXT_REMEDIATION,
    },
    ThemeRule {
        kind: ThemeDiagnosticKind::ContrastPair,
        tier: ThemeDiagnosticTier::Core,
        name: "text-panel",
        tokens: ["text_primary", "surface_panel"],
        minimum: 4.5,
        remediation: TEXT_REMEDIATION,
    },
    ThemeRule {
        kind: ThemeDiagnosticKind::ContrastPair,
        tier: ThemeDiagnosticTier::Core,
        name: "secondary-base",
        tokens: ["text_secondary", "surface_base"],
        minimum: 3.0,
        remediation: TEXT_REMEDIATION,
    },
    ThemeRule {
        kind: ThemeDiagnosticKind::ContrastPair,
        tier: ThemeDiagnosticTier::Advisory,
        name: "muted-base",
        tokens: ["text_muted", "surface_base"],
        minimum: 3.0,
        remediation: TEXT_REMEDIATION,
    },
    ThemeRule {
        kind: ThemeDiagnosticKind::ContrastPair,
        tier: ThemeDiagnosticTier::Core,
        name: "selection",
        tokens: ["selection_fg", "selection_bg"],
        minimum: 3.0,
        remediation: TEXT_REMEDIATION,
    },
    ThemeRule {
        kind: ThemeDiagnosticKind::ContrastPair,
        tier: ThemeDiagnosticTier::Core,
        name: "selected-button",
        tokens: ["button_selected_fg", "button_selected_bg"],
        minimum: 3.0,
        remediation: TEXT_REMEDIATION,
    },
    ThemeRule {
        kind: ThemeDiagnosticKind::ContrastPair,
        tier: ThemeDiagnosticTier::Safety,
        name: "diff-added",
        tokens: ["diff_added_fg", "diff_added_bg"],
        minimum: 3.0,
        remediation: SAFETY_REMEDIATION,
    },
    ThemeRule {
        kind: ThemeDiagnosticKind::ContrastPair,
        tier: ThemeDiagnosticTier::Safety,
        name: "diff-removed",
        tokens: ["diff_removed_fg", "diff_removed_bg"],
        minimum: 3.0,
        remediation: SAFETY_REMEDIATION,
    },
    ThemeRule {
        kind: ThemeDiagnosticKind::ContrastPair,
        tier: ThemeDiagnosticTier::Core,
        name: "markdown-heading",
        tokens: ["markdown_heading", "surface_base"],
        minimum: 3.0,
        remediation: TEXT_REMEDIATION,
    },
    ThemeRule {
        kind: ThemeDiagnosticKind::ContrastPair,
        tier: ThemeDiagnosticTier::Core,
        name: "markdown-link",
        tokens: ["markdown_link", "surface_base"],
        minimum: 3.0,
        remediation: TEXT_REMEDIATION,
    },
    ThemeRule {
        kind: ThemeDiagnosticKind::ContrastPair,
        tier: ThemeDiagnosticTier::Advisory,
        name: "markdown-quote",
        tokens: ["markdown_quote_text", "surface_base"],
        minimum: 3.0,
        remediation: TEXT_REMEDIATION,
    },
    ThemeRule {
        kind: ThemeDiagnosticKind::ContrastPair,
        tier: ThemeDiagnosticTier::Core,
        name: "markdown-code",
        tokens: ["markdown_code_fg", "markdown_code_bg"],
        minimum: 4.5,
        remediation: TEXT_REMEDIATION,
    },
    ThemeRule {
        kind: ThemeDiagnosticKind::ContrastPair,
        tier: ThemeDiagnosticTier::Safety,
        name: "approval-body",
        tokens: ["text_primary", "approval_bg"],
        minimum: 4.5,
        remediation: SAFETY_REMEDIATION,
    },
    ThemeRule {
        kind: ThemeDiagnosticKind::ContrastPair,
        tier: ThemeDiagnosticTier::Safety,
        name: "approval-allow",
        tokens: ["text_inverse", "approval_allow_bg"],
        minimum: 3.0,
        remediation: SAFETY_REMEDIATION,
    },
    ThemeRule {
        kind: ThemeDiagnosticKind::ContrastPair,
        tier: ThemeDiagnosticTier::Safety,
        name: "approval-deny",
        tokens: ["text_inverse", "approval_deny_bg"],
        minimum: 3.0,
        remediation: SAFETY_REMEDIATION,
    },
    ThemeRule {
        kind: ThemeDiagnosticKind::ContrastPair,
        tier: ThemeDiagnosticTier::Safety,
        name: "approval-selected",
        tokens: ["text_inverse", "approval_selected_bg"],
        minimum: 3.0,
        remediation: SAFETY_REMEDIATION,
    },
    ThemeRule {
        kind: ThemeDiagnosticKind::ContrastPair,
        tier: ThemeDiagnosticTier::Core,
        name: "diff-header",
        tokens: ["diff_header_fg", "surface_code"],
        minimum: 3.0,
        remediation: TEXT_REMEDIATION,
    },
    ThemeRule {
        kind: ThemeDiagnosticKind::ContrastPair,
        tier: ThemeDiagnosticTier::Core,
        name: "diff-hunk",
        tokens: ["diff_hunk_fg", "surface_code"],
        minimum: 3.0,
        remediation: TEXT_REMEDIATION,
    },
    ThemeRule {
        kind: ThemeDiagnosticKind::ContrastPair,
        tier: ThemeDiagnosticTier::Core,
        name: "diff-context",
        tokens: ["diff_context_fg", "surface_code"],
        minimum: 3.0,
        remediation: TEXT_REMEDIATION,
    },
    ThemeRule {
        kind: ThemeDiagnosticKind::ContrastPair,
        tier: ThemeDiagnosticTier::Advisory,
        name: "diff-gutter",
        tokens: ["diff_gutter_fg", "surface_code"],
        minimum: 2.0,
        remediation: TEXT_REMEDIATION,
    },
    ThemeRule {
        kind: ThemeDiagnosticKind::ContrastPair,
        tier: ThemeDiagnosticTier::Core,
        name: "config-detail",
        tokens: ["config_detail", "config_bg"],
        minimum: 3.0,
        remediation: TEXT_REMEDIATION,
    },
    ThemeRule {
        kind: ThemeDiagnosticKind::ContrastPair,
        tier: ThemeDiagnosticTier::Core,
        name: "config-warning",
        tokens: ["config_warning", "config_bg"],
        minimum: 3.0,
        remediation: TEXT_REMEDIATION,
    },
    ThemeRule {
        kind: ThemeDiagnosticKind::ContrastPair,
        tier: ThemeDiagnosticTier::Core,
        name: "config-danger",
        tokens: ["config_danger", "config_bg"],
        minimum: 3.0,
        remediation: TEXT_REMEDIATION,
    },
    ThemeRule {
        kind: ThemeDiagnosticKind::ContrastPair,
        tier: ThemeDiagnosticTier::Core,
        name: "config-selected",
        tokens: ["text_primary", "config_selected_bg"],
        minimum: 3.0,
        remediation: TEXT_REMEDIATION,
    },
];

const SURFACE_RULES: &[ThemeRule] = &[
    ThemeRule {
        kind: ThemeDiagnosticKind::ContrastPair,
        tier: ThemeDiagnosticTier::Surface,
        name: "rail-title",
        tokens: ["text_primary", "surface_rail"],
        minimum: 4.5,
        remediation: TEXT_REMEDIATION,
    },
    ThemeRule {
        kind: ThemeDiagnosticKind::ContrastPair,
        tier: ThemeDiagnosticTier::Surface,
        name: "rail-muted",
        tokens: ["text_muted", "surface_rail"],
        minimum: 3.0,
        remediation: TEXT_REMEDIATION,
    },
    ThemeRule {
        kind: ThemeDiagnosticKind::ContrastPair,
        tier: ThemeDiagnosticTier::Surface,
        name: "composer-input",
        tokens: ["text_primary", "surface_input"],
        minimum: 4.5,
        remediation: TEXT_REMEDIATION,
    },
    ThemeRule {
        kind: ThemeDiagnosticKind::ContrastPair,
        tier: ThemeDiagnosticTier::Surface,
        name: "user-message",
        tokens: ["text_primary", "surface_user_message"],
        minimum: 4.5,
        remediation: TEXT_REMEDIATION,
    },
    ThemeRule {
        kind: ThemeDiagnosticKind::ContrastPair,
        tier: ThemeDiagnosticTier::Surface,
        name: "agent-panel-active",
        tokens: ["accent_info", "surface_agent_panel"],
        minimum: 3.0,
        remediation: TEXT_REMEDIATION,
    },
    ThemeRule {
        kind: ThemeDiagnosticKind::ContrastPair,
        tier: ThemeDiagnosticTier::Surface,
        name: "agent-panel-detail",
        tokens: ["text_secondary", "surface_agent_panel"],
        minimum: 3.0,
        remediation: TEXT_REMEDIATION,
    },
    ThemeRule {
        kind: ThemeDiagnosticKind::ContrastPair,
        tier: ThemeDiagnosticTier::Surface,
        name: "agent-panel-muted",
        tokens: ["text_muted", "surface_agent_panel"],
        minimum: 3.0,
        remediation: TEXT_REMEDIATION,
    },
    ThemeRule {
        kind: ThemeDiagnosticKind::ContrastPair,
        tier: ThemeDiagnosticTier::Surface,
        name: "status-badge-success",
        tokens: ["status_success", "surface_badge"],
        minimum: 3.0,
        remediation: TEXT_REMEDIATION,
    },
    ThemeRule {
        kind: ThemeDiagnosticKind::ContrastPair,
        tier: ThemeDiagnosticTier::Surface,
        name: "status-badge-warning",
        tokens: ["status_warning", "surface_badge"],
        minimum: 3.0,
        remediation: TEXT_REMEDIATION,
    },
    ThemeRule {
        kind: ThemeDiagnosticKind::ContrastPair,
        tier: ThemeDiagnosticTier::Surface,
        name: "status-badge-error",
        tokens: ["status_error", "surface_badge"],
        minimum: 3.0,
        remediation: TEXT_REMEDIATION,
    },
    ThemeRule {
        kind: ThemeDiagnosticKind::ContrastPair,
        tier: ThemeDiagnosticTier::Surface,
        name: "status-badge-pending",
        tokens: ["status_pending", "surface_badge"],
        minimum: 3.0,
        remediation: TEXT_REMEDIATION,
    },
    ThemeRule {
        kind: ThemeDiagnosticKind::ContrastPair,
        tier: ThemeDiagnosticTier::Surface,
        name: "code-surface",
        tokens: ["markdown_code_fg", "surface_code"],
        minimum: 4.5,
        remediation: TEXT_REMEDIATION,
    },
    ThemeRule {
        kind: ThemeDiagnosticKind::ContrastPair,
        tier: ThemeDiagnosticTier::Surface,
        name: "modal-body",
        tokens: ["text_primary", "modal_bg"],
        minimum: 4.5,
        remediation: TEXT_REMEDIATION,
    },
    ThemeRule {
        kind: ThemeDiagnosticKind::ContrastPair,
        tier: ThemeDiagnosticTier::Surface,
        name: "modal-selected",
        tokens: ["text_primary", "surface_selection"],
        minimum: 3.0,
        remediation: TEXT_REMEDIATION,
    },
    ThemeRule {
        kind: ThemeDiagnosticKind::ContrastPair,
        tier: ThemeDiagnosticTier::Surface,
        name: "config-modal-selected",
        tokens: ["text_primary", "config_selected_bg"],
        minimum: 3.0,
        remediation: TEXT_REMEDIATION,
    },
    ThemeRule {
        kind: ThemeDiagnosticKind::ContrastPair,
        tier: ThemeDiagnosticTier::Surface,
        name: "setup-body",
        tokens: ["config_primary", "setup_bg"],
        minimum: 4.5,
        remediation: TEXT_REMEDIATION,
    },
    ThemeRule {
        kind: ThemeDiagnosticKind::ContrastPair,
        tier: ThemeDiagnosticTier::Surface,
        name: "overlay-body",
        tokens: ["text_primary", "overlay_bg"],
        minimum: 4.5,
        remediation: TEXT_REMEDIATION,
    },
    ThemeRule {
        kind: ThemeDiagnosticKind::ContrastPair,
        tier: ThemeDiagnosticTier::Surface,
        name: "config-preview-allow",
        tokens: ["button_selected_fg", "approval_allow_bg"],
        minimum: 3.0,
        remediation: TEXT_REMEDIATION,
    },
    ThemeRule {
        kind: ThemeDiagnosticKind::ContrastPair,
        tier: ThemeDiagnosticTier::Surface,
        name: "config-preview-deny",
        tokens: ["button_selected_fg", "approval_deny_bg"],
        minimum: 3.0,
        remediation: TEXT_REMEDIATION,
    },
    ThemeRule {
        kind: ThemeDiagnosticKind::ContrastPair,
        tier: ThemeDiagnosticTier::Surface,
        name: "config-preview-selected",
        tokens: ["button_selected_fg", "approval_selected_bg"],
        minimum: 3.0,
        remediation: TEXT_REMEDIATION,
    },
];

const SEMANTIC_RULES: &[ThemeRule] = &[
    ThemeRule {
        kind: ThemeDiagnosticKind::SemanticSeparation,
        tier: ThemeDiagnosticTier::Semantic,
        name: "status-success-warning",
        tokens: ["status_success", "status_warning"],
        minimum: 0.22,
        remediation: SEMANTIC_REMEDIATION,
    },
    ThemeRule {
        kind: ThemeDiagnosticKind::SemanticSeparation,
        tier: ThemeDiagnosticTier::Semantic,
        name: "status-warning-error",
        tokens: ["status_warning", "status_error"],
        minimum: 0.22,
        remediation: SEMANTIC_REMEDIATION,
    },
    ThemeRule {
        kind: ThemeDiagnosticKind::SemanticSeparation,
        tier: ThemeDiagnosticTier::Semantic,
        name: "status-error-pending",
        tokens: ["status_error", "status_pending"],
        minimum: 0.18,
        remediation: SEMANTIC_REMEDIATION,
    },
    ThemeRule {
        kind: ThemeDiagnosticKind::SemanticSeparation,
        tier: ThemeDiagnosticTier::Semantic,
        name: "risk-low-medium",
        tokens: ["risk_low", "risk_medium"],
        minimum: 0.18,
        remediation: SEMANTIC_REMEDIATION,
    },
    ThemeRule {
        kind: ThemeDiagnosticKind::SemanticSeparation,
        tier: ThemeDiagnosticTier::Semantic,
        name: "risk-medium-high",
        tokens: ["risk_medium", "risk_high"],
        minimum: 0.22,
        remediation: SEMANTIC_REMEDIATION,
    },
    ThemeRule {
        kind: ThemeDiagnosticKind::SemanticSeparation,
        tier: ThemeDiagnosticTier::Safety,
        name: "approval-allow-deny",
        tokens: ["approval_allow_bg", "approval_deny_bg"],
        minimum: 0.22,
        remediation: SAFETY_REMEDIATION,
    },
    ThemeRule {
        kind: ThemeDiagnosticKind::SemanticSeparation,
        tier: ThemeDiagnosticTier::Safety,
        name: "diff-added-removed-bg",
        tokens: ["diff_added_bg", "diff_removed_bg"],
        minimum: 0.18,
        remediation: SAFETY_REMEDIATION,
    },
    ThemeRule {
        kind: ThemeDiagnosticKind::SemanticSeparation,
        tier: ThemeDiagnosticTier::Semantic,
        name: "diff-added-removed-fg",
        tokens: ["diff_added_fg", "diff_removed_fg"],
        minimum: 0.18,
        remediation: SEMANTIC_REMEDIATION,
    },
];

const STRUCTURAL_RULES: &[ThemeRule] = &[
    ThemeRule {
        kind: ThemeDiagnosticKind::StructuralCue,
        tier: ThemeDiagnosticTier::Structural,
        name: "border-subtle-panel",
        tokens: ["border_subtle", "surface_panel"],
        minimum: 1.5,
        remediation: STRUCTURAL_REMEDIATION,
    },
    ThemeRule {
        kind: ThemeDiagnosticKind::StructuralCue,
        tier: ThemeDiagnosticTier::Structural,
        name: "border-strong-base",
        tokens: ["border_strong", "surface_base"],
        minimum: 3.0,
        remediation: STRUCTURAL_REMEDIATION,
    },
    ThemeRule {
        kind: ThemeDiagnosticKind::StructuralCue,
        tier: ThemeDiagnosticTier::Structural,
        name: "border-focus-base",
        tokens: ["border_focus", "surface_base"],
        minimum: 3.0,
        remediation: STRUCTURAL_REMEDIATION,
    },
    ThemeRule {
        kind: ThemeDiagnosticKind::StructuralCue,
        tier: ThemeDiagnosticTier::Safety,
        name: "border-danger-base",
        tokens: ["border_danger", "surface_base"],
        minimum: 3.0,
        remediation: SAFETY_REMEDIATION,
    },
    ThemeRule {
        kind: ThemeDiagnosticKind::StructuralCue,
        tier: ThemeDiagnosticTier::Structural,
        name: "markdown-rule-base",
        tokens: ["markdown_rule", "surface_base"],
        minimum: 1.5,
        remediation: STRUCTURAL_REMEDIATION,
    },
    ThemeRule {
        kind: ThemeDiagnosticKind::StructuralCue,
        tier: ThemeDiagnosticTier::Structural,
        name: "quote-bar-base",
        tokens: ["markdown_quote_bar", "surface_base"],
        minimum: 1.5,
        remediation: STRUCTURAL_REMEDIATION,
    },
    ThemeRule {
        kind: ThemeDiagnosticKind::StructuralCue,
        tier: ThemeDiagnosticTier::Structural,
        name: "modal-border",
        tokens: ["modal_border", "modal_bg"],
        minimum: 2.0,
        remediation: STRUCTURAL_REMEDIATION,
    },
    ThemeRule {
        kind: ThemeDiagnosticKind::StructuralCue,
        tier: ThemeDiagnosticTier::Structural,
        name: "config-border",
        tokens: ["config_border", "config_bg"],
        minimum: 2.0,
        remediation: STRUCTURAL_REMEDIATION,
    },
    ThemeRule {
        kind: ThemeDiagnosticKind::StructuralCue,
        tier: ThemeDiagnosticTier::Structural,
        name: "approval-border",
        tokens: ["approval_border", "approval_bg"],
        minimum: 2.0,
        remediation: STRUCTURAL_REMEDIATION,
    },
    ThemeRule {
        kind: ThemeDiagnosticKind::StructuralCue,
        tier: ThemeDiagnosticTier::Safety,
        name: "approval-danger-border",
        tokens: ["approval_border", "approval_deny_bg"],
        minimum: 3.0,
        remediation: SAFETY_REMEDIATION,
    },
];
