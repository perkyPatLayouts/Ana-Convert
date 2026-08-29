// Ana-Convert — anaglyph 3D to full-colour stereo recovery
// SPDX-License-Identifier: GPL-3.0-or-later

//! Video I/O for Ana-Convert.
//!
//! Decoding and encoding are done by driving `ffmpeg` as a child process and
//! moving raw frames over pipes, rather than linking `libav*`. That keeps the
//! build identical on macOS, Linux and Windows, keeps every codec ffmpeg
//! supports available without extra work, and keeps the licensing story simple.
//! The cost is a bundled binary and one process per stream.

use std::ffi::OsString;
use std::path::{Path, PathBuf};

pub mod decode;
pub mod encode;
pub mod locate;
pub mod probe;
#[cfg(any(test, feature = "fixtures"))]
pub mod testing;

pub use decode::{grab_frame, Decoder};
pub use encode::{EncodeSettings, Encoder, VideoCodec};
pub use locate::{locate, FfmpegTools, ToolSource};
pub use probe::{probe, SourceDepth, VideoInfo};

/// A path as ffmpeg should see it: the name of a file, never a URL.
///
/// ffmpeg resolves protocol prefixes inside input and output names, so a file
/// called `concat:…` or `http:…` would be reached through that protocol instead
/// of being opened, and a relative name beginning with `-` would be read as an
/// option — ffmpeg has no `--` to stop it. The `file:` prefix settles both: it
/// pins the name to the filesystem and it cannot start with a dash.
///
/// Built as an `OsString` rather than through `Display` so that a path which is
/// not valid UTF-8 survives intact.
pub(crate) fn file_arg(path: &Path) -> OsString {
    let mut arg = OsString::from("file:");
    arg.push(path);
    arg
}

/// Anything that can go wrong reading or writing media.
#[derive(Debug, thiserror::Error)]
pub enum MediaError {
    #[error("could not find ffmpeg and ffprobe in {0}")]
    ToolsNotInDirectory(PathBuf),

    #[error(
        "could not find ffmpeg and ffprobe.\n\
         Install them (on macOS: `brew install ffmpeg`) or point Ana-Convert at \
         a directory containing both."
    )]
    ToolsNotFound,

    #[error("{tool} failed: {message}")]
    ToolFailed { tool: &'static str, message: String },

    #[error("{path} has no video stream")]
    NoVideoStream { path: String },

    #[error("could not understand ffprobe's output: {0}")]
    ProbeParse(String),

    #[error(
        "the file claims to be {width}x{height}, which is not a frame size this \
         can work with. It is most likely damaged."
    )]
    ImplausibleGeometry { width: usize, height: usize },

    #[error("expected a {}x{} frame, got {}x{}", expected.0, expected.1, got.0, got.1)]
    FrameSizeMismatch {
        expected: (usize, usize),
        got: (usize, usize),
    },

    #[error("could not read {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
}
