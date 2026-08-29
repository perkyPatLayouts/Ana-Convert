// Ana-Convert — anaglyph 3D to full-colour stereo recovery
// SPDX-License-Identifier: GPL-3.0-or-later

//! Decoding video into float frames.
//!
//! Two access patterns, because the app needs both: a sequential [`Decoder`]
//! for rendering a whole file, and [`grab_frame`] for the preview, which jumps
//! around and wants one frame at a time.

use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdout, Command, Stdio};

use ana_core::FrameF32;

use crate::probe::{SourceDepth, VideoInfo};
use crate::{file_arg, FfmpegTools, MediaError};

/// Streams frames out of a file in order.
///
/// The child process is killed when this is dropped, so abandoning a render
/// never leaves an ffmpeg running.
pub struct Decoder {
    child: Child,
    stdout: Option<ChildStdout>,
    path: PathBuf,
    depth: SourceDepth,
    width: usize,
    height: usize,
    buffer: Vec<u8>,
    frames_read: u64,
}

impl Decoder {
    /// Opens a file for sequential decoding from `start`.
    ///
    /// Seeking rather than discarding matters: a trim beginning half an hour
    /// into a film would otherwise decode tens of thousands of frames just to
    /// throw them away.
    pub fn open_at(
        tools: &FfmpegTools,
        path: &Path,
        info: &VideoInfo,
        start: u64,
    ) -> Result<Self, MediaError> {
        let depth = info.source_depth();
        let mut command = Command::new(&tools.ffmpeg);
        command.args(["-nostdin", "-v", "error"]);
        apply_seek(&mut command, path, info, start);

        let mut child = command
            .args([
                "-f",
                "rawvideo",
                "-pix_fmt",
                depth.pix_fmt(),
                // Video only: audio is carried separately at encode time.
                "-an",
                "-",
            ])
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|source| MediaError::Io {
                path: tools.ffmpeg.clone(),
                source,
            })?;

        let stdout = child.stdout.take();
        Ok(Self {
            child,
            stdout,
            path: path.to_path_buf(),
            depth,
            width: info.width,
            height: info.height,
            buffer: vec![0u8; info.frame_bytes(depth)],
            frames_read: 0,
        })
    }

    /// Opens a file for sequential decoding from the beginning.
    pub fn open(tools: &FfmpegTools, path: &Path, info: &VideoInfo) -> Result<Self, MediaError> {
        Self::open_at(tools, path, info, 0)
    }

    /// Reads the next frame, or `None` at end of stream.
    pub fn next_frame(&mut self) -> Result<Option<FrameF32>, MediaError> {
        let Some(stdout) = self.stdout.as_mut() else {
            return Ok(None);
        };

        match stdout.read_exact(&mut self.buffer) {
            Ok(()) => {
                self.frames_read += 1;
                Ok(Some(decode_frame_bytes(
                    &self.buffer,
                    self.width,
                    self.height,
                    self.depth,
                )))
            }
            // A clean end of stream lands exactly on a frame boundary. Anything
            // else means the stream was cut mid-frame and is a real failure.
            Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => {
                self.stdout = None;
                Ok(None)
            }
            Err(source) => Err(MediaError::Io {
                path: self.path.clone(),
                source,
            }),
        }
    }

    /// How many frames have been handed out so far.
    pub fn frames_read(&self) -> u64 {
        self.frames_read
    }
}

