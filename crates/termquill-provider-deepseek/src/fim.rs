#[derive(Debug, Clone)]
pub struct DeepSeekFimCompletionRequest {
    pub model: Option<String>,
    pub prompt: String,
    pub suffix: String,
    pub max_tokens: Option<u32>,
    pub stop: Vec<String>,
}
