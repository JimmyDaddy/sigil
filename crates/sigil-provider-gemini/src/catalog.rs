use anyhow::{Context, Result};
use serde::Deserialize;

pub const BUNDLED_GEMINI_MODELS: [(&str, &str, bool); 2] = [
    ("gemini-2.5-pro", "Gemini 2.5 Pro", true),
    ("gemini-2.5-flash", "Gemini 2.5 Flash", false),
];

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GeminiCatalogModel {
    pub id: String,
    pub display_name: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GeminiCatalogPage {
    pub models: Vec<GeminiCatalogModel>,
    pub next_page_token: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ModelList {
    #[serde(default)]
    models: Vec<ModelObject>,
    next_page_token: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ModelObject {
    name: String,
    display_name: String,
    #[serde(default)]
    supported_generation_methods: Vec<String>,
}

pub fn parse_gemini_model_list(bytes: &[u8]) -> Result<GeminiCatalogPage> {
    let payload: ModelList =
        serde_json::from_slice(bytes).context("invalid Gemini model-list response")?;
    anyhow::ensure!(
        payload.models.len() <= 1_000,
        "Gemini model page is too large"
    );
    let mut models = Vec::new();
    for model in payload.models {
        if !model
            .supported_generation_methods
            .iter()
            .any(|method| method == "generateContent")
        {
            continue;
        }
        anyhow::ensure!(
            model.name.starts_with("models/")
                && !model.name["models/".len()..].is_empty()
                && model.name.len() <= 256,
            "invalid Gemini model name"
        );
        anyhow::ensure!(
            !model.display_name.trim().is_empty() && model.display_name.len() <= 256,
            "invalid Gemini model display name"
        );
        models.push(GeminiCatalogModel {
            id: model.name,
            display_name: model.display_name,
        });
    }
    let next_page_token = payload
        .next_page_token
        .filter(|cursor| !cursor.is_empty())
        .map(|cursor| {
            anyhow::ensure!(cursor.len() <= 512, "Gemini pagination cursor is too large");
            Ok(cursor)
        })
        .transpose()?;
    Ok(GeminiCatalogPage {
        models,
        next_page_token,
    })
}