impl Drop for Decoder {
    fn drop(&mut self) {
        // Closing the pipe first lets ffmpeg exit on its own; the kill is for
        // the case where it is blocked writing.
        drop(self.stdout.take());
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// Decodes a single frame by index, for preview scrubbing.
///
/// Seeks before the input so ffmpeg can jump by keyframe rather than decoding
/// from the start, then steps forward to land on the exact frame.
pub fn grab_frame(
    tools: &FfmpegTools,
    path: &Path,
    info: &VideoInfo,
    index: u64,
) -> Result<FrameF32, MediaError> {
    let depth = info.source_depth();
    let mut command = Command::new(&tools.ffmpeg);
    command.args(["-nostdin", "-v", "error"]);
    apply_seek(&mut command, path, info, index);

    let out = command
        .args([
            "-frames:v",
            "1",
            "-f",
            "rawvideo",
            "-pix_fmt",
            depth.pix_fmt(),
            "-an",
            "-",
        ])
        .stdin(Stdio::null())
        .stderr(Stdio::piped())
        .output()
        .map_err(|source| MediaError::Io {
            path: tools.ffmpeg.clone(),
            source,
        })?;

    let expected = info.frame_bytes(depth);
    if out.stdout.len() < expected {
        return Err(MediaError::ToolFailed {
            tool: "ffmpeg",
            message: format!(
                "wanted {expected} bytes for frame {index}, got {}. {}",
                out.stdout.len(),
                String::from_utf8_lossy(&out.stderr).trim()
            ),
        });
    }
    Ok(decode_frame_bytes(
        &out.stdout[..expected],
        info.width,
        info.height,
        depth,
    ))
}

/// Points ffmpeg at `path`, starting output at frame `index`.
///
/// Shared by both entry points so that "frame N" means exactly one thing: the
/// preview and the render must never disagree about which frame they are on.
///
/// Seeks to shortly before the target so ffmpeg can start from a keyframe, then
/// selects the exact frame. Seeking straight to the frame's own timestamp risks
/// landing early or late on long-GOP sources.
fn apply_seek(command: &mut Command, path: &Path, info: &VideoInfo, index: u64) {
    if index == 0 || info.fps <= 0.0 {
        command.arg("-i").arg(file_arg(path));
        return;
    }
    let lead_in = 1.0_f64.min(index as f64 / info.fps);
    let seek_to = (index as f64 / info.fps) - lead_in;
    let skip = (lead_in * info.fps).round() as u64;
    command
        .args(["-ss", &format!("{seek_to:.6}")])
        .arg("-i")
        .arg(file_arg(path))
        // `-fps_mode passthrough` stops ffmpeg duplicating or dropping frames
        // to hit a target rate. It replaced `-vsync`, removed in ffmpeg 8.
        .args([
            "-vf",
            &format!(r"select=gte(n\,{skip})"),
            "-fps_mode",
            "passthrough",
        ]);
}

/// Converts one raw frame's bytes into a float frame.
pub(crate) fn decode_frame_bytes(
    bytes: &[u8],
    width: usize,
    height: usize,
    depth: SourceDepth,
) -> FrameF32 {
    match depth {
        SourceDepth::Eight => FrameF32::from_rgb8(width, height, bytes),
        SourceDepth::Sixteen => {
            let samples: Vec<u16> = bytes
                .chunks_exact(2)
                .map(|p| u16::from_le_bytes([p[0], p[1]]))
                .collect();
            FrameF32::from_rgb16(width, height, &samples)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::probe::probe;
    use crate::testing::{make_solid_clip, make_test_clip};

    fn tools() -> FfmpegTools {
        crate::locate(None).expect("ffmpeg must be installed").0
    }

    #[test]
    fn eight_bit_bytes_become_normalised_floats() {
        let bytes = [255u8, 0, 0, 0, 128, 255];
        let frame = decode_frame_bytes(&bytes, 2, 1, SourceDepth::Eight);
        let (r, g, b) = frame.rgb_planes();
        assert_eq!(r, &[1.0, 0.0]);
        assert_eq!(g, &[0.0, 128.0 / 255.0]);
        assert_eq!(b, &[0.0, 1.0]);
    }

    #[test]
    fn sixteen_bit_bytes_are_read_little_endian() {
        // rgb48le: low byte first. Reading these the other way round would put
        // full white at 255/65535 instead of 1.0.
        let bytes = [0xFF, 0xFF, 0x00, 0x00, 0x00, 0x80];
        let frame = decode_frame_bytes(&bytes, 1, 1, SourceDepth::Sixteen);
        let (r, g, b) = frame.rgb_planes();
        assert_eq!(r, &[1.0], "0xFFFF is white");
        assert_eq!(g, &[0.0]);
        assert!(
            (b[0] - 0x8000 as f32 / 65535.0).abs() < 1e-6,
            "got {}",
            b[0]
        );
    }

    #[test]
    fn a_clip_decodes_the_number_of_frames_it_contains() {
        let t = tools();
        let dir = tempfile::tempdir().expect("temp dir");
        let clip = dir.path().join("clip.mp4");
        make_test_clip(&t, &clip, 64, 48, 10, 10.0);
        let info = probe(&t, &clip).expect("probe");

        let mut decoder = Decoder::open(&t, &clip, &info).expect("open");
        let mut count = 0;
        while decoder.next_frame().expect("decode").is_some() {
            count += 1;
        }
        assert_eq!(count, 10);
        assert_eq!(decoder.frames_read(), 10);
    }

    #[test]
    fn decoded_frames_have_the_probed_geometry() {
        let t = tools();
        let dir = tempfile::tempdir().expect("temp dir");
        let clip = dir.path().join("clip.mp4");
        make_test_clip(&t, &clip, 64, 48, 3, 10.0);
        let info = probe(&t, &clip).expect("probe");

        let mut decoder = Decoder::open(&t, &clip, &info).expect("open");
        let frame = decoder
            .next_frame()
            .expect("decode")
            .expect("a first frame");
        assert_eq!(
            (frame.width(), frame.height(), frame.channels()),
            (64, 48, 3)
        );
    }

    #[test]
    fn a_solid_colour_clip_decodes_to_that_colour() {
        // Lossless source, so this pins down the whole chain: pixel format,
        // channel order and normalisation all have to be right at once.
        let t = tools();
        let dir = tempfile::tempdir().expect("temp dir");
        let clip = dir.path().join("solid.mkv");
        make_solid_clip(&t, &clip, 32, 16, 2, "red");
        let info = probe(&t, &clip).expect("probe");

        let mut decoder = Decoder::open(&t, &clip, &info).expect("open");
        let frame = decoder.next_frame().expect("decode").expect("a frame");
        let (r, g, b) = frame.rgb_planes();
        assert!(r[0] > 0.9, "red channel should be high, got {}", r[0]);
        assert!(g[0] < 0.1, "green channel should be low, got {}", g[0]);
        assert!(b[0] < 0.1, "blue channel should be low, got {}", b[0]);
    }

    #[test]
    fn decoding_stops_cleanly_at_the_end() {
        let t = tools();
        let dir = tempfile::tempdir().expect("temp dir");
        let clip = dir.path().join("clip.mp4");
        make_test_clip(&t, &clip, 32, 32, 2, 10.0);
        let info = probe(&t, &clip).expect("probe");

        let mut decoder = Decoder::open(&t, &clip, &info).expect("open");
        while decoder.next_frame().expect("decode").is_some() {}
        assert!(
            decoder.next_frame().expect("decode past the end").is_none(),
            "reading past the end must keep returning None, not error"
        );
    }

    #[test]
    fn grabbing_a_frame_by_index_returns_that_frame() {
        // testsrc2 animates, so different indices must differ — otherwise the
        // seek is silently landing on frame zero every time.
        let t = tools();
        let dir = tempfile::tempdir().expect("temp dir");
        let clip = dir.path().join("clip.mp4");
        make_test_clip(&t, &clip, 64, 48, 20, 10.0);
        let info = probe(&t, &clip).expect("probe");

        let first = grab_frame(&t, &clip, &info, 0).expect("grab 0");
        let later = grab_frame(&t, &clip, &info, 15).expect("grab 15");
        assert_eq!((later.width(), later.height()), (64, 48));
        assert_ne!(
            first.as_slice(),
            later.as_slice(),
            "frame 15 should not be identical to frame 0"
        );
    }

    #[test]
    fn grabbing_the_same_index_twice_gives_the_same_frame() {
        let t = tools();
        let dir = tempfile::tempdir().expect("temp dir");
        let clip = dir.path().join("clip.mp4");
        make_test_clip(&t, &clip, 64, 48, 20, 10.0);
        let info = probe(&t, &clip).expect("probe");

        let a = grab_frame(&t, &clip, &info, 12).expect("grab");
        let b = grab_frame(&t, &clip, &info, 12).expect("grab again");
        assert_eq!(a.as_slice(), b.as_slice(), "scrubbing must be repeatable");
    }

    #[test]
    fn grabbing_matches_sequential_decoding_at_the_same_index() {
        // The preview and the render must agree about what frame N is.
        let t = tools();
        let dir = tempfile::tempdir().expect("temp dir");
        let clip = dir.path().join("clip.mkv");
        make_solid_clip(&t, &clip, 32, 24, 8, "blue");
        let info = probe(&t, &clip).expect("probe");

        let mut decoder = Decoder::open(&t, &clip, &info).expect("open");
        let mut sequential = None;
        for _ in 0..=5 {
            sequential = decoder.next_frame().expect("decode");
        }
        let grabbed = grab_frame(&t, &clip, &info, 5).expect("grab");
        assert_eq!(
            sequential.expect("frame 5").as_slice(),
            grabbed.as_slice(),
            "sequential and seeked decoding disagree about frame 5"
        );
    }

    #[test]
    fn opening_at_a_frame_starts_there() {
        // The frame a seeked decoder hands over first must be the same one
        // grab_frame would return, or a trimmed render silently starts in the
        // wrong place.
        let t = tools();
        let dir = tempfile::tempdir().expect("temp dir");
        let clip = dir.path().join("clip.mkv");
        make_test_clip(&t, &clip, 64, 48, 30, 10.0);
        let info = probe(&t, &clip).expect("probe");

        for start in [0u64, 1, 7, 20] {
            let mut decoder = Decoder::open_at(&t, &clip, &info, start).expect("open");
            let got = decoder.next_frame().expect("decode").expect("a frame");
            let want = grab_frame(&t, &clip, &info, start).expect("grab");
            assert_eq!(
                got.as_slice(),
                want.as_slice(),
                "seeking to {start} landed on a different frame"
            );
        }
    }

    #[test]
    fn a_seeked_decoder_yields_the_rest_of_the_file() {
        let t = tools();
        let dir = tempfile::tempdir().expect("temp dir");
        let clip = dir.path().join("clip.mkv");
        make_test_clip(&t, &clip, 64, 48, 20, 10.0);
        let info = probe(&t, &clip).expect("probe");

        let mut decoder = Decoder::open_at(&t, &clip, &info, 12).expect("open");
        let mut count = 0;
        while decoder.next_frame().expect("decode").is_some() {
            count += 1;
        }
        assert_eq!(count, 8, "20 frames starting at 12 leaves 8");
    }

    #[test]
    fn a_seeked_decoder_stays_in_step_with_sequential_decoding() {
        let t = tools();
        let dir = tempfile::tempdir().expect("temp dir");
        let clip = dir.path().join("clip.mkv");
        make_test_clip(&t, &clip, 48, 32, 24, 10.0);
        let info = probe(&t, &clip).expect("probe");

        let mut plain = Decoder::open(&t, &clip, &info).expect("open");
        let mut from_five = Decoder::open_at(&t, &clip, &info, 5).expect("open");
        for _ in 0..5 {
            plain.next_frame().expect("decode");
        }
        // Both are now at frame 5; they must stay together from here.
        for step in 0..6 {
            let a = plain.next_frame().expect("decode").expect("frame");
            let b = from_five.next_frame().expect("decode").expect("frame");
            assert_eq!(a.as_slice(), b.as_slice(), "diverged {step} frames in");
        }
    }

    #[test]
    fn seeking_past_the_end_yields_nothing_rather_than_failing() {
        let t = tools();
        let dir = tempfile::tempdir().expect("temp dir");
        let clip = dir.path().join("clip.mkv");
        make_test_clip(&t, &clip, 32, 32, 6, 10.0);
        let info = probe(&t, &clip).expect("probe");

        let mut decoder = Decoder::open_at(&t, &clip, &info, 500).expect("open");
        assert!(decoder.next_frame().expect("decode").is_none());
    }

    #[test]
    fn a_filename_is_never_taken_as_an_ffmpeg_protocol() {
        // The same rule the probe follows: what is handed over is a file name,
        // never a URL for ffmpeg to go and resolve.
        let t = tools();
        let dir = tempfile::tempdir().expect("temp dir");
        let clip = dir.path().join("clip.mkv");
        make_test_clip(&t, &clip, 64, 48, 5, 10.0);
        let info = probe(&t, &clip).expect("probe");

        let disguised = PathBuf::from(format!("cache:file:{}", clip.display()));
        assert!(
            grab_frame(&t, &disguised, &info, 0).is_err(),
            "the cache: protocol was resolved instead of looking for a file of that name"
        );
    }

    #[test]
    fn a_path_containing_a_colon_still_decodes() {
        let t = tools();
        let dir = tempfile::tempdir().expect("temp dir");
        let clip = dir.path().join("my:film.mkv");
        make_test_clip(&t, &clip, 64, 48, 5, 10.0);
        let info = probe(&t, &clip).expect("probe");

        let frame = grab_frame(&t, &clip, &info, 1).expect("grab");
        assert_eq!((frame.width(), frame.height()), (64, 48));
    }

    #[test]
    fn opening_a_missing_file_is_an_error() {
        let t = tools();
        let info = VideoInfo {
            width: 64,
            height: 48,
            fps: 10.0,
            frame_count: Some(1),
            duration_secs: Some(0.1),
            sample_aspect: 1.0,
            bit_depth: 8,
            pix_fmt: "yuv420p".into(),
            has_audio: false,
        };
        let mut decoder = Decoder::open(&t, Path::new("/definitely/not/here.mp4"), &info)
            .expect("spawning succeeds; the failure surfaces on read");
        assert!(
            decoder.next_frame().is_err() || decoder.next_frame().unwrap().is_none(),
            "a missing input must not yield frames"
        );
    }
}
