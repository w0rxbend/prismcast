//! Scenes and scene items: the compositing domain (PLAN.md §8).
//!
//! A [`Scene`] is an ordered container of [`SceneItem`]s; each item places a
//! shared [`crate::source::Source`] with its own transform, crop, opacity,
//! visibility, lock, blend mode, bounds, and z-index. Items are kept sorted
//! ascending by `z_index`, with insertion order as tiebreak (stable ordering).

use serde::{Deserialize, Serialize};

use crate::capture::SourceDimensions;
use crate::id::{CanvasId, ProfileId, SceneId, SceneItemId, SourceId};
use crate::project::VideoConfig;

/// A scene: an ordered set of placed sources.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Scene {
    /// Unique scene ID.
    pub id: SceneId,
    /// User-facing name.
    pub name: String,
    /// Placed sources, sorted ascending by `z_index` (ties keep insertion order).
    pub items: Vec<SceneItem>,
}

impl Scene {
    /// Creates an empty scene with a fresh ID.
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            id: SceneId::new(),
            name: name.into(),
            items: Vec::new(),
        }
    }

    /// Returns the item with the given ID, if present.
    pub fn item(&self, item_id: SceneItemId) -> Option<&SceneItem> {
        self.items.iter().find(|item| item.id == item_id)
    }

    /// Returns a mutable reference to the item with the given ID, if present.
    pub fn item_mut(&mut self, item_id: SceneItemId) -> Option<&mut SceneItem> {
        self.items.iter_mut().find(|item| item.id == item_id)
    }

    /// Inserts an item and re-sorts by `z_index`.
    pub fn add_item(&mut self, item: SceneItem) {
        self.items.push(item);
        self.sort_items();
    }

    /// Removes and returns the item with the given ID, if present.
    pub fn remove_item(&mut self, item_id: SceneItemId) -> Option<SceneItem> {
        let pos = self.items.iter().position(|item| item.id == item_id)?;
        Some(self.items.remove(pos))
    }

    /// Sets an item's z-index and re-sorts.
    pub fn set_z_index(&mut self, item_id: SceneItemId, z_index: i32) -> bool {
        match self.item_mut(item_id) {
            Some(item) => {
                item.z_index = z_index;
                self.sort_items();
                true
            }
            None => false,
        }
    }

    /// Moves an item one step up in z-order (swaps with the next item above).
    ///
    /// Returns the new `(own z_index, displaced item ID)` pair so callers can
    /// build undo inverses, or `None` if the item is missing or already on top.
    pub fn raise_item(&mut self, item_id: SceneItemId) -> Option<(i32, SceneItemId)> {
        let pos = self.items.iter().position(|item| item.id == item_id)?;
        if pos + 1 >= self.items.len() {
            return None;
        }
        let above_id = self.items[pos + 1].id;
        let own_z = self.items[pos].z_index;
        let above_z = self.items[pos + 1].z_index;
        let new_z = if own_z == above_z {
            // Tie: bump strictly above; the neighbor keeps its z-index.
            above_z + 1
        } else {
            // Distinct z-indices: swap them so the order actually changes
            // (a stable sort would otherwise leave a tie in place).
            self.items[pos + 1].z_index = own_z;
            above_z
        };
        self.items[pos].z_index = new_z;
        self.sort_items();
        Some((new_z, above_id))
    }

    /// Moves an item one step down in z-order (swaps with the next item below).
    ///
    /// Returns the new `(own z_index, displaced item ID)` pair so callers can
    /// build undo inverses, or `None` if the item is missing or already at the
    /// bottom.
    pub fn lower_item(&mut self, item_id: SceneItemId) -> Option<(i32, SceneItemId)> {
        let pos = self.items.iter().position(|item| item.id == item_id)?;
        if pos == 0 {
            return None;
        }
        let below_id = self.items[pos - 1].id;
        let own_z = self.items[pos].z_index;
        let below_z = self.items[pos - 1].z_index;
        let new_z = if own_z == below_z {
            // Tie: drop strictly below; the neighbor keeps its z-index.
            below_z - 1
        } else {
            self.items[pos - 1].z_index = own_z;
            below_z
        };
        self.items[pos].z_index = new_z;
        self.sort_items();
        Some((new_z, below_id))
    }

    fn sort_items(&mut self) {
        self.items.sort_by_key(|item| item.z_index);
    }
}

