use std::collections::BTreeMap;

use super::ContextTruncation;

/// Estimates a stable, local token cost for context packing and snippet validation.
#[must_use]
pub fn estimate_context_token_cost(text: &str) -> usize {
    tokenize_context_text(text).len().max(1)
}

pub(super) fn tokenize_context_text(text: &str) -> Vec<String> {
    let mut tokens = Vec::new();
    let mut current = String::new();
    let mut cjk_run = Vec::new();
    for ch in text.chars() {
        if is_cjk_context_char(ch) {
            flush_context_word_token(&mut tokens, &mut current);
            cjk_run.push(ch);
        } else if ch.is_alphanumeric() || ch == '_' {
            flush_cjk_context_tokens(&mut tokens, &mut cjk_run);
            for lower in ch.to_lowercase() {
                current.push(lower);
            }
        } else {
            flush_context_word_token(&mut tokens, &mut current);
            flush_cjk_context_tokens(&mut tokens, &mut cjk_run);
        }
    }
    flush_context_word_token(&mut tokens, &mut current);
    flush_cjk_context_tokens(&mut tokens, &mut cjk_run);
    tokens
}

fn flush_context_word_token(tokens: &mut Vec<String>, current: &mut String) {
    if !current.is_empty() {
        tokens.push(std::mem::take(current));
    }
}

fn flush_cjk_context_tokens(tokens: &mut Vec<String>, cjk_run: &mut Vec<char>) {
    match cjk_run.len() {
        0 => {}
        1 => tokens.push(cjk_run[0].to_string()),
        _ => {
            for pair in cjk_run.windows(2) {
                tokens.push(pair.iter().collect());
            }
        }
    }
    cjk_run.clear();
}

fn is_cjk_context_char(ch: char) -> bool {
    matches!(
        ch,
        '\u{3400}'..='\u{4dbf}'
            | '\u{4e00}'..='\u{9fff}'
            | '\u{f900}'..='\u{faff}'
            | '\u{3040}'..='\u{30ff}'
            | '\u{ac00}'..='\u{d7af}'
            | '\u{20000}'..='\u{2a6df}'
            | '\u{2a700}'..='\u{2b73f}'
            | '\u{2b740}'..='\u{2b81f}'
            | '\u{2b820}'..='\u{2ceaf}'
    )
}

pub(super) fn term_counts(tokens: &[String]) -> BTreeMap<String, usize> {
    let mut counts = BTreeMap::new();
    for token in tokens {
        *counts.entry(token.clone()).or_insert(0) += 1;
    }
    counts
}

pub(super) fn bm25_score(
    query_terms: &[String],
    term_counts: &BTreeMap<String, usize>,
    doc_len: usize,
    average_doc_len: f32,
    doc_count: f32,
    document_frequency: &BTreeMap<String, usize>,
) -> f32 {
    const K1: f32 = 1.2;
    const B: f32 = 0.75;

    let doc_len = doc_len.max(1) as f32;
    let average_doc_len = average_doc_len.max(1.0);
    let mut score = 0.0;
    for term in query_terms {
        let Some(term_frequency) = term_counts.get(term).copied() else {
            continue;
        };
        let document_frequency = document_frequency.get(term).copied().unwrap_or_default() as f32;
        let idf = ((doc_count - document_frequency + 0.5) / (document_frequency + 0.5) + 1.0).ln();
        let term_frequency = term_frequency as f32;
        let denominator = term_frequency + K1 * (1.0 - B + B * (doc_len / average_doc_len));
        score += idf * (term_frequency * (K1 + 1.0)) / denominator;
    }
    score
}

pub(super) fn truncate_context_body(body: &str, max_bytes: usize) -> (String, ContextTruncation) {
    if body.len() <= max_bytes {
        return (body.to_owned(), ContextTruncation::none(body.len()));
    }

    let mut end = max_bytes.min(body.len());
    while !body.is_char_boundary(end) {
        end -= 1;
    }
    let indexed_body = body[..end].to_owned();
    (
        indexed_body,
        ContextTruncation {
            original_byte_len: body.len(),
            indexed_byte_len: end,
            truncated: true,
        },
    )
}

fn context_snippet(body: &str, max_chars: usize) -> String {
    let mut snippet = String::new();
    for ch in body.chars().take(max_chars) {
        snippet.push(ch);
    }
    if body.chars().count() > max_chars {
        snippet.push_str("...");
    }
    snippet
}

pub(super) fn context_snippet_around_terms(
    body: &str,
    query_terms: &[String],
    max_chars: usize,
) -> String {
    let lower_body = body.to_lowercase();
    let mut terms = query_terms
        .iter()
        .filter(|term| !term.is_empty())
        .collect::<Vec<_>>();
    terms.sort_by_key(|term| std::cmp::Reverse(term.len()));
    let Some(byte_index) = terms
        .into_iter()
        .find_map(|term| lower_body.find(term.as_str()))
    else {
        return context_snippet(body, max_chars);
    };
    context_snippet_window(body, byte_index, max_chars)
}

fn context_snippet_window(body: &str, byte_index: usize, max_chars: usize) -> String {
    if max_chars == 0 || body.is_empty() {
        return String::new();
    }
    let total_chars = body.chars().count();
    if total_chars <= max_chars {
        return body.to_owned();
    }
    let mut byte_index = byte_index.min(body.len());
    while byte_index > 0 && !body.is_char_boundary(byte_index) {
        byte_index -= 1;
    }
    let focus_char = body[..byte_index].chars().count();
    let start_char = focus_char.saturating_sub(max_chars / 4);
    let end_char = start_char.saturating_add(max_chars).min(total_chars);
    let mut snippet = String::new();
    if start_char > 0 {
        snippet.push_str("...");
    }
    snippet.extend(body.chars().skip(start_char).take(end_char - start_char));
    if end_char < total_chars {
        snippet.push_str("...");
    }
    snippet
}
