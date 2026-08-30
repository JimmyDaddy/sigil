//! Application-neutral bounded text layout helpers.

use wezterm_bidi::{BidiContext, ParagraphDirectionHint};

use crate::{CoreError, MAX_SURFACE_TEXT_BYTES};

/// A bounded UAX #9 result with both directions of the logical/visual index mapping.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BidiText {
    logical: String,
    visual: String,
    map: BidiLineMap,
}

impl BidiText {
    pub fn new(text: impl Into<String>) -> Result<Self, CoreError> {
        let logical = text.into();
        if logical.len() > MAX_SURFACE_TEXT_BYTES {
            return Err(CoreError::InvalidValue("bidi text budget exceeded"));
        }
        let chars = logical.chars().collect::<Vec<_>>();
        let visual_order = if chars.len() < 2 {
            (0..chars.len()).collect::<Vec<_>>()
        } else {
            let mut context = BidiContext::new();
            context.resolve_paragraph(&chars, ParagraphDirectionHint::AutoLeftToRight);
            let (_, visual_order) = context.reorder_line(0..chars.len());
            complete_visual_order(chars.len(), visual_order)?
        };
        let mut logical_to_visual = vec![0; chars.len()];
        for (visual, logical) in visual_order.iter().copied().enumerate() {
            let Some(mapped) = logical_to_visual.get_mut(logical) else {
                return Err(CoreError::InvalidValue(
                    "bidi mapping index is out of bounds",
                ));
            };
            *mapped = visual;
        }
        let visual = visual_order
            .iter()
            .map(|index| chars[*index])
            .collect::<String>();
        Ok(Self {
            logical,
            visual,
            map: BidiLineMap {
                visual_to_logical: visual_order,
                logical_to_visual,
            },
        })
    }

    pub fn logical(&self) -> &str {
        &self.logical
    }

    pub fn visual(&self) -> &str {
        &self.visual
    }

    pub fn map(&self) -> &BidiLineMap {
        &self.map
    }
}

fn complete_visual_order(
    character_count: usize,
    mut visual_order: Vec<usize>,
) -> Result<Vec<usize>, CoreError> {
    let mut seen = vec![false; character_count];
    for &logical in &visual_order {
        let Some(mapped) = seen.get_mut(logical) else {
            return Err(CoreError::InvalidValue(
                "bidi mapping index is out of bounds",
            ));
        };
        if *mapped {
            return Err(CoreError::InvalidValue("bidi mapping index is duplicated"));
        }
        *mapped = true;
    }

    // wezterm-bidi intentionally omits Rule X9 formatting controls from its reordered result.
    // Keep those zero-width codepoints in the public mapping so a logical string remains a
    // complete, round-trippable value (notably for emoji ZWJ sequences).
    for (logical, was_seen) in seen.iter().copied().enumerate() {
        if !was_seen {
            let insertion = visual_order
                .iter()
                .position(|&candidate| candidate > logical)
                .unwrap_or(visual_order.len());
            visual_order.insert(insertion, logical);
        }
    }
    Ok(visual_order)
}

/// The bijection between logical character offsets and visual character offsets.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BidiLineMap {
    visual_to_logical: Vec<usize>,
    logical_to_visual: Vec<usize>,
}

impl BidiLineMap {
    pub fn visual_to_logical(&self) -> &[usize] {
        &self.visual_to_logical
    }

    pub fn logical_to_visual(&self) -> &[usize] {
        &self.logical_to_visual
    }

    pub fn logical_index_for_visual(&self, visual: usize) -> Option<usize> {
        self.visual_to_logical.get(visual).copied()
    }

    pub fn visual_index_for_logical(&self, logical: usize) -> Option<usize> {
        self.logical_to_visual.get(logical).copied()
    }
}

#[cfg(test)]
#[path = "tests/text_tests.rs"]
mod tests;
