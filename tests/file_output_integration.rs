//! The output chain must be able to write a transcript to a file.
//!
//! Regression test for `voxtype record start --file=PATH` during a streaming
//! session: the daemon folds that request into an `OutputConfig` with
//! `mode = File` and builds the session's chain from it. Before the fix the
//! chain factory had no file sink at all — it warned and fell back to the
//! clipboard — so a streaming transcript went to the cursor instead of the
//! file the user named.

use std::path::{Path, PathBuf};
use voxtype::config::{FileMode, OutputConfig, OutputMode};
use voxtype::output::create_output_chain;

fn temp_file(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("voxtype-chain-{}-{}", std::process::id(), name));
    std::fs::create_dir_all(&dir).unwrap();
    dir.join("transcript.txt")
}

fn file_config(path: &Path, mode: FileMode) -> OutputConfig {
    OutputConfig {
        mode: OutputMode::File,
        file_path: Some(path.to_path_buf()),
        file_mode: mode,
        ..OutputConfig::default()
    }
}

#[test]
fn file_mode_builds_a_file_chain_not_a_clipboard_one() {
    let path = temp_file("names");
    let chain = create_output_chain(&file_config(&path, FileMode::Overwrite));
    let names: Vec<&str> = chain.iter().map(|o| o.name()).collect();
    assert_eq!(
        names,
        vec!["file"],
        "file mode must deliver to the file, not fall back to another sink"
    );
    let _ = std::fs::remove_dir_all(path.parent().unwrap());
}

#[tokio::test]
async fn a_streaming_session_writes_its_segments_to_the_file() {
    let path = temp_file("segments");
    let chain = create_output_chain(&file_config(&path, FileMode::Overwrite));
    // What the daemon's streaming pump does: hand each finalized segment to
    // the session's chain as it arrives.
    for segment in ["first segment", "second segment"] {
        chain[0].output(segment).await.unwrap();
    }
    assert_eq!(
        std::fs::read_to_string(&path).unwrap(),
        "first segment\nsecond segment\n"
    );
    let _ = std::fs::remove_dir_all(path.parent().unwrap());
}

#[test]
fn file_mode_without_a_path_still_falls_back() {
    let config = OutputConfig {
        mode: OutputMode::File,
        file_path: None,
        ..OutputConfig::default()
    };
    let chain = create_output_chain(&config);
    assert!(
        !chain.is_empty() && chain[0].name() != "file",
        "with no path configured the chain must fall back rather than fail"
    );
}
