use sigil_kernel::ReasoningEffort;

#[derive(Debug, Clone)]
pub struct DeepSeekPrefixCompletionRequest {
    pub model: Option<String>,
    pub prompt: String,
    pub assistant_prefix: String,
    pub stop: Vec<String>,
    pub reasoning_effort: Option<ReasoningEffort>,
    pub traffic_partition_key: Option<String>,
}
