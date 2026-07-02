use super::*;

pub(super) async fn preview_line(path: &Path, line: u64) -> Option<String> {
    let content = tokio::fs::read_to_string(path).await.ok()?;
    content
        .lines()
        .nth(line.saturating_sub(1) as usize)
        .map(|line| line.chars().take(200).collect())
}
