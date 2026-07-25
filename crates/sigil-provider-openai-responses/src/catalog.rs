use anyhow::{Context, Result};
use serde::Deserialize;

pub const BUNDLED_OPENAI_RESPONSES_MODELS: [(&str, &str, bool); 3] = [
    ("gpt-5", "GPT-5", true),
    ("gpt-5-mini", "GPT-5 mini", false),
    ("gpt-4.1", "GPT-4.1", false),
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OpenAiModelAdmission {
    KnownGeneration,
    UnverifiedGeneration,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OpenAiCatalogModel {
    pub id: String,
    pub admission: OpenAiModelAdmission,
}

#[derive(Deserialize)]
struct ModelList {
    data: Vec<ModelObject>,
}

#[derive(Deserialize)]
struct ModelObject {
    id: String,
    object: String,
}

pub fn parse_openai_responses_model_list(bytes: &[u8]) -> Result<Vec<OpenAiCatalogModel>> {
    let payload: ModelList =
        serde_json::from_slice(bytes).context("invalid OpenAI model-list response")?;
    anyhow::ensure!(
        payload.data.len() <= 2_000,
        "OpenAI model list is too large"
    );
    let mut models = Vec::new();
    for model in payload.data {
        anyhow::ensure!(model.object == "model", "invalid OpenAI model object");
        anyhow::ensure!(
            !model.id.trim().is_empty() && model.id.len() <= 256,
            "invalid OpenAI model id"
        );
        if is_known_non_generation_model(&model.id) {
            continue;
        }
        let admission = if is_known_generation_model(&model.id) {
            OpenAiModelAdmission::KnownGeneration
        } else {
            OpenAiModelAdmission::UnverifiedGeneration
        };
        models.push(OpenAiCatalogModel {
            id: model.id,
            admission,
        });
    }
    Ok(models)
}

fn is_known_generation_model(id: &str) -> bool {
    ["gpt-", "o1", "o3", "o4", "codex-", "computer-use-"]
        .iter()
        .any(|prefix| id.starts_with(prefix))
}

fn is_known_non_generation_model(id: &str) -> bool {
    [
        "text-embedding-",
        "text-moderation-",
        "omni-moderation-",
        "whisper-",
        "tts-",
        "dall-e-",
        "gpt-image-",
        "sora-",
    ]
    .iter()
    .any(|prefix| id.starts_with(prefix))
}
