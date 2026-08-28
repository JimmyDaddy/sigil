use crate::{CoreError, MAX_SURFACE_TEXT_BYTES, NodeId, NodeKey, Rect, Surface};

/// Application-neutral standard widget categories.
#[non_exhaustive]
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum WidgetKind {
    Box,
    Text,
    Stack,
    Scroll,
    VirtualList,
    Input,
    Button,
    Select,
    Modal,
    Popover,
    Status,
    Card,
    Markdown,
}

/// A bounded widget declaration. It contains only framework identity and display data.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WidgetSpec {
    key: NodeKey,
    kind: WidgetKind,
    bounds: Rect,
    label: String,
    binding: Option<NodeKey>,
}

impl WidgetSpec {
    pub fn new(
        key: NodeKey,
        kind: WidgetKind,
        bounds: Rect,
        label: impl Into<String>,
    ) -> Result<Self, CoreError> {
        let label = label.into();
        if label.len() > MAX_SURFACE_TEXT_BYTES {
            return Err(CoreError::InvalidValue("widget label budget exceeded"));
        }
        Ok(Self {
            key,
            kind,
            bounds,
            label,
            binding: None,
        })
    }

    pub fn action(mut self, binding: NodeKey) -> Self {
        self.binding = Some(binding);
        self
    }

    pub fn key(&self) -> &NodeKey {
        &self.key
    }

    pub fn kind(&self) -> WidgetKind {
        self.kind
    }

    pub fn bounds(&self) -> Rect {
        self.bounds
    }

    pub fn label(&self) -> &str {
        &self.label
    }

    pub fn binding(&self) -> Option<&NodeKey> {
        self.binding.as_ref()
    }
}

/// Bounded builder that lowers standard widget declarations into a framework surface.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WidgetTree {
    surface: Surface,
}

impl WidgetTree {
    pub fn new(viewport: Rect, generation: u64) -> Result<Self, CoreError> {
        Ok(Self {
            surface: Surface::new(viewport, generation)?,
        })
    }

    pub fn push(&mut self, widget: WidgetSpec) -> Result<NodeId, CoreError> {
        match widget.binding {
            Some(binding) => {
                self.surface
                    .push_action(widget.key, widget.bounds, widget.label, binding)
            }
            None => self
                .surface
                .push_text(widget.key, widget.bounds, widget.label),
        }
    }

    pub fn surface(&self) -> &Surface {
        &self.surface
    }

    pub fn into_surface(self) -> Surface {
        self.surface
    }
}

#[cfg(test)]
#[path = "tests/widgets_tests.rs"]
mod tests;
