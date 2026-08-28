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

pub const MAX_SURFACE_NODES: usize = 4_096;
pub const MAX_SURFACE_TEXT_BYTES: usize = 64 * 1024;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SurfaceNodeKind {
    Text(String),
    Action { label: String, binding: NodeKey },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SurfaceNode {
    pub id: NodeId,
    pub key: NodeKey,
    pub bounds: Rect,
    pub kind: SurfaceNodeKind,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Surface {
    viewport: Rect,
    generation: u64,
    nodes: Vec<SurfaceNode>,
}

impl Surface {
    pub fn new(viewport: Rect, generation: u64) -> Result<Self, CoreError> {
        if generation == 0 {
            return Err(CoreError::InvalidValue(
                "surface generation must be non-zero",
            ));
        }
        Ok(Self {
            viewport,
            generation,
            nodes: Vec::new(),
        })
    }

    pub fn viewport(&self) -> Rect {
        self.viewport
    }

    pub fn generation(&self) -> u64 {
        self.generation
    }

    pub fn nodes(&self) -> &[SurfaceNode] {
        &self.nodes
    }

    pub fn push_text(
        &mut self,
        key: NodeKey,
        bounds: Rect,
        value: impl Into<String>,
    ) -> Result<NodeId, CoreError> {
        let value = bounded_text(value.into())?;
        self.push_node(key, bounds, SurfaceNodeKind::Text(value))
    }

    pub fn push_action(
        &mut self,
        key: NodeKey,
        bounds: Rect,
        label: impl Into<String>,
        binding: NodeKey,
    ) -> Result<NodeId, CoreError> {
        let label = bounded_text(label.into())?;
        self.push_node(key, bounds, SurfaceNodeKind::Action { label, binding })
    }

    pub fn hit_test(&self, x: u16, y: u16) -> Option<&SurfaceNode> {
        self.nodes
            .iter()
            .rev()
            .find(|node| node.bounds.contains(x, y))
    }

    pub fn committed_presentation(
        &self,
        terminal_epoch: u64,
    ) -> Result<CommittedPresentation, CoreError> {
        let hits = self
            .nodes
            .iter()
            .filter_map(|node| match &node.kind {
                SurfaceNodeKind::Action { binding, .. } => {
                    Some(HitTarget::new(node.id, node.bounds, binding.as_str()))
                }
                SurfaceNodeKind::Text(_) => None,
            })
            .collect::<Result<Vec<_>, _>>()?;
        CommittedPresentation::new(
            terminal_epoch,
            self.viewport,
            self.generation,
            format!("surface:{}:{}", self.generation, self.nodes.len()),
            hits,
        )
    }

    fn push_node(
        &mut self,
        key: NodeKey,
        bounds: Rect,
        kind: SurfaceNodeKind,
    ) -> Result<NodeId, CoreError> {
        if self.nodes.len() >= MAX_SURFACE_NODES {
            return Err(CoreError::InvalidValue("surface node budget exceeded"));
        }
        if self.nodes.iter().any(|node| node.key == key) {
            return Err(CoreError::InvalidValue("surface node key is duplicated"));
        }
        let slot = u32::try_from(self.nodes.len())
            .map_err(|_| CoreError::InvalidValue("surface node slot overflow"))?;
        let id = NodeId::new(self.generation, slot);
        self.nodes.push(SurfaceNode {
            id,
            key,
            bounds,
            kind,
        });
        Ok(id)
    }
}

fn bounded_text(value: String) -> Result<String, CoreError> {
    if value.len() > MAX_SURFACE_TEXT_BYTES {
        return Err(CoreError::InvalidValue("surface text budget exceeded"));
    }
    Ok(value)
}

/// A bounded resident window over a larger logical sequence.
///
/// The owner supplies the resident items; this type never loads, pages, or performs I/O. The
/// generation-scoped IDs prevent an old viewport item from being mistaken for a new one after a
/// projection refresh.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VirtualSequence<T> {
    pub generation: u64,
    pub first_item: usize,
    pub total_items: usize,
    pub items: Vec<T>,
    pub item_ids: Vec<SurfaceItemId>,
}

impl<T> VirtualSequence<T> {
    pub fn new(generation: u64, first_item: usize, total_items: usize, items: Vec<T>) -> Self {
        let item_ids = (0..items.len())
            .map(|offset| SurfaceItemId {
                generation,
                ordinal: first_item.saturating_add(offset),
            })
            .collect();
        Self {
            generation,
            first_item,
            total_items: total_items.max(first_item.saturating_add(items.len())),
            items,
            item_ids,
        }
    }

    pub fn resident_len(&self) -> usize {
        self.items.len()
    }

    pub fn is_bounded_by(&self, max_resident_items: usize) -> bool {
        self.items.len() <= max_resident_items
            && self.total_items >= self.first_item.saturating_add(self.items.len())
            && self.item_ids.len() == self.items.len()
            && self.item_ids.iter().enumerate().all(|(offset, id)| {
                id.generation == self.generation
                    && id.ordinal == self.first_item.saturating_add(offset)
            })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct SurfaceItemId {
    pub generation: u64,
    pub ordinal: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ViewportAnchor {
    pub item_id: SurfaceItemId,
    pub intra_item_row: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ProjectionPageRequest {
    pub request_id: u64,
    pub generation: u64,
    pub first_item: usize,
    pub item_count: usize,
}

/// O(log N) prefix-sum index for variable-height virtual items.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HeightIndex {
    heights: Vec<u32>,
    tree: Vec<u64>,
}

impl HeightIndex {
    pub fn with_estimate(item_count: usize, estimated_height: u32) -> Self {
        let mut index = Self {
            heights: vec![estimated_height.max(1); item_count],
            tree: vec![0; item_count.saturating_add(1)],
        };
        for item in 0..item_count {
            index.add_tree(item, u64::from(estimated_height.max(1)));
        }
        index
    }

    pub fn len(&self) -> usize {
        self.heights.len()
    }

    pub fn is_empty(&self) -> bool {
        self.heights.is_empty()
    }

    pub fn total_height(&self) -> u64 {
        self.prefix_height(self.len())
    }

    pub fn height(&self, item: usize) -> Option<u32> {
        self.heights.get(item).copied()
    }

    pub fn set_height(&mut self, item: usize, height: u32) -> Option<u32> {
        let previous = *self.heights.get(item)?;
        let height = height.max(1);
        if previous != height {
            self.heights[item] = height;
            if height > previous {
                self.add_tree(item, u64::from(height - previous));
            } else {
                self.subtract_tree(item, u64::from(previous - height));
            }
        }
        Some(previous)
    }

    pub fn prefix_height(&self, item_count: usize) -> u64 {
        let mut index = item_count.min(self.len());
        let mut total = 0;
        while index > 0 {
            total += self.tree[index];
            index &= index - 1;
        }
        total
    }

    /// Locate the item containing a logical row offset and return its intra-item row.
    pub fn locate_row(&self, row: u64) -> Option<(usize, u32)> {
        if row >= self.total_height() || self.heights.is_empty() {
            return None;
        }
        let mut low = 0usize;
        let mut high = self.len();
        while low < high {
            let middle = low + (high - low) / 2;
            if self.prefix_height(middle.saturating_add(1)) <= row {
                low = middle.saturating_add(1);
            } else {
                high = middle;
            }
        }
        let item = low.min(self.len().saturating_sub(1));
        Some((item, (row - self.prefix_height(item)) as u32))
    }

    fn add_tree(&mut self, item: usize, value: u64) {
        let mut index = item.saturating_add(1);
        while index < self.tree.len() {
            self.tree[index] = self.tree[index].saturating_add(value);
            index += index & index.wrapping_neg();
        }
    }

    fn subtract_tree(&mut self, item: usize, value: u64) {
        let mut index = item.saturating_add(1);
        while index < self.tree.len() {
            self.tree[index] = self.tree[index].saturating_sub(value);
            index += index & index.wrapping_neg();
        }
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
