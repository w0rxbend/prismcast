//! Native GStreamer platform implementing the `prismcast-media` backend traits
//! (ADR-0004, ADR-0011).
//!
//! # Initialization
//!
//! Call [`GstRuntime::initialize`] once from the future media owner thread —
//! never from the GTK main thread or Tokio worker threads. Initialization is
//! fallible ([`GstInitError`]) and idempotent (GStreamer init is repeatable);
//! the process never calls the global deinit because other graphs may still
//! exist. The runtime refuses versions older than the ADR-0011 floor
//! ([`MINIMUM_RUNTIME_VERSION`]).
//!
//! # Capabilities
//!
//! [`GstCapabilities`] is a startup snapshot of the registry: runtime version,
//! registered plugins, the full element-factory list, and a structured
//! [`ElementInventory`] of the element families Prismcast cares about. Entries
//! are *observations*, not guarantees: presence does not promise device access,
//! codec licensing, or hardware acceleration (RES-003 §8). Graphs explicitly
//! require the factories they need via [`GstRuntime::require_factory`] and get
//! typed [`GstError::MissingFactory`] failures.
//!
//! # Threading contract
//!
//! GStreamer streaming threads are owned by each pipeline. Backend methods may
//! block on graph operations and run only on the media control actor's thread.
//! Asynchronous happenings (EOS, bus errors, warnings) are translated into
//! `prismcast_media::BackendEvent`s by a small internal bus-watch thread per
//! component, which is always joined on stop/drop. No blocking graph work ever
//! happens on GTK or Tokio threads (PLAN.md §57).

pub mod source;

use gstreamer::{self as gst, prelude::*};

pub use source::{GstSourceBackend, TestPatternKind, TestPatternSettings};

/// Minimum GStreamer runtime version accepted at initialization (ADR-0011).
pub const MINIMUM_RUNTIME_VERSION: (u32, u32, u32) = (1, 26, 0);

/// Platform initialization failures, distinct from runtime graph errors.
#[derive(Debug, thiserror::Error)]
pub enum GstInitError {
    /// `gst::init()` itself failed.
    #[error("GStreamer initialization failed: {0}")]
    Init(#[source] gst::glib::Error),
    /// The runtime is older than the ADR-0011 floor.
    #[error(
        "unsupported GStreamer runtime {found_major}.{found_minor}.{found_micro}: \
         {required_major}.{required_minor}.{required_micro} or newer is required"
    )]
    UnsupportedVersion {
        /// Found runtime version.
        found_major: u32,
        /// Found runtime version.
        found_minor: u32,
        /// Found runtime version.
        found_micro: u32,
        /// Required floor.
        required_major: u32,
        /// Required floor.
        required_minor: u32,
        /// Required floor.
        required_micro: u32,
    },
}

/// Runtime graph/registry failures, distinct from domain and wire errors.
#[derive(Debug, thiserror::Error)]
pub enum GstError {
    /// A factory the graph explicitly requires is not registered.
    #[error("required GStreamer element factory is unavailable: {factory}")]
    MissingFactory {
        /// The missing factory name.
        factory: String,
    },
}

/// Registered plugin metadata. A registry entry need not have been loaded yet.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PluginCapability {
    /// Plugin name (e.g. `coreelements`).
    pub name: String,
    /// Plugin version string.
    pub version: String,
}

/// Presence of the element families Prismcast builds graphs from, probed from
/// the registry at startup (ADR-0011: detected, never assumed).
///
/// Each list holds the names of the probed elements that are *registered* on
/// this machine, sorted. Empty means the whole family is unavailable.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ElementInventory {
    /// Video compositors (`compositor`, `glvideomixer`, `vacompositor`,
    /// `cudacompositor`) — RES-003 §2.
    pub compositors: Vec<String>,
    /// Preview sinks (`gtk4paintablesink`) — RES-003 §3.
    pub preview_sinks: Vec<String>,
    /// Capture sources (`pipewiresrc`, `pipewiresink`, `v4l2src`) — RES-003 §4.
    pub capture: Vec<String>,
    /// VA-API encoders (`vah264enc`, `vaav1enc`, ...) — RES-005.
    pub va_encoders: Vec<String>,
    /// NVIDIA NVENC encoders (`nvh264enc`, `nvav1enc`, ...) — RES-005.
    pub nv_encoders: Vec<String>,
    /// Vulkan video encoders (`vulkanh264enc`, ...) — experimental (RES-003 §5).
    pub vulkan_encoders: Vec<String>,
    /// Software encoders (`x264enc`, `svtav1enc`, ...) — RES-003 §5.
    pub software_encoders: Vec<String>,
    /// Muxers/recording plumbing (`matroskamux`, `splitmuxsink`, `isofmp4mux`,
    /// ...) — RES-003 §6.
    pub muxers: Vec<String>,
    /// Streaming sinks (`rtmp2sink`, `srtsink`, `whipclientsink`, ...) —
    /// RES-003 §7.
    pub streaming_sinks: Vec<String>,
    /// Browser-source elements (`wpe2src`, `wpesrc`) — RES-006.
    pub browser: Vec<String>,
}

