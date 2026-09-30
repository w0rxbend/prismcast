//! [`CompositorBackend`]: scene composition control seam (PLAN.md §5, §8).
//!
//! The compositor renders the placed items of scenes onto a canvas. RES-003
//! §2 established that `compositor`, `glvideomixer`, and `vacompositor` all
//! expose the same per-input vocabulary (`xpos`/`ypos`/`width`/`height`/
//! `alpha`/`zorder`), which maps 1:1 onto [`SceneItem`] — so this trait
//! speaks in domain scene terms and the GStreamer backend can swap elements
//! underneath without changes here.
//!
//! Per-item geometry derived from [`Transform`], [`Crop`], [`Bounds`], and
//! [`BlendMode`] is computed by the backend implementation (or the
//! `prismcast-compositor` crate above it), not by the domain.

use prismcast_core::{
    CanvasId, Result, Scene, SceneId, SceneItem, SceneItemId, Transition, VideoConfig,
};

use crate::component::BackendComponent;

/// Controls the scene compositor.
///
/// The compositor is a singleton-per-engine component rather than a per-entity
/// instance; it still reports [`crate::ComponentState`] and
/// [`crate::BackendEvent`]s (e.g. GPU device lost, PLAN.md §61) through the
/// [`BackendComponent`] supertrait.
///
/// Threading and failure semantics match [`crate::SourceBackend`].
pub trait CompositorBackend: BackendComponent {
    /// (Re)configures a canvas render target. Changing resolution or frame
    /// rate may force downstream renegotiation; backends report that via
    /// [`crate::BackendEvent::RenegotiationRequired`].
    fn configure_canvas(&mut self, canvas_id: CanvasId, video: VideoConfig) -> Result<()>;

    /// Replaces the full item set of a scene (scene switch, collection load).
    ///
    /// Incremental edits should prefer [`CompositorBackend::upsert_item`] and
    /// [`CompositorBackend::remove_item`] to avoid graph churn.
    fn sync_scene(&mut self, scene: &Scene) -> Result<()>;

    /// Inserts or updates one placed item (transform, crop, opacity,
    /// visibility, blend mode, bounds, z-index).
    fn upsert_item(&mut self, scene_id: SceneId, item: &SceneItem) -> Result<()>;

    /// Removes one item from a scene.
    fn remove_item(&mut self, scene_id: SceneId, item_id: SceneItemId) -> Result<()>;

    /// Selects the scene mixed onto the program output. In studio mode this
    /// is the preview→program handoff target; see
    /// [`CompositorBackend::start_transition`].
    fn set_program_scene(&mut self, scene_id: SceneId) -> Result<()>;

    /// Runs a scene transition onto the program output (PLAN.md §17).
    ///
    /// [`prismcast_core::TransitionKind::Cut`] must be accepted and behave as
    /// an immediate switch; backends that cannot render an effect natively
    /// degrade to `Cut` and report a [`crate::BackendEvent::Warning`].
    fn start_transition(&mut self, transition: &Transition) -> Result<()>;
}