/// A placed source within a scene (PLAN.md §8).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SceneItem {
    /// Unique item ID.
    pub id: SceneItemId,
    /// The shared source this item renders.
    pub source_id: SourceId,
    /// Position/scale/rotation/anchor.
    pub transform: Transform,
    /// Edge crop in pixels.
    pub crop: Crop,
    /// Opacity in `[0.0, 1.0]`.
    pub opacity: f32,
    /// Whether the item is rendered.
    pub visible: bool,
    /// Whether the item is protected from edits.
    pub locked: bool,
    /// Compositing blend mode.
    pub blend_mode: BlendMode,
    /// Bounds-based fitting (alternative to free scaling).
    pub bounds: Bounds,
    /// Stacking order; items render ascending (higher = on top).
    pub z_index: i32,
}

impl SceneItem {
    /// Creates a visible, unlocked item at the origin with a fresh ID.
    pub fn new(source_id: SourceId, z_index: i32) -> Self {
        Self {
            id: SceneItemId::new(),
            source_id,
            transform: Transform::default(),
            crop: Crop::default(),
            opacity: 1.0,
            visible: true,
            locked: false,
            blend_mode: BlendMode::default(),
            bounds: Bounds::default(),
            z_index,
        }
    }
}

/// 2D position/scale/rotation of a scene item.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Transform {
    /// Position of the anchor point in canvas pixels.
    pub position: Vec2,
    /// Scale factor per axis (`1.0` = natural size).
    pub scale: Vec2,
    /// Rotation in degrees, clockwise.
    pub rotation: f32,
    /// The point of the item that `position` refers to.
    pub anchor: Anchor,
}

impl Default for Transform {
    fn default() -> Self {
        Self {
            position: Vec2::default(),
            scale: Vec2 { x: 1.0, y: 1.0 },
            rotation: 0.0,
            anchor: Anchor::default(),
        }
    }
}

/// A 2D vector in canvas pixels (or scale factors, by context).
#[derive(Debug, Clone, Copy, Default, PartialEq, Serialize, Deserialize)]
pub struct Vec2 {
    /// Horizontal component.
    pub x: f32,
    /// Vertical component.
    pub y: f32,
}

impl Vec2 {
    /// Constructs a vector from components.
    pub fn new(x: f32, y: f32) -> Self {
        Self { x, y }
    }
}

/// Per-edge crop in source pixels.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Crop {
    /// Pixels cropped from the left edge.
    pub left: u32,
    /// Pixels cropped from the top edge.
    pub top: u32,
    /// Pixels cropped from the right edge.
    pub right: u32,
    /// Pixels cropped from the bottom edge.
    pub bottom: u32,
}

/// The reference point within an item that its position applies to.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Anchor {
    /// Top-left corner (OBS default).
    #[default]
    TopLeft,
    /// Top edge midpoint.
    Top,
    /// Top-right corner.
    TopRight,
    /// Left edge midpoint.
    Left,
    /// Item center.
    Center,
    /// Right edge midpoint.
    Right,
    /// Bottom-left corner.
    BottomLeft,
    /// Bottom edge midpoint.
    Bottom,
    /// Bottom-right corner.
    BottomRight,
}

/// Compositing blend mode (maps onto GStreamer compositor pad operators).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BlendMode {
    /// Standard alpha-over compositing.
    #[default]
    Normal,
    /// Additive blending.
    Additive,
    /// Multiply blending.
    Multiply,
    /// Screen blending.
    Screen,
}

/// Bounds-based fitting: fit/stretch an item into a rectangle instead of free
/// scaling. Disabled when `kind` is [`BoundsKind::None`].
#[derive(Debug, Clone, Copy, Default, PartialEq, Serialize, Deserialize)]
pub struct Bounds {
    /// Fitting mode.
    pub kind: BoundsKind,
    /// Target rectangle size in canvas pixels.
    pub size: Vec2,
    /// Alignment within the rectangle.
    pub alignment: Anchor,
}

