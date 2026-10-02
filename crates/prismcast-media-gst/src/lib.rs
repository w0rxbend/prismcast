//! Native GStreamer platform. Call initialization from the media owner thread.
//! Registry capabilities are observations, not guarantees of usable hardware.
use gstreamer::{self as gst, prelude::*};

/// Platform setup failures, distinct from domain and wire errors.
#[derive(Debug, thiserror::Error)]
pub enum GstError {
    #[error("GStreamer initialization failed: {0}")]
    Initialization(#[source] gst::glib::Error),
    #[error("required GStreamer element factory is unavailable: {factory}")]
    MissingFactory { factory: String },
}

/// Registered plugin metadata. A registry entry need not have been loaded yet.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PluginCapability {
    pub name: String,
    pub version: String,
}

/// Snapshot of the native registry after successful initialization.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Capabilities {
    pub version: (u32, u32, u32, u32),
    pub plugins: Vec<PluginCapability>,
    pub element_factories: Vec<String>,
}

/// Successful setup token plus observed capabilities; owns no global singleton.
#[derive(Debug)]
pub struct GstRuntime {
    capabilities: Capabilities,
}

impl GstRuntime {
    /// Initialize the process-wide native library and snapshot its registry.
    /// GStreamer initialization is repeatable; dropping this token does not deinit it.
    pub fn initialize() -> Result<Self, GstError> {
        gst::init().map_err(GstError::Initialization)?;
        let registry = gst::Registry::get();
        let mut plugins: Vec<_> = registry
            .plugins()
            .iter()
            .map(|plugin| PluginCapability {
                name: plugin.plugin_name().to_string(),
                version: plugin.version().to_string(),
            })
            .collect();
        plugins.sort_by(|a, b| a.name.cmp(&b.name));
        let mut element_factories: Vec<_> = registry
            .features(gst::ElementFactory::static_type())
            .iter()
            .map(|feature| feature.name().to_string())
            .collect();
        element_factories.sort();
        element_factories.dedup();
        let capabilities = Capabilities {
            version: gst::version(),
            plugins,
            element_factories,
        };
        tracing::info!(version = ?capabilities.version, plugins = capabilities.plugins.len(),
            factories = capabilities.element_factories.len(), "GStreamer platform initialized");
        Ok(Self { capabilities })
    }

    pub fn capabilities(&self) -> &Capabilities {
        &self.capabilities
    }

    /// Resolve current registry state (the inventory is a startup snapshot).
    pub fn require_factory(&self, name: &str) -> Result<gst::ElementFactory, GstError> {
        gst::ElementFactory::find(name).ok_or_else(|| GstError::MissingFactory {
            factory: name.to_owned(),
        })
    }
}

pub mod test_pattern;
pub use test_pattern::{
    build_test_pattern_bin, GstTestPatternSource, Pattern, TestPatternSettings,
};
pub mod compositor;
pub use compositor::GstCompositor;
pub mod audio;
pub use audio::GstAudioMixer;
