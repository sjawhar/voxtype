//! ydotool-based text output
//!
//! Uses ydotool to simulate keyboard input. This works on all Wayland
//! compositors because ydotool uses the uinput kernel interface.
//!
//! Requires:
//! - ydotool installed
//! - ydotoold daemon running (systemctl --user start ydotool)
//! - User in 'input' group

use super::TextOutput;
use crate::error::OutputError;
use crate::output::find_ydotool_socket;
use std::path::PathBuf;
use std::process::Stdio;
use std::time::Duration;
use tokio::process::Command;

/// ydotool-based text output
pub struct YdotoolOutput {
    /// Delay between keypresses in milliseconds
    type_delay_ms: u32,
    /// Delay before typing starts in milliseconds
    pre_type_delay_ms: u32,
    /// Whether ydotool supports --key-hold flag (added in newer versions)
    supports_key_hold: bool,
    /// Whether to send Enter key after output
    auto_submit: bool,
    /// Text to append after transcription (before auto_submit)
    append_text: Option<String>,
    /// Path to ydotoold socket, if found at a non-default location
    socket_path: Option<PathBuf>,
    /// Convert newlines to Shift+Enter (for apps where Enter submits)
    shift_enter_newlines: bool,
}

impl YdotoolOutput {
    /// Create a new ydotool output
    ///
    /// Detects ydotool capabilities at construction time.
    pub fn new(
        type_delay_ms: u32,
        pre_type_delay_ms: u32,
        auto_submit: bool,
        append_text: Option<String>,
        shift_enter_newlines: bool,
    ) -> Self {
        let supports_key_hold = Self::detect_key_hold_support();
        if supports_key_hold {
            tracing::debug!("ydotool supports --key-hold flag");
        } else {
            tracing::debug!("ydotool does not support --key-hold flag, using --key-delay only");
        }
        let socket_path = find_ydotool_socket();
        Self {
            type_delay_ms,
            pre_type_delay_ms,
            supports_key_hold,
            auto_submit,
            append_text,
            socket_path,
            shift_enter_newlines,
        }
    }

    /// Apply the discovered socket path to a ydotool Command, if any.
    fn apply_socket_env(&self, cmd: &mut Command) {
        if let Some(ref path) = self.socket_path {
            cmd.env("YDOTOOL_SOCKET", path);
        }
    }

    /// Detect if ydotool supports the --key-hold flag
    ///
    /// Older versions of ydotool don't have this flag and silently ignore it
    /// (exiting with code 0), which can cause subtle issues.
    fn detect_key_hold_support() -> bool {
        std::process::Command::new("ydotool")
            .args(["type", "--help"])
            .output()
            .map(|output| {
                let stdout = String::from_utf8_lossy(&output.stdout);
                let stderr = String::from_utf8_lossy(&output.stderr);
                stdout.contains("--key-hold") || stderr.contains("--key-hold")
            })
            .unwrap_or(false)
    }

    /// Type a single segment of text (no newline, append, or submit handling).
    async fn type_text(&self, text: &str) -> Result<(), OutputError> {
        let mut cmd = Command::new("ydotool");
        self.apply_socket_env(&mut cmd);
        cmd.arg("type");

        // Always set delay explicitly (ydotool defaults to 12ms if not specified)
        cmd.arg("--key-delay").arg(self.type_delay_ms.to_string());

        // Use --key-hold only if supported (older versions silently ignore unknown flags)
        if self.supports_key_hold {
            cmd.arg("--key-hold").arg(self.type_delay_ms.to_string());
        }

        // The -- ensures text starting with - isn't treated as an option
        cmd.arg("--").arg(text);

        let output = cmd
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .output()
            .await
            .map_err(|e| {
                if e.kind() == std::io::ErrorKind::NotFound {
                    OutputError::YdotoolNotFound
                } else {
                    OutputError::InjectionFailed(e.to_string())
                }
            })?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);

            // Check for common errors
            if stderr.contains("socket") || stderr.contains("connect") || stderr.contains("daemon")
            {
                return Err(OutputError::YdotoolNotRunning);
            }

