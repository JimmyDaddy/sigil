use anyhow::{Context, Result};
use serde::Deserialize;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OpenAiCompatibleCatalogModel {
    pub id: String,
}

#[derive(Deserialize)]
struct ModelList {
    data: Vec<ModelObject>,
}

#[derive(Deserialize)]
struct ModelObject {
    id: String,
}

pub fn parse_openai_compatible_model_list(
    bytes: &[u8],
) -> Result<Vec<OpenAiCompatibleCatalogModel>> {
    let payload: ModelList =
        serde_json::from_slice(bytes).context("invalid OpenAI-compatible model-list response")?;
    anyhow::ensure!(
        payload.data.len() <= 2_000,
        "OpenAI-compatible model list is too large"
    );
    payload
        .data
        .into_iter()
        .map(|model| {
            anyhow::ensure!(
                !model.id.trim().is_empty() && model.id.len() <= 256,
                "invalid OpenAI-compatible model id"
            );
            Ok(OpenAiCompatibleCatalogModel { id: model.id })
        })
        .collect()
}
