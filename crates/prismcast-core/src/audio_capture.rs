//! Framework-free advisory PipeWire target settings (ADR-0024).
//!
//! These settings name a requested target; they never constitute a capture
//! grant. Native object identities are resolved only after explicit commands.

use serde::{Deserialize, Serialize};

use crate::{Error, Result, Source, SourceKind};

/// Current per-source PipeWire audio settings schema.
pub const PIPEWIRE_AUDIO_SETTINGS_VERSION: u32 = 1;
/// Maximum UTF-8 bytes in an advisory node name.
pub const MAX_PIPEWIRE_AUDIO_TARGET_BYTES: usize = 1024;

/// Which direction/class the selected target must support.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PipeWireAudioMode {
    /// Microphone or another capture node.
    Input,
    /// Monitor of a selected playback sink.
    Output,
    /// One selected application playback stream.
    Application,
}

/// Persisted advisory selection, separate from transient native authorization.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PipeWireAudioSettings {
    /// Explicit settings schema; unsupported versions fail before capture.
    pub schema_version: u32,
    /// Exact advisory `node.name`, resolved without default-target fallback.
    pub target: String,
    /// Required node direction/class.
    pub mode: PipeWireAudioMode,
}

impl PipeWireAudioSettings {
    /// Parse strict versioned settings without discarding unknown fields.
    pub fn from_json(value: serde_json::Value) -> Result<Self> {
        let settings: Self = serde_json::from_value(value).map_err(|error| {
            Error::InvalidInput(format!("invalid PipeWire audio settings: {error}"))
        })?;
        settings.validate()?;
        Ok(settings)
    }

    /// Parse and validate the source kind and requested mode together.
    pub fn from_source(source: &Source) -> Result<Self> {
        let settings = Self::from_json(source.settings.clone())?;
        settings.validate_for_kind(source.kind)?;
        Ok(settings)
    }

    /// Validate a settings value constructed directly by a local caller.
    pub fn validate(&self) -> Result<()> {
        if self.schema_version != PIPEWIRE_AUDIO_SETTINGS_VERSION {
            return Err(Error::InvalidInput(
                "unsupported PipeWire audio settings schema".into(),
            ));
        }
        if self.target.trim().is_empty()
            || self.target.len() > MAX_PIPEWIRE_AUDIO_TARGET_BYTES
            || self.target.chars().any(char::is_control)
        {
            return Err(Error::InvalidInput(
                "invalid PipeWire audio target name".into(),
            ));
        }
        Ok(())
    }

    /// Keep the existing wire source kinds while distinguishing sink monitors.
    pub fn validate_for_kind(&self, kind: SourceKind) -> Result<()> {
        self.validate()?;
        if !matches!(
            (kind, self.mode),
            (
                SourceKind::PipeWireAudioInput,
                PipeWireAudioMode::Input | PipeWireAudioMode::Output
            ) | (SourceKind::PipeWireAppAudio, PipeWireAudioMode::Application)
        ) {
            return Err(Error::InvalidInput(
                "PipeWire audio source kind/mode mismatch".into(),
            ));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn explicit_schema_target_and_kind_mode_are_validated() {
        for (kind, mode) in [
            (SourceKind::PipeWireAudioInput, "input"),
            (SourceKind::PipeWireAudioInput, "output"),
            (SourceKind::PipeWireAppAudio, "application"),
        ] {
            let mut source = Source::new(kind, "Audio");
            source.settings = json!({"schema_version":1,"target":"chosen.node","mode":mode});
            let settings = PipeWireAudioSettings::from_source(&source).unwrap();
            assert_eq!(serde_json::to_value(&settings).unwrap(), source.settings);
        }
        for value in [
            json!({}),
            json!({"schema_version":2,"target":"node","mode":"input"}),
            json!({"schema_version":1,"target":"","mode":"input"}),
            json!({"schema_version":1,"target":"   ","mode":"input"}),
            json!({"schema_version":1,"target":"bad\nnode","mode":"input"}),
            json!({"schema_version":1,"target":"x".repeat(1025),"mode":"input"}),
            json!({"schema_version":1,"target":"node","mode":"default"}),
            json!({"schema_version":1,"target":"node","mode":"input","object_serial":42}),
        ] {
            assert!(PipeWireAudioSettings::from_json(value).is_err());
        }
        let settings = PipeWireAudioSettings::from_json(
            json!({"schema_version":1,"target":"node","mode":"input"}),
        )
        .unwrap();
        assert!(settings
            .validate_for_kind(SourceKind::PipeWireAppAudio)
            .is_err());
        assert!(settings.validate_for_kind(SourceKind::Color).is_err());
        let settings = PipeWireAudioSettings::from_json(
            json!({"schema_version":1,"target":"node","mode":"application"}),
        )
        .unwrap();
        assert!(settings
            .validate_for_kind(SourceKind::PipeWireAudioInput)
            .is_err());
    }
}
