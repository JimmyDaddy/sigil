#![forbid(unsafe_code)]

pub mod text;
pub mod theme;
pub mod widgets;

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
pub const MAX_INPUT_TEXT_BYTES: usize = 64 * 1024;
/// Maximum cells reserved by a committed presentation's dense hit grid.
///
/// A `u32` target index keeps the grid below 400 KiB while covering the largest reference
/// qualification viewport (400x120). Oversized viewports fail at commit time rather than
/// silently falling back to a linear interaction path.
pub const MAX_HIT_GRID_CELLS: usize = 48_000;

#[non_exhaustive]
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SurfaceNodeKind {
    Text(String),
    Action { label: String, binding: NodeKey },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SurfaceNode {
    id: NodeId,
    key: NodeKey,
    bounds: Rect,
    kind: SurfaceNodeKind,
}

impl SurfaceNode {
    pub fn id(&self) -> NodeId {
        self.id
    }

    pub fn key(&self) -> &NodeKey {
        &self.key
    }

    pub fn bounds(&self) -> Rect {
        self.bounds
    }

    pub fn kind(&self) -> &SurfaceNodeKind {
        &self.kind
    }
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
    generation: u64,
    first_item: usize,
    total_items: usize,
    items: Vec<T>,
    item_ids: Vec<SurfaceItemId>,
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

    pub fn generation(&self) -> u64 {
        self.generation
    }

    pub fn first_item(&self) -> usize {
        self.first_item
    }

    pub fn total_items(&self) -> usize {
        self.total_items
    }

    pub fn items(&self) -> &[T] {
        &self.items
    }

    pub fn item_ids(&self) -> &[SurfaceItemId] {
        &self.item_ids
    }

    pub fn validate(&self) -> Result<(), CoreError> {
        if self.generation == 0 {
            return Err(CoreError::InvalidValue(
                "virtual sequence generation must be non-zero",
            ));
        }
        if !self.is_bounded_by(MAX_SURFACE_NODES) {
            return Err(CoreError::InvalidValue(
                "virtual sequence metadata is invalid",
            ));
        }
        Ok(())
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
        // Fenwick-tree binary lifting finds the first prefix whose sum exceeds `row` in O(log N).
        // Calling `prefix_height` from a binary search would make this hot path O(log² N).
        let mut bit = 1usize;
        while bit.saturating_mul(2) <= self.len() {
            bit = bit.saturating_mul(2);
        }
        let mut prefix_items = 0usize;
        let mut prefix_height = 0u64;
        while bit > 0 {
            let candidate = prefix_items.saturating_add(bit);
            if candidate <= self.len() && prefix_height.saturating_add(self.tree[candidate]) <= row
            {
                prefix_items = candidate;
                prefix_height = prefix_height.saturating_add(self.tree[candidate]);
            }
            bit >>= 1;
        }
        Some((prefix_items, (row - prefix_height) as u32))
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

impl InputEvent {
    /// Validates the bounded, normalized input representation before a host dispatches it.
    pub fn validate(&self) -> Result<(), CoreError> {
        match self {
            Self::Key { code } if code.is_empty() || code.len() > 256 => {
                Err(CoreError::InvalidValue("input key code must be bounded"))
            }
            Self::Paste(value) if value.len() > MAX_INPUT_TEXT_BYTES => {
                Err(CoreError::InvalidValue("input paste exceeds its bound"))
            }
            _ => Ok(()),
        }
    }
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
    hit_grid: Vec<u32>,
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
        let cell_count = usize::from(viewport.width)
            .checked_mul(usize::from(viewport.height))
            .ok_or(CoreError::InvalidValue(
                "presentation viewport is too large",
            ))?;
        if cell_count > MAX_HIT_GRID_CELLS {
            return Err(CoreError::InvalidValue(
                "presentation viewport exceeds hit grid bound",
            ));
        }
        let mut hit_grid = vec![u32::MAX; cell_count];
        for (target_index, target) in hits.iter().enumerate() {
            let target_index = u32::try_from(target_index)
                .map_err(|_| CoreError::InvalidValue("hit target index overflow"))?;
            let viewport_x_end = u32::from(viewport.x) + u32::from(viewport.width);
            let viewport_y_end = u32::from(viewport.y) + u32::from(viewport.height);
            let target_x_end = u32::from(target.bounds.x) + u32::from(target.bounds.width);
            let target_y_end = u32::from(target.bounds.y) + u32::from(target.bounds.height);
            let x_start = u32::from(target.bounds.x.max(viewport.x));
            let y_start = u32::from(target.bounds.y.max(viewport.y));
            let x_end = target_x_end.min(viewport_x_end);
            let y_end = target_y_end.min(viewport_y_end);
            if x_start >= x_end || y_start >= y_end {
                continue;
            }
            for y in y_start..y_end {
                for x in x_start..x_end {
                    let offset = ((y - u32::from(viewport.y)) * u32::from(viewport.width)
                        + (x - u32::from(viewport.x))) as usize;
                    hit_grid[offset] = target_index;
                }
            }
        }
        Ok(Self {
            terminal_epoch,
            viewport,
            generation,
            cell_digest: cell_digest.into(),
            hits,
            hit_grid,
        })
    }

    pub fn hit_test(&self, x: u16, y: u16) -> Option<&HitTarget> {
        if !self.viewport.contains(x, y) {
            return None;
        }
        let offset = (usize::from(y - self.viewport.y) * usize::from(self.viewport.width))
            + usize::from(x - self.viewport.x);
        let target_index = *self.hit_grid.get(offset)?;
        if target_index == u32::MAX {
            return None;
        }
        self.hits.get(target_index as usize)
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

#[cfg(test)]
mod tests {
    use super::{
        CommittedPresentation, HeightIndex, HitTarget, InputEvent, MAX_HIT_GRID_CELLS,
        MAX_INPUT_TEXT_BYTES, MAX_SURFACE_NODES, NodeId, NodeKey, Rect, Surface, VirtualSequence,
    };

    #[test]
    fn surface_nodes_expose_only_validated_read_access() {
        let viewport = Rect::new(0, 0, 10, 2);
        let mut surface = Surface::new(viewport, 1).expect("surface");
        surface
            .push_action(
                NodeKey::new("save").expect("key"),
                viewport,
                "Save",
                NodeKey::new("save.command").expect("binding"),
            )
            .expect("action");
        let node = &surface.nodes()[0];
        assert_eq!(node.id().generation, 1);
        assert_eq!(node.key().as_str(), "save");
        assert_eq!(node.bounds(), viewport);
    }

    #[test]
    fn virtual_sequence_validation_rejects_unusable_generation() {
        let invalid = VirtualSequence::new(0, 0, 1, vec!["item"]);
        assert!(invalid.validate().is_err());

        let valid = VirtualSequence::new(1, 3, 4, vec!["item"]);
        assert!(valid.validate().is_ok());
        assert!(valid.is_bounded_by(MAX_SURFACE_NODES));
        assert_eq!(valid.item_ids()[0].ordinal, 3);
    }

    #[test]
    fn input_validation_rejects_unbounded_payloads() {
        assert!(
            InputEvent::Key {
                code: String::new()
            }
            .validate()
            .is_err()
        );
        assert!(
            InputEvent::Paste("x".repeat(MAX_INPUT_TEXT_BYTES + 1))
                .validate()
                .is_err()
        );
        assert!(
            InputEvent::Resize {
                width: 80,
                height: 24,
            }
            .validate()
            .is_ok()
        );
    }

    #[test]
    fn committed_presentation_uses_the_topmost_target_from_one_dense_grid() {
        let viewport = Rect::new(10, 4, 20, 4);
        let lower = HitTarget::new(NodeId::new(1, 1), viewport, "lower").expect("target");
        let upper =
            HitTarget::new(NodeId::new(1, 2), Rect::new(12, 5, 4, 2), "upper").expect("target");
        let presentation = CommittedPresentation::new(1, viewport, 1, "digest", vec![lower, upper])
            .expect("presentation");

        assert_eq!(
            presentation.hit_test(10, 4).map(HitTarget::binding),
            Some("lower")
        );
        assert_eq!(
            presentation.hit_test(13, 5).map(HitTarget::binding),
            Some("upper")
        );
        assert!(presentation.hit_test(9, 4).is_none());
    }

    #[test]
    fn committed_presentation_rejects_an_unbounded_dense_grid() {
        let width = (MAX_HIT_GRID_CELLS as f64).sqrt() as u16 + 1;
        let viewport = Rect::new(0, 0, width, width);
        assert!(CommittedPresentation::new(1, viewport, 1, "digest", Vec::new()).is_err());
    }

    #[test]
    fn height_index_locates_variable_rows_with_fenwick_binary_lifting() {
        let mut index = HeightIndex::with_estimate(100_000, 2);
        assert_eq!(index.locate_row(0), Some((0, 0)));
        assert_eq!(index.locate_row(199_999), Some((99_999, 1)));
        assert_eq!(index.set_height(50_000, 7), Some(2));
        assert_eq!(index.locate_row(100_000), Some((50_000, 0)));
        assert!(index.locate_row(index.total_height()).is_none());
    }
}

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
