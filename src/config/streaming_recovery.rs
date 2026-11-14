//! Recovery for a streaming session that ends without delivering text.

use serde::{Deserialize, Serialize};

use super::default_true;

/// What the daemon does when a streaming backend fails or stalls.
///
/// A streaming backend holds the only copy of the transcript until it emits
/// it. When the socket dies mid-recording, or the backend never answers the
/// stop, the dictation is gone even though the capture still holds every
/// sample. With this on, those samples are transcribed with the batch
/// backend and delivered through the ordinary output path instead.
#[derive(Debug, Clone, Copy, Deserialize, Serialize)]
pub struct StreamingRecoveryConfig {
    /// Recover a streaming session that produced no text.
    #[serde(default = "default_true")]
    pub enabled: bool,

    /// How long after a stop the daemon waits for the backend before it
    /// tears the session down itself, in seconds. Set this above the
    /// backend's own drain timeout so it only fires on a stuck session.
    #[serde(default = "default_stop_timeout_secs")]
    pub stop_timeout_secs: u64,
}

fn default_stop_timeout_secs() -> u64 {
    20
}

impl Default for StreamingRecoveryConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            stop_timeout_secs: default_stop_timeout_secs(),
        }
    }
}

impl StreamingRecoveryConfig {
    /// The stop bound, clamped to something a person could have meant.
    pub fn stop_timeout(&self) -> std::time::Duration {
        std::time::Duration::from_secs(self.stop_timeout_secs.clamp(1, 120))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recovery_is_on_by_default() {
        let config = StreamingRecoveryConfig::default();
        assert!(config.enabled);
        assert_eq!(config.stop_timeout_secs, 20);
    }

    #[test]
    fn an_absent_table_keeps_the_defaults() {
        let config: crate::config::Config = toml::from_str(
            r#"
            engine = "whisper"
            [audio]
            device = "default"
            [whisper]
            model = "base.en"
            "#,
        )
        .unwrap();
        assert!(config.streaming_recovery.enabled);
        assert_eq!(config.streaming_recovery.stop_timeout_secs, 20);
    }

    #[test]
    fn the_stop_bound_is_clamped() {
        let config = StreamingRecoveryConfig {
            enabled: true,
            stop_timeout_secs: 0,
        };
        assert_eq!(config.stop_timeout().as_secs(), 1);
        let config = StreamingRecoveryConfig {
            enabled: true,
            stop_timeout_secs: 10_000,
        };
        assert_eq!(config.stop_timeout().as_secs(), 120);
    }
}
