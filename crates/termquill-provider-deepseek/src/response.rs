#[derive(Debug, Clone)]
pub enum DeepSeekSseFrame {
    Data(String),
    Comment,
    Blank,
    Done,
}
