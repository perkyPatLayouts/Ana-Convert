// Ana-Convert — anaglyph 3D to full-colour stereo recovery
// SPDX-License-Identifier: GPL-3.0-or-later

//! Fixtures for the crate's own tests.
//!
//! Generating clips with ffmpeg beats checking binaries into the repository:
//! the fixtures stay small, their contents are described in one place, and
//! they exercise the same decode path a real file would.

#![cfg(test)]

use std::path::Path;
use std::process::Command;

use crate::FfmpegTools;

/// Writes a short test clip with a moving pattern and a tone.
///
/// Panics rather than returning an error: a fixture that will not build is a
/// broken test environment, not a case worth handling.
pub fn make_test_clip(
    tools: &FfmpegTools,
    path: &Path,
    width: usize,
    height: usize,
    frames: usize,
    fps: f64,
) {
    let duration = frames as f64 / fps;
    run(
        tools,
        &[
            "-y",
            "-v",
            "error",
            "-f",
            "lavfi",
            "-i",
            &format!("testsrc2=size={width}x{height}:rate={fps}:duration={duration}"),
            "-f",
            "lavfi",
            "-i",
            &format!("sine=frequency=440:duration={duration}"),
            "-c:v",
            "libx264",
            "-pix_fmt",
            "yuv420p",
            "-c:a",
            "aac",
            &path.to_string_lossy(),
        ],
    );
}

/// Writes a clip of one solid colour, so decoded values can be predicted.
pub fn make_solid_clip(
    tools: &FfmpegTools,
    path: &Path,
    width: usize,
    height: usize,
    frames: usize,
    colour: &str,
) {
    run(
        tools,
        &[
            "-y",
            "-v",
            "error",
            "-f",
            "lavfi",
            "-i",
            &format!(
                "color=c={colour}:size={width}x{height}:rate=10:duration={}",
                frames as f64 / 10.0
            ),
            "-c:v",
            "ffv1", // lossless, so the test can assert on exact values
            &path.to_string_lossy(),
        ],
    );
}

fn run(tools: &FfmpegTools, args: &[&str]) {
    let out = Command::new(&tools.ffmpeg)
        .args(args)
        .output()
        .expect("run ffmpeg");
    assert!(
        out.status.success(),
        "ffmpeg {args:?} failed:\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
}
