// Ana-Convert — anaglyph 3D to full-colour stereo recovery
// SPDX-License-Identifier: GPL-3.0-or-later

//! Finding the `ffmpeg` and `ffprobe` binaries.
//!
//! A shipped build carries its own pair inside the app bundle, but during
//! development they usually come from the system. Failing to find them must
//! produce something a user can act on, never a silent fallback.

use std::path::{Path, PathBuf};

use crate::MediaError;

/// The two binaries the rest of the crate drives.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FfmpegTools {
    pub ffmpeg: PathBuf,
    pub ffprobe: PathBuf,
}

/// Where a located pair came from, for display in diagnostics.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToolSource {
    /// An explicit directory the user pointed us at.
    Override,
    /// Alongside the running executable — how a shipped bundle finds its own.
    Bundled,
    /// Found on `PATH`.
    SystemPath,
}

impl FfmpegTools {
    /// Looks for both binaries in one directory.
    ///
    /// Both or neither: half a toolchain would only fail later, somewhere less
    /// legible than here.
    pub fn in_directory(dir: &Path) -> Option<Self> {
        let ffmpeg = executable_at(&dir.join(exe_name("ffmpeg")))?;
        let ffprobe = executable_at(&dir.join(exe_name("ffprobe")))?;
        Some(Self { ffmpeg, ffprobe })
    }
}

/// Finds ffmpeg and ffprobe, preferring an override, then a bundled copy, then
/// whatever is on `PATH`.
pub fn locate(override_dir: Option<&Path>) -> Result<(FfmpegTools, ToolSource), MediaError> {
    // An override that does not resolve is an error, never a fall-through:
    // silently using a different build than the one asked for is worse than
    // refusing to start.
    if let Some(dir) = override_dir {
        return FfmpegTools::in_directory(dir)
            .map(|t| (t, ToolSource::Override))
            .ok_or_else(|| MediaError::ToolsNotInDirectory(dir.to_path_buf()));
    }

    if let Some(tools) = bundled_directory()
        .as_deref()
        .and_then(FfmpegTools::in_directory)
    {
        return Ok((tools, ToolSource::Bundled));
    }

    if let Some(tools) = search_path() {
        return Ok((tools, ToolSource::SystemPath));
    }

    Err(MediaError::ToolsNotFound)
}

/// The directory holding the running executable — where a shipped `.app` keeps
/// its own copies.
fn bundled_directory() -> Option<PathBuf> {
    std::env::current_exe()
        .ok()?
        .parent()
        .map(Path::to_path_buf)
}

fn search_path() -> Option<FfmpegTools> {
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path).find_map(|dir| FfmpegTools::in_directory(&dir))
}

/// Returns the path if it names a file we are actually allowed to execute.
fn executable_at(path: &Path) -> Option<PathBuf> {
    let meta = std::fs::metadata(path).ok()?;
    if !meta.is_file() || !is_executable(&meta) {
        return None;
    }
    Some(path.to_path_buf())
}

#[cfg(unix)]
fn is_executable(meta: &std::fs::Metadata) -> bool {
    use std::os::unix::fs::PermissionsExt;
    meta.permissions().mode() & 0o111 != 0
}

#[cfg(not(unix))]
fn is_executable(_meta: &std::fs::Metadata) -> bool {
    // Windows has no execute bit; the `.exe` suffix carries the meaning.
    true
}

fn exe_name(stem: &str) -> String {
    if cfg!(windows) {
        format!("{stem}.exe")
    } else {
        stem.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A directory holding fake executables with the given names.
    fn fake_tools_dir(names: &[&str]) -> tempfile::TempDir {
        let dir = tempfile::tempdir().expect("temp dir");
        for name in names {
            let path = dir.path().join(name);
            std::fs::write(&path, b"#!/bin/sh\nexit 0\n").expect("write fake tool");
            make_executable(&path);
        }
        dir
    }

    fn make_executable(path: &Path) {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = std::fs::metadata(path).expect("metadata").permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(path, perms).expect("chmod");
    }

    #[test]
    fn a_directory_with_both_binaries_is_accepted() {
        let dir = fake_tools_dir(&["ffmpeg", "ffprobe"]);
        let found = FfmpegTools::in_directory(dir.path()).expect("should find both");
        assert_eq!(found.ffmpeg, dir.path().join("ffmpeg"));
        assert_eq!(found.ffprobe, dir.path().join("ffprobe"));
    }

    #[test]
    fn a_directory_with_only_ffmpeg_is_rejected() {
        // Half a toolchain is not a toolchain: probing would fail later, and
        // far less legibly.
        let dir = fake_tools_dir(&["ffmpeg"]);
        assert_eq!(FfmpegTools::in_directory(dir.path()), None);
    }

    #[test]
    fn an_empty_directory_is_rejected() {
        let dir = fake_tools_dir(&[]);
        assert_eq!(FfmpegTools::in_directory(dir.path()), None);
    }

    #[test]
    fn a_non_executable_file_does_not_count() {
        let dir = tempfile::tempdir().expect("temp dir");
        for name in ["ffmpeg", "ffprobe"] {
            std::fs::write(dir.path().join(name), b"not a program").expect("write");
        }
        assert_eq!(
            FfmpegTools::in_directory(dir.path()),
            None,
            "a plain file must not be mistaken for a binary"
        );
    }

    #[test]
    fn an_override_directory_wins() {
        let dir = fake_tools_dir(&["ffmpeg", "ffprobe"]);
        let (tools, source) = locate(Some(dir.path())).expect("override should be used");
        assert_eq!(source, ToolSource::Override);
        assert_eq!(tools.ffmpeg, dir.path().join("ffmpeg"));
    }

    #[test]
    fn a_bad_override_reports_the_directory_it_tried() {
        // Silently falling through to PATH would leave the user believing their
        // chosen build was in use.
        let dir = fake_tools_dir(&[]);
        let err = locate(Some(dir.path())).expect_err("should not fall through");
        let message = err.to_string();
        assert!(
            message.contains(&dir.path().display().to_string()),
            "error must name the directory, got: {message}"
        );
    }

    #[test]
    fn the_system_path_is_used_when_there_is_no_override() {
        // ffmpeg is a hard requirement of this crate's test suite.
        let (tools, source) = locate(None).expect("ffmpeg must be installed to run these tests");
        assert!(
            matches!(source, ToolSource::SystemPath | ToolSource::Bundled),
            "unexpected source {source:?}"
        );
        assert!(tools.ffmpeg.exists(), "{:?} should exist", tools.ffmpeg);
        assert!(tools.ffprobe.exists(), "{:?} should exist", tools.ffprobe);
    }

    #[test]
    fn located_tools_actually_run() {
        let (tools, _) = locate(None).expect("locate");
        let out = std::process::Command::new(&tools.ffmpeg)
            .arg("-version")
            .output()
            .expect("run ffmpeg");
        assert!(out.status.success(), "ffmpeg -version failed");
        assert!(
            String::from_utf8_lossy(&out.stdout).contains("ffmpeg version"),
            "unexpected output from the located binary"
        );
    }
}