/// How an item fits into its [`Bounds`] rectangle.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BoundsKind {
    /// No bounds fitting; free transform applies.
    #[default]
    None,
    /// Stretch to fill the rectangle, ignoring aspect ratio.
    Stretch,
    /// Scale to fit inside the rectangle, preserving aspect ratio.
    FitInner,
    /// Scale to fill the rectangle, preserving aspect ratio (may overflow).
    FitOuter,
}

/// Bounded typed basis a conditional scene placement edit was computed from
/// (ADR-0026). Compared field-by-field against authoritative state; any
/// mismatch rejects the edit as `Error::Conflict` without changing state.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct PlacementExpectation {
    /// Scene the controller was previewing when the edit was computed.
    pub current_scene: SceneId,
    /// Active profile of the editing context.
    pub active_profile: ProfileId,
    /// Canvas video configuration of the editing context.
    pub video: VideoConfig,
    /// Item transform preimage the geometry was derived from.
    pub transform: Transform,
    /// Item crop preimage (rendered-size basis).
    pub crop: Crop,
    /// Item bounds preimage (placement interpretation basis).
    pub bounds: Bounds,
    /// Item lock preimage (editability basis).
    pub locked: bool,
    /// Negotiated native source pixels; `None` expects no active dimensions.
    /// Runtime-only: checked by the application actor, not by `AppState`.
    pub source_dimensions: Option<SourceDimensions>,
}

