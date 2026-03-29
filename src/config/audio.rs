//! Audio capture and feedback configuration.

use serde::{Deserialize, Serialize};

use super::default_true;

/// Audio capture configuration
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct AudioConfig {
    /// PipeWire/PulseAudio device name, or "default"
    #[serde(default = "default_audio_device")]
    pub device: String,

    /// Sample rate in Hz (whisper expects 16000)
    #[serde(default = "default_audio_sample_rate")]
    pub sample_rate: u32,

    /// Maximum recording duration in seconds (safety limit)
    #[serde(default = "default_audio_max_duration_secs")]
    pub max_duration_secs: u32,

    /// Pause MPRIS media players during recording and resume on stop
    #[serde(default)]
    pub pause_media: bool,

    /// MPRIS player bus-name suffixes to skip when pausing. Matched against
    /// the part after `org.mpris.MediaPlayer2.` either exactly or as a
    /// `<entry>.<instance>` prefix (e.g. `"chromium"` matches
    /// `chromium.instance123`). Useful for ignoring browsers whose MPRIS
    /// status is unreliable, or background players you never want paused.
    #[serde(default)]
    pub pause_media_ignored_players: Vec<String>,

    /// Lower active media stream volume during recording and restore on stop
    #[serde(default)]
    pub duck_media: bool,

    /// Target volume percentage for media ducking
    #[serde(default = "default_duck_media_volume_percent")]
    pub duck_media_volume_percent: u8,

    /// Audio feedback settings
    #[serde(default)]
    pub feedback: AudioFeedbackConfig,

    /// Dead-input guard settings
    #[serde(default)]
    pub dead_input: DeadInputConfig,
}

impl Default for AudioConfig {
    fn default() -> Self {
        Self {
            device: default_audio_device(),
            sample_rate: default_audio_sample_rate(),
            max_duration_secs: default_audio_max_duration_secs(),
            pause_media: false,
            pause_media_ignored_players: Vec::new(),
            duck_media: false,
            duck_media_volume_percent: default_duck_media_volume_percent(),
            feedback: AudioFeedbackConfig::default(),
            dead_input: DeadInputConfig::default(),
        }
    }
}

fn default_audio_device() -> String {
    "default".to_string()
}

fn default_audio_sample_rate() -> u32 {
    16000
}

fn default_audio_max_duration_secs() -> u32 {
    60
}

fn default_duck_media_volume_percent() -> u8 {
    70
}

/// Dead-input guard: stop a recording whose capture delivers no sound.
///
/// A microphone that is off, muted in hardware, or asleep still opens and
/// still streams; the samples are digital silence or a bare DC offset.
/// Without this guard the recording runs to its end and transcribes to
/// nothing, with no sign to the user that the dictation was lost.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct DeadInputConfig {
    /// Stop a recording when its input carries no sound
    #[serde(default = "default_true")]
    pub enabled: bool,

    /// How long to listen for sound before giving up, in milliseconds
    #[serde(default = "default_dead_input_window_ms")]
    pub window_ms: u32,

    /// Level a 100 ms frame must reach, in dBFS, for the input to count as
    /// alive. Measured after a 150 Hz high-pass, so a DC offset adds
    /// nothing to it.
    #[serde(default = "default_dead_input_threshold_dbfs")]
    pub threshold_dbfs: f32,
}

fn default_dead_input_window_ms() -> u32 {
    1500
}

fn default_dead_input_threshold_dbfs() -> f32 {
    -100.0
}

impl Default for DeadInputConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            window_ms: default_dead_input_window_ms(),
            threshold_dbfs: default_dead_input_threshold_dbfs(),
        }
    }
}

/// Audio feedback configuration for sound cues
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct AudioFeedbackConfig {
    /// Enable audio feedback sounds
    #[serde(default)]
    pub enabled: bool,

    /// Sound theme: "default", "subtle", "mechanical", or path to custom theme directory
    #[serde(default = "default_sound_theme")]
    pub theme: String,

    /// Volume level (0.0 to 1.0)
    #[serde(default = "default_volume")]
    pub volume: f32,
}

fn default_sound_theme() -> String {
    "default".to_string()
}

fn default_volume() -> f32 {
    0.7
}

impl Default for AudioFeedbackConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            theme: default_sound_theme(),
            volume: default_volume(),
        }
    }
}