            return Err(OutputError::InjectionFailed(stderr.to_string()));
        }

        Ok(())
    }

    /// Send Shift+Enter key combination using ydotool.
    /// evdev keycodes: KEY_LEFTSHIFT=42, KEY_ENTER=28.
    async fn send_shift_enter(&self) -> Result<(), OutputError> {
        let mut cmd = Command::new("ydotool");
        self.apply_socket_env(&mut cmd);
        let output = cmd
            .args(["key", "42:1", "28:1", "28:0", "42:0"])
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .output()
            .await
            .map_err(|e| {
                OutputError::InjectionFailed(format!("ydotool Shift+Enter failed: {}", e))
            })?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            tracing::warn!("Failed to send Shift+Enter: {}", stderr);
        }

        Ok(())
    }

    /// Send a single Enter key using ydotool (evdev KEY_ENTER=28).
    async fn send_enter(&self) -> Result<(), OutputError> {
        let mut cmd = Command::new("ydotool");
        self.apply_socket_env(&mut cmd);
        let output = cmd
            .args(["key", "28:1", "28:0"])
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .output()
            .await
            .map_err(|e| OutputError::InjectionFailed(format!("ydotool Enter failed: {}", e)))?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            tracing::warn!("Failed to send Enter key: {}", stderr);
        }

        Ok(())
    }

    /// Output text with newlines converted to Shift+Enter (for apps where a
    /// bare Enter submits, e.g. chat clients).
    async fn output_with_shift_enter_newlines(&self, text: &str) -> Result<(), OutputError> {
        let segments: Vec<&str> = text.split('\n').collect();

        for (i, segment) in segments.iter().enumerate() {
            // Type the text segment
            if !segment.is_empty() {
                self.type_text(segment).await?;
            }

            // Send Shift+Enter between segments (not after the last one)
            if i < segments.len() - 1 {
                self.send_shift_enter().await?;
            }
        }

        Ok(())
    }
}

#[async_trait::async_trait]
impl TextOutput for YdotoolOutput {
    async fn output(&self, text: &str) -> Result<(), OutputError> {
        if text.is_empty() {
            return Ok(());
        }

        // Pre-typing delay if configured
        if self.pre_type_delay_ms > 0 {
            tracing::debug!(
                "ydotool: sleeping {}ms before typing",
                self.pre_type_delay_ms
            );
            tokio::time::sleep(Duration::from_millis(self.pre_type_delay_ms as u64)).await;
        }

        // If shift_enter_newlines is enabled, convert newlines to Shift+Enter
        if self.shift_enter_newlines && text.contains('\n') {
            self.output_with_shift_enter_newlines(text).await?;
        } else {
            self.type_text(text).await?;
        }

        // Append text if configured (e.g., a space to separate sentences)
        if let Some(ref append) = self.append_text {
            self.type_text(append).await?;
        }

        // Send Enter key if configured
        if self.auto_submit {
            self.send_enter().await?;
        }

        Ok(())
    }

    async fn is_available(&self) -> bool {
        // Check if ydotool exists in PATH
        let which_result = Command::new("which")
            .arg("ydotool")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .await;

        if !which_result.map(|s| s.success()).unwrap_or(false) {
            return false;
        }

        // Check if ydotoold is running by trying a no-op
        // ydotool type "" should succeed quickly if daemon is running
        let mut cmd = Command::new("ydotool");
        self.apply_socket_env(&mut cmd);
        cmd.args(["type", ""])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .await
            .map(|s| s.success())
            .unwrap_or(false)
    }

    fn name(&self) -> &'static str {
        "ydotool"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_new() {
        let output = YdotoolOutput::new(10, 0, false, None, false);
        assert_eq!(output.type_delay_ms, 10);
        assert_eq!(output.pre_type_delay_ms, 0);
        assert!(!output.auto_submit);
        // supports_key_hold depends on system ydotool version, so we just check it's set
        let _ = output.supports_key_hold;
    }

    #[test]
    fn test_new_with_enter() {
        let output = YdotoolOutput::new(0, 0, true, None, false);
        assert_eq!(output.type_delay_ms, 0);
        assert!(output.auto_submit);
    }

    #[test]
    fn test_new_with_pre_type_delay() {
        let output = YdotoolOutput::new(0, 200, false, None, false);
        assert_eq!(output.type_delay_ms, 0);
        assert_eq!(output.pre_type_delay_ms, 200);
    }

    #[test]
    fn test_detect_key_hold_support() {
        // This test will pass regardless of ydotool version - it just shouldn't panic
        let _supports = YdotoolOutput::detect_key_hold_support();
    }
}
