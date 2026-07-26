use anyhow::{Context, Result};
use serde::Deserialize;

pub const BUNDLED_ANTHROPIC_MODELS: [(&str, &str, bool); 2] = [
    ("claude-sonnet-4-5", "Claude Sonnet 4.5", true),
    ("claude-opus-4-1", "Claude Opus 4.1", false),
];

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AnthropicCatalogModel {
    pub id: String,
    pub display_name: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AnthropicCatalogPage {
    pub models: Vec<AnthropicCatalogModel>,
    pub next_after_id: Option<String>,
}

#[derive(Deserialize)]
struct ModelList {
    data: Vec<ModelObject>,
    has_more: bool,
    last_id: Option<String>,
}

#[derive(Deserialize)]
struct ModelObject {
    id: String,
    display_name: String,
}

pub fn parse_anthropic_model_list(bytes: &[u8]) -> Result<AnthropicCatalogPage> {
    let payload: ModelList =
        serde_json::from_slice(bytes).context("invalid Anthropic model-list response")?;
    anyhow::ensure!(
        payload.data.len() <= 1_000,
        "Anthropic model page is too large"
    );
    let models = payload
        .data
        .into_iter()
        .map(|model| {
            anyhow::ensure!(
                !model.id.trim().is_empty() && model.id.len() <= 256,
                "invalid Anthropic model id"
            );
            anyhow::ensure!(
                !model.display_name.trim().is_empty() && model.display_name.len() <= 256,
                "invalid Anthropic model display name"
            );
            Ok(AnthropicCatalogModel {
                id: model.id,
                display_name: model.display_name,
            })
        })
        .collect::<Result<Vec<_>>>()?;
    let next_after_id = if payload.has_more {
        Some(
            payload
                .last_id
                .filter(|cursor| !cursor.is_empty() && cursor.len() <= 256)
                .ok_or_else(|| anyhow::anyhow!("Anthropic pagination cursor is missing"))?,
        )
    } else {
        None
    };
    Ok(AnthropicCatalogPage {
        models,
        next_after_id,
    })
}
