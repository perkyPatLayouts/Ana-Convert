// Ana-Convert — anaglyph 3D to full-colour stereo recovery
// SPDX-License-Identifier: GPL-3.0-or-later

//! Video I/O for Ana-Convert.
//!
//! Decoding and encoding are done by driving `ffmpeg` as a child process and
//! moving raw frames over pipes, rather than linking `libav*`. That keeps the
//! build identical on macOS, Linux and Windows, keeps every codec ffmpeg
//! supports available without extra work, and keeps the licensing story simple.
//! The cost is a bundled binary and one process per stream.

use std::path::PathBuf;

pub mod decode;
pub mod encode;
pub mod locate;
pub mod probe;
mod testing;

pub use decode::{grab_frame, Decoder};
pub use encode::{EncodeSettings, Encoder, VideoCodec};
pub use locate::{locate, FfmpegTools, ToolSource};
pub use probe::{probe, SourceDepth, VideoInfo};

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
