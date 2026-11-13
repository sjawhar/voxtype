//! File output: write the transcription to a file instead of the keyboard.
//!
//! This is the output end of `[output] mode = "file"` and of
//! `voxtype record start --file[=PATH]`. The batch transcription path
//! writes its one finished transcript itself; this type is what lets every
//! other path — streaming sessions above all, which deliver a transcript in
//! pieces — honour the same request without typing anything into whatever
//! window happens to have focus.
//!
//! Within one session the first write obeys `file_mode` and every later
//! write appends, so a streaming session's segments accumulate into one
//! transcript rather than each one replacing the last.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};

use tokio::io::AsyncWriteExt;

use crate::config::FileMode;
use crate::error::OutputError;

use super::TextOutput;

/// Writes text to a file. One instance per recording session.
pub struct FileOutput {
    path: PathBuf,
    mode: FileMode,
    /// Set once the session's first write has happened.
    written: AtomicBool,
}

impl FileOutput {
    pub fn new(path: PathBuf, mode: FileMode) -> Self {
        Self {
            path,
            mode,
            written: AtomicBool::new(false),
        }
    }

    /// Path this output writes to.
    pub fn path(&self) -> &Path {
        &self.path
    }
}

#[async_trait::async_trait]
impl TextOutput for FileOutput {
    async fn output(&self, text: &str) -> Result<(), OutputError> {
        if let Some(parent) = self.path.parent() {
            if !parent.as_os_str().is_empty() {
                tokio::fs::create_dir_all(parent)
                    .await
                    .map_err(|e| OutputError::InjectionFailed(e.to_string()))?;
            }
        }

        let first_write = !self.written.swap(true, Ordering::SeqCst);
        let truncate = first_write && matches!(self.mode, FileMode::Overwrite);

        let text = if text.ends_with('\n') {
            text.to_string()
        } else {
            format!("{}\n", text)
        };

        let mut file = tokio::fs::OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(truncate)
            .append(!truncate)
            .open(&self.path)
            .await
            .map_err(|e| OutputError::InjectionFailed(format!("{}: {}", self.path.display(), e)))?;
        file.write_all(text.as_bytes())
            .await
            .map_err(|e| OutputError::InjectionFailed(format!("{}: {}", self.path.display(), e)))?;
        // tokio's File buffers; without this the bytes of a segment can still
        // be in memory when the next segment reopens the file and writes past
        // them.
        file.flush()
            .await
            .map_err(|e| OutputError::InjectionFailed(format!("{}: {}", self.path.display(), e)))?;

        tracing::info!("Wrote transcription to {:?}", self.path);
        Ok(())
    }

    async fn is_available(&self) -> bool {
        true
    }

    fn name(&self) -> &'static str {
        "file"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_path(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "voxtype-file-output-{}-{}",
            std::process::id(),
            name
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir.join("out.txt")
    }

    #[tokio::test]
    async fn writes_the_transcript_with_a_trailing_newline() {
        let path = temp_path("write");
        let out = FileOutput::new(path.clone(), FileMode::Overwrite);
        out.output("hello there").await.unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "hello there\n");
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[tokio::test]
    async fn segments_of_one_session_accumulate() {
        let path = temp_path("segments");
        let out = FileOutput::new(path.clone(), FileMode::Overwrite);
        out.output("first").await.unwrap();
        out.output("second").await.unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "first\nsecond\n");
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[tokio::test]
    async fn overwrite_mode_replaces_an_earlier_session() {
        let path = temp_path("overwrite");
        std::fs::write(&path, "stale\n").unwrap();
        let out = FileOutput::new(path.clone(), FileMode::Overwrite);
        out.output("fresh").await.unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "fresh\n");
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[tokio::test]
    async fn append_mode_keeps_an_earlier_session() {
        let path = temp_path("append");
        std::fs::write(&path, "earlier\n").unwrap();
        let out = FileOutput::new(path.clone(), FileMode::Append);
        out.output("later").await.unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "earlier\nlater\n");
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[tokio::test]
    async fn missing_parent_directories_are_created() {
        let path = temp_path("nested").parent().unwrap().join("a/b/out.txt");
        let out = FileOutput::new(path.clone(), FileMode::Overwrite);
        out.output("deep").await.unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "deep\n");
        let _ = std::fs::remove_dir_all(path.parent().unwrap().parent().unwrap().parent().unwrap());
    }
}
