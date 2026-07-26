use anyhow::{Context, Result};
use serde::Deserialize;

pub const BUNDLED_DEEPSEEK_MODELS: [(&str, &str, bool); 2] = [
    ("deepseek-v4-flash", "DeepSeek V4 Flash", true),
    ("deepseek-v4-pro", "DeepSeek V4 Pro", false),
];

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeepSeekCatalogModel {
    pub id: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ModelList {
    object: String,
    data: Vec<ModelObject>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ModelObject {
    id: String,
    object: String,
    owned_by: String,
}

pub fn parse_deepseek_model_list(bytes: &[u8]) -> Result<Vec<DeepSeekCatalogModel>> {
    let payload: ModelList =
        serde_json::from_slice(bytes).context("invalid DeepSeek model-list response")?;
    anyhow::ensure!(payload.object == "list", "invalid DeepSeek list object");
    anyhow::ensure!(
        payload.data.len() <= 2_000,
        "DeepSeek model list is too large"
    );
    payload
        .data
        .into_iter()
        .map(|model| {
            anyhow::ensure!(
                model.object == "model" && model.owned_by == "deepseek",
                "invalid DeepSeek model object"
            );
            anyhow::ensure!(
                !model.id.trim().is_empty() && model.id.len() <= 256,
                "invalid DeepSeek model id"
            );
            Ok(DeepSeekCatalogModel { id: model.id })
        })
        .collect()
}
