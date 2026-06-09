#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeepSeekEndpointClass {
    Primary,
    Beta,
    #[allow(dead_code)]
    AnthropicCompat,
}
