#![forbid(unsafe_code)]

use std::fmt;

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct NodeKey(String);

impl NodeKey {
    pub fn new(value: impl Into<String>) -> Result<Self, CoreError> {
        let value = value.into();
        if value.is_empty() || value.len() > 256 {
            return Err(CoreError::InvalidValue("node key must be bounded"));
        }
        Ok(Self(value))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct NodeId {
    pub generation: u64,
    pub slot: u32,
}

impl NodeId {
    pub const fn new(generation: u64, slot: u32) -> Self {
        Self { generation, slot }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Rect {
    pub x: u16,
    pub y: u16,
    pub width: u16,
    pub height: u16,
}

impl Rect {
    pub const fn new(x: u16, y: u16, width: u16, height: u16) -> Self {
        Self {
            x,
            y,
            width,
            height,
        }
    }

    pub fn contains(self, x: u16, y: u16) -> bool {
        x >= self.x
            && y >= self.y
            && x < self.x.saturating_add(self.width)
            && y < self.y.saturating_add(self.height)
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Damage(u8);

impl Damage {
    pub const NONE: Self = Self(0);
    pub const PAINT: Self = Self(1);
    pub const INTERACTION: Self = Self(2);
    pub const TERMINAL: Self = Self(4);
    pub const FULL: Self = Self(8);

    pub const fn union(self, other: Self) -> Self {
        Self(self.0 | other.0)
    }

    pub const fn is_empty(self) -> bool {
        self.0 == 0
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum InputEvent {
    Key { code: String },
    Pointer { x: u16, y: u16 },
    Resize { width: u16, height: u16 },
    Paste(String),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HitTarget {
    pub node: NodeId,
    pub bounds: Rect,
    binding: String,
}

impl HitTarget {
    pub fn new(node: NodeId, bounds: Rect, binding: impl Into<String>) -> Result<Self, CoreError> {
        let binding = binding.into();
        if binding.is_empty() || binding.len() > 256 {
            return Err(CoreError::InvalidValue("hit binding must be bounded"));
        }
        Ok(Self {
            node,
            bounds,
            binding,
        })
    }

    pub fn binding(&self) -> &str {
        &self.binding
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CommittedPresentation {
    pub terminal_epoch: u64,
    pub viewport: Rect,
    pub generation: u64,
    pub cell_digest: String,
    hits: Vec<HitTarget>,
}

impl CommittedPresentation {
    pub fn new(
        terminal_epoch: u64,
        viewport: Rect,
        generation: u64,
        cell_digest: impl Into<String>,
        hits: Vec<HitTarget>,
    ) -> Result<Self, CoreError> {
        if terminal_epoch == 0 || generation == 0 {
            return Err(CoreError::InvalidValue(
                "presentation generations are non-zero",
            ));
        }
        Ok(Self {
            terminal_epoch,
            viewport,
            generation,
            cell_digest: cell_digest.into(),
            hits,
        })
    }

    pub fn hit_test(&self, x: u16, y: u16) -> Option<&HitTarget> {
        self.hits
            .iter()
            .rev()
            .find(|target| target.bounds.contains(x, y))
    }

    pub fn hits(&self) -> &[HitTarget] {
        &self.hits
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PresentOutcome {
    Presented(TrustedPresentReceipt),
    NotStarted(PresentNotStarted),
    IndeterminateAfterIo(PresentFault),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TrustedPresentReceipt {
    pub terminal_epoch: u64,
    pub generation: u64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PresentNotStarted {
    pub reason: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PresentFault {
    pub terminal_epoch: u64,
    pub reason: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CoreError {
    InvalidValue(&'static str),
}

impl fmt::Display for CoreError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidValue(reason) => formatter.write_str(reason),
        }
    }
}

impl std::error::Error for CoreError {}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ScrollAnchor {
    pub item_index: u64,
    pub cell_offset: u16,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TerminalCapabilities {
    pub alternate_screen: bool,
    pub mouse: bool,
    pub focus_events: bool,
}