impl ElementInventory {
    /// Probes the registry for the well-known element families.
    fn probe(registry: &gst::Registry) -> Self {
        let present = |candidates: &[&str]| -> Vec<String> {
            let mut found: Vec<String> = candidates
                .iter()
                .filter(|name| registry.find_feature(name, gst::ElementFactory::static_type()).is_some())
                .map(|name| (*name).to_string())
                .collect();
            found.sort();
            found
        };
        Self {
            compositors: present(&["compositor", "glvideomixer", "vacompositor", "cudacompositor"]),
            preview_sinks: present(&["gtk4paintablesink"]),
            capture: present(&["pipewiresrc", "pipewiresink", "v4l2src"]),
            va_encoders: present(&[
                "vah264enc", "vah264lpenc", "vah265enc", "vah265lpenc", "vaav1enc", "vavp8enc",
                "vavp9enc", "vajpegenc",
            ]),
            nv_encoders: present(&["nvh264enc", "nvh265enc", "nvav1enc", "nvjpegenc"]),
            vulkan_encoders: present(&["vulkanh264enc", "vulkanh265enc", "vulkanav1enc"]),
            software_encoders: present(&[
                "x264enc", "x265enc", "svtav1enc", "vp8enc", "vp9enc", "openh264enc",
            ]),
            muxers: present(&[
                "matroskamux", "mp4mux", "splitmuxsink", "isomp4mux", "isofmp4mux", "cmafmux",
                "dashmp4mux",
            ]),
            streaming_sinks: present(&[
                "rtmp2sink", "srtsink", "whipclientsink", "webrtcsink", "eflvmux",
            ]),
            browser: present(&["wpe2src", "wpesrc"]),
        }
    }

    /// Every present element across all families.
    pub fn all_present(&self) -> impl Iterator<Item = &str> {
        [
            &self.compositors,
            &self.preview_sinks,
            &self.capture,
            &self.va_encoders,
            &self.nv_encoders,
            &self.vulkan_encoders,
            &self.software_encoders,
            &self.muxers,
            &self.streaming_sinks,
            &self.browser,
        ]
        .into_iter()
        .flatten()
        .map(String::as_str)
    }
}

/// Snapshot of the native registry after successful initialization.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GstCapabilities {
    /// Runtime version `(major, minor, micro, nano)`.
    pub version: (u32, u32, u32, u32),
    /// All registered plugins, sorted by name.
    pub plugins: Vec<PluginCapability>,
    /// All registered element factory names, sorted and deduplicated.
    pub element_factories: Vec<String>,
    /// Structured presence of the element families Prismcast uses.
    pub elements: ElementInventory,
}

impl GstCapabilities {
    /// Whether an element factory with this name is registered.
    pub fn has_element(&self, name: &str) -> bool {
        self.element_factories.iter().any(|factory| factory == name)
    }
}

/// Successful setup token plus observed capabilities; owns no global singleton.
#[derive(Debug)]
pub struct GstRuntime {
    capabilities: GstCapabilities,
}

impl GstRuntime {
    /// Initialize the process-wide native library and snapshot its registry.
    ///
    /// Idempotent: GStreamer initialization is repeatable and each call returns
    /// a fresh snapshot. Dropping this token does not deinit the library.
    /// Fails with [`GstInitError::UnsupportedVersion`] below the ADR-0011 floor.
    pub fn initialize() -> Result<Self, GstInitError> {
        gst::init().map_err(GstInitError::Init)?;
        let version = gst::version();
        let (found_major, found_minor, found_micro, _) = version;
        let (required_major, required_minor, required_micro) = MINIMUM_RUNTIME_VERSION;
        if (found_major, found_minor, found_micro) < MINIMUM_RUNTIME_VERSION {
            return Err(GstInitError::UnsupportedVersion {
                found_major,
                found_minor,
                found_micro,
                required_major,
                required_minor,
                required_micro,
            });
        }
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
        let capabilities = GstCapabilities {
            version,
            plugins,
            element_factories,
            elements: ElementInventory::probe(&registry),
        };
        tracing::info!(
            version = ?capabilities.version,
            plugins = capabilities.plugins.len(),
            factories = capabilities.element_factories.len(),
            key_elements = capabilities.elements.all_present().count(),
            "GStreamer platform initialized"
        );
        Ok(Self { capabilities })
    }

    /// The startup capability snapshot.
    pub fn capabilities(&self) -> &GstCapabilities {
        &self.capabilities
    }

    /// Resolve current registry state (the inventory is a startup snapshot).
    pub fn require_factory(&self, name: &str) -> Result<gst::ElementFactory, GstError> {
        gst::ElementFactory::find(name).ok_or_else(|| GstError::MissingFactory {
            factory: name.to_owned(),
        })
    }
}