/// A canvas: a render target with its own resolution.
///
/// Stub reserved per the RES-002 open question so per-canvas resolution can be
/// introduced later without a persisted-schema break. Only the main canvas
/// exists for now; collection-level video settings come from the active
/// profile (PLAN.md §19).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Canvas {
    /// Unique canvas ID.
    pub id: CanvasId,
    /// Width in pixels.
    pub width: u32,
    /// Height in pixels.
    pub height: u32,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn item_at(z: i32) -> SceneItem {
        SceneItem::new(SourceId::new(), z)
    }

    #[test]
    fn scene_serde_roundtrip() {
        let mut scene = Scene::new("Main");
        scene.add_item(item_at(2));
        scene.add_item(item_at(0));
        let json = serde_json::to_string(&scene).unwrap();
        let back: Scene = serde_json::from_str(&json).unwrap();
        assert_eq!(scene, back);
    }

    #[test]
    fn add_item_keeps_z_order_sorted() {
        let mut scene = Scene::new("s");
        let high = item_at(10);
        let low = item_at(-5);
        let mid = item_at(3);
        scene.add_item(high.clone());
        scene.add_item(low.clone());
        scene.add_item(mid.clone());
        let ids: Vec<_> = scene.items.iter().map(|i| i.id).collect();
        assert_eq!(ids, vec![low.id, mid.id, high.id]);
    }

    #[test]
    fn equal_z_keeps_insertion_order() {
        let mut scene = Scene::new("s");
        let a = item_at(0);
        let b = item_at(0);
        scene.add_item(a.clone());
        scene.add_item(b.clone());
        assert_eq!(scene.items[0].id, a.id);
        assert_eq!(scene.items[1].id, b.id);
    }

    #[test]
    fn remove_item_returns_it() {
        let mut scene = Scene::new("s");
        let item = item_at(0);
        scene.add_item(item.clone());
        assert_eq!(scene.remove_item(item.id), Some(item));
        assert!(scene.items.is_empty());
        assert_eq!(scene.remove_item(SceneItemId::new()), None);
    }

    #[test]
    fn set_z_index_reorders() {
        let mut scene = Scene::new("s");
        let a = item_at(0);
        let b = item_at(1);
        scene.add_item(a.clone());
        scene.add_item(b.clone());
        assert!(scene.set_z_index(a.id, 5));
        assert_eq!(scene.items[1].id, a.id);
        assert!(!scene.set_z_index(SceneItemId::new(), 0));
    }

    #[test]
    fn raise_and_lower_swap_neighbors() {
        let mut scene = Scene::new("s");
        let a = item_at(0);
        let b = item_at(1);
        scene.add_item(a.clone());
        scene.add_item(b.clone());

        let (new_z, displaced) = scene.raise_item(a.id).unwrap();
        assert_eq!(new_z, 1);
        assert_eq!(displaced, b.id);
        assert_eq!(scene.items[1].id, a.id);

        // Already on top: no-op.
        assert!(scene.raise_item(a.id).is_none());

        let (new_z, _) = scene.lower_item(a.id).unwrap();
        assert_eq!(new_z, 0);
        assert_eq!(scene.items[0].id, a.id);

        // Already at the bottom: no-op.
        assert!(scene.lower_item(a.id).is_none());
    }

    #[test]
    fn raise_with_tied_z_bumps_strictly_above() {
        let mut scene = Scene::new("s");
        let a = item_at(0);
        let b = item_at(0);
        scene.add_item(a.clone());
        scene.add_item(b.clone());
        let (new_z, displaced) = scene.raise_item(a.id).unwrap();
        assert_eq!(new_z, 1);
        assert_eq!(displaced, b.id);
        assert_eq!(scene.items[1].id, a.id);
        assert_eq!(scene.items[0].z_index, 0);
    }

    #[test]
    fn placement_expectation_serde_roundtrip() {
        let expectation = PlacementExpectation {
            current_scene: SceneId::new(),
            active_profile: ProfileId::new(),
            video: VideoConfig::default(),
            transform: Transform {
                position: Vec2::new(10.5, -3.0),
                scale: Vec2::new(2.0, 0.5),
                rotation: 45.0,
                anchor: Anchor::Center,
            },
            crop: Crop {
                left: 1,
                top: 2,
                right: 3,
                bottom: 4,
            },
            bounds: Bounds {
                kind: BoundsKind::FitInner,
                size: Vec2::new(1920.0, 1080.0),
                alignment: Anchor::TopLeft,
            },
            locked: false,
            source_dimensions: Some(SourceDimensions {
                width: 1920,
                height: 1080,
            }),
        };
        let json = serde_json::to_string(&expectation).unwrap();
        assert_eq!(expectation, serde_json::from_str(&json).unwrap());

        let without_dimensions = PlacementExpectation {
            source_dimensions: None,
            ..expectation
        };
        let json = serde_json::to_string(&without_dimensions).unwrap();
        assert_eq!(without_dimensions, serde_json::from_str(&json).unwrap());
    }

    #[test]
    fn small_types_serde_roundtrip() {
        let transform = Transform {
            position: Vec2::new(10.5, -3.0),
            scale: Vec2::new(2.0, 0.5),
            rotation: 45.0,
            anchor: Anchor::Center,
        };
        let json = serde_json::to_string(&transform).unwrap();
        assert_eq!(transform, serde_json::from_str(&json).unwrap());

        let crop = Crop {
            left: 1,
            top: 2,
            right: 3,
            bottom: 4,
        };
        let json = serde_json::to_string(&crop).unwrap();
        assert_eq!(crop, serde_json::from_str(&json).unwrap());

        let bounds = Bounds {
            kind: BoundsKind::FitInner,
            size: Vec2::new(1920.0, 1080.0),
            alignment: Anchor::TopLeft,
        };
        let json = serde_json::to_string(&bounds).unwrap();
        assert_eq!(bounds, serde_json::from_str(&json).unwrap());

        let canvas = Canvas {
            id: CanvasId::new(),
            width: 1920,
            height: 1080,
        };
        let json = serde_json::to_string(&canvas).unwrap();
        assert_eq!(canvas, serde_json::from_str(&json).unwrap());

        for mode in [
            BlendMode::Normal,
            BlendMode::Additive,
            BlendMode::Multiply,
            BlendMode::Screen,
        ] {
            let json = serde_json::to_string(&mode).unwrap();
            assert_eq!(mode, serde_json::from_str(&json).unwrap());
        }
    }
}
