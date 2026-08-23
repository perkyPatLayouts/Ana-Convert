// Ana-Convert — anaglyph 3D to full-colour stereo recovery
// SPDX-License-Identifier: GPL-3.0-or-later

//! Reading a file's shape with `ffprobe`.
//!
//! Parsing is kept separate from running the process so the awkward cases —
//! missing frame counts, unset frame rates, high bit depths — can be tested
//! against fixtures rather than requiring a file that exhibits each one.

use std::path::Path;

use crate::{FfmpegTools, MediaError};

/// What one source file contains.
#[derive(Debug, Clone, PartialEq)]
pub struct VideoInfo {
    pub width: usize,
    pub height: usize,
    /// Frames per second, from `r_frame_rate` where it is usable.
    pub fps: f64,
    /// Frame count as reported by the container, when it reports one at all.
    pub frame_count: Option<u64>,
    pub duration_secs: Option<f64>,
    /// Bits per component in the source, derived from the pixel format.
    pub bit_depth: u8,
    pub pix_fmt: String,
    pub has_audio: bool,
}

/// The pixel format to ask ffmpeg for when decoding.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SourceDepth {
    /// `rgb24` — three bytes per pixel.
    Eight,
    /// `rgb48le` — six bytes per pixel, for sources carrying more than 8 bits.
    Sixteen,
}

impl SourceDepth {
    /// The ffmpeg pixel format name.
    pub fn pix_fmt(self) -> &'static str {
        match self {
            Self::Eight => "rgb24",
            Self::Sixteen => "rgb48le",
        }
    }

    /// Bytes per pixel in that format.
    pub fn bytes_per_pixel(self) -> usize {
        match self {
            Self::Eight => 3,
            Self::Sixteen => 6,
        }
    }
}

impl VideoInfo {
    /// Which raw format to decode into: enough to carry the source without
    /// wasting pipe bandwidth on sources that never had the precision.
    pub fn source_depth(&self) -> SourceDepth {
        if self.bit_depth > 8 {
            SourceDepth::Sixteen
        } else {
            SourceDepth::Eight
        }
    }

    /// Bytes in one decoded frame at the given depth.
    pub fn frame_bytes(&self, depth: SourceDepth) -> usize {
        self.width * self.height * depth.bytes_per_pixel()
    }

    /// Best available frame count, falling back to duration times frame rate
    /// for containers that do not store one.
    pub fn estimated_frame_count(&self) -> Option<u64> {
        self.frame_count.or_else(|| {
            let duration = self.duration_secs?;
            if self.fps > 0.0 && duration > 0.0 {
                Some((duration * self.fps).round() as u64)
            } else {
                None
            }
        })
    }
}

/// Runs `ffprobe` against a file and returns what it found.
pub fn probe(tools: &FfmpegTools, path: &Path) -> Result<VideoInfo, MediaError> {
    let out = std::process::Command::new(&tools.ffprobe)
        .args([
            "-v",
            "error",
            "-show_streams",
            "-show_format",
            "-of",
            "json",
        ])
        .arg(path)
        .output()
        .map_err(|source| MediaError::Io {
            path: tools.ffprobe.clone(),
            source,
        })?;

    if !out.status.success() {
        return Err(MediaError::ToolFailed {
            tool: "ffprobe",
            message: String::from_utf8_lossy(&out.stderr).trim().to_string(),
        });
    }
    parse_probe_json(&String::from_utf8_lossy(&out.stdout))
}

/// Parses `ffprobe -show_streams -show_format -of json` output.
pub(crate) fn parse_probe_json(json: &str) -> Result<VideoInfo, MediaError> {
    let root: serde_json::Value =
        serde_json::from_str(json).map_err(|e| MediaError::ProbeParse(e.to_string()))?;

    let streams = root
        .get("streams")
        .and_then(|s| s.as_array())
        .ok_or_else(|| MediaError::ProbeParse("no streams array".into()))?;

    let is_type = |s: &serde_json::Value, kind: &str| {
        s.get("codec_type").and_then(|t| t.as_str()) == Some(kind)
    };

    let video = streams
        .iter()
        .find(|s| is_type(s, "video"))
        .ok_or_else(|| MediaError::NoVideoStream {
            path: "input".into(),
        })?;

    let uint = |v: &serde_json::Value, key: &str| -> Option<u64> {
        let field = v.get(key)?;
        // ffprobe reports some numbers as JSON numbers and others as strings.
        field
            .as_u64()
            .or_else(|| field.as_str().and_then(|s| s.parse().ok()))
    };
    let float = |v: &serde_json::Value, key: &str| -> Option<f64> {
        let field = v.get(key)?;
        field
            .as_f64()
            .or_else(|| field.as_str().and_then(|s| s.parse().ok()))
    };

    let width = uint(video, "width")
        .ok_or_else(|| MediaError::ProbeParse("video stream has no width".into()))?
        as usize;
    let height = uint(video, "height")
        .ok_or_else(|| MediaError::ProbeParse("video stream has no height".into()))?
        as usize;

    // r_frame_rate is the more accurate of the two, but is "0/0" on some
    // sources, in which case the average is all there is.
    let fps = ["r_frame_rate", "avg_frame_rate"]
        .iter()
        .filter_map(|key| video.get(*key).and_then(|v| v.as_str()))
        .find_map(parse_rational)
        .unwrap_or(0.0);

    let pix_fmt = video
        .get("pix_fmt")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();

    let bit_depth = bit_depth_of(&pix_fmt)
        .or_else(|| uint(video, "bits_per_raw_sample").map(|d| d as u8))
        .unwrap_or(8);

    let duration_secs =
        float(video, "duration").or_else(|| root.get("format").and_then(|f| float(f, "duration")));

    Ok(VideoInfo {
        width,
        height,
        fps,
        frame_count: uint(video, "nb_frames").filter(|&n| n > 0),
        duration_secs,
        bit_depth,
        pix_fmt,
        has_audio: streams.iter().any(|s| is_type(s, "audio")),
    })
}

/// Evaluates ffprobe's `"24000/1001"` style rationals. `None` for unusable
/// values like `"0/0"`, so the caller can try the next field.
fn parse_rational(text: &str) -> Option<f64> {
    let (num, den) = text.split_once('/')?;
    let (num, den): (f64, f64) = (num.parse().ok()?, den.parse().ok()?);
    if den == 0.0 || num == 0.0 {
        return None;
    }
    Some(num / den)
}

/// Reads the component depth out of a pixel format name, e.g. `yuv420p10le`.
fn bit_depth_of(pix_fmt: &str) -> Option<u8> {
    let digits: String = pix_fmt
        .trim_end_matches("le")
        .trim_end_matches("be")
        .chars()
        .rev()
        .take_while(char::is_ascii_digit)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect();
    // "yuv420p" ends in "420", which is subsampling, not depth. Only formats
    // whose trailing digits follow a marker like "p" or "s" carry a depth, and
    // real depths are 9 or more — 8-bit formats simply omit the suffix.
    let depth: u8 = digits.parse().ok()?;
    (9..=16).contains(&depth).then_some(depth)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn video_stream(extra: &str) -> String {
        format!(
            r#"{{"streams":[{{"index":0,"codec_type":"video","codec_name":"h264",
               "width":1920,"height":1080,"pix_fmt":"yuv420p",
               "r_frame_rate":"24/1","avg_frame_rate":"24/1"{extra}}}],
               "format":{{"duration":"10.000000"}}}}"#
        )
    }

    #[test]
    fn dimensions_and_frame_rate_are_read_from_the_video_stream() {
        let info = parse_probe_json(&video_stream(r#","nb_frames":"240""#)).expect("parse");
        assert_eq!((info.width, info.height), (1920, 1080));
        assert_eq!(info.fps, 24.0);
        assert_eq!(info.frame_count, Some(240));
    }

    #[test]
    fn a_fractional_frame_rate_is_evaluated() {
        // 23.976 arrives as a rational and must not be truncated to 23.
        let json =
            video_stream("").replace(r#""r_frame_rate":"24/1""#, r#""r_frame_rate":"24000/1001""#);
        let info = parse_probe_json(&json).expect("parse");
        assert!((info.fps - 23.976).abs() < 0.001, "got {}", info.fps);
    }

    #[test]
    fn a_missing_frame_count_is_estimated_from_the_duration() {
        // Matroska routinely omits nb_frames, and progress reporting needs a
        // total from somewhere.
        let info = parse_probe_json(&video_stream("")).expect("parse");
        assert_eq!(info.frame_count, None, "nothing was reported");
        assert_eq!(info.estimated_frame_count(), Some(240), "10s at 24fps");
    }

    #[test]
    fn an_unusable_frame_rate_falls_back_to_the_average() {
        let json = video_stream("").replace(r#""r_frame_rate":"24/1""#, r#""r_frame_rate":"0/0""#);
        let info = parse_probe_json(&json).expect("parse");
        assert_eq!(info.fps, 24.0, "should have used avg_frame_rate");
    }

    #[test]
    fn an_eight_bit_source_decodes_as_rgb24() {
        let info = parse_probe_json(&video_stream("")).expect("parse");
        assert_eq!(info.bit_depth, 8);
        assert_eq!(info.source_depth(), SourceDepth::Eight);
        assert_eq!(info.source_depth().pix_fmt(), "rgb24");
    }

    #[test]
    fn a_ten_bit_source_decodes_as_rgb48() {
        // Losing the extra bits at the front door would undo the point of a
        // float pipeline.
        let json = video_stream("").replace(r#""pix_fmt":"yuv420p""#, r#""pix_fmt":"yuv420p10le""#);
        let info = parse_probe_json(&json).expect("parse");
        assert_eq!(info.bit_depth, 10);
        assert_eq!(info.source_depth(), SourceDepth::Sixteen);
        assert_eq!(info.source_depth().pix_fmt(), "rgb48le");
    }

    #[test]
    fn a_twelve_bit_source_is_recognised() {
        let json = video_stream("").replace(r#""pix_fmt":"yuv420p""#, r#""pix_fmt":"yuv444p12le""#);
        assert_eq!(parse_probe_json(&json).expect("parse").bit_depth, 12);
    }

    #[test]
    fn frame_size_follows_the_chosen_depth() {
        let info = parse_probe_json(&video_stream("")).expect("parse");
        assert_eq!(info.frame_bytes(SourceDepth::Eight), 1920 * 1080 * 3);
        assert_eq!(info.frame_bytes(SourceDepth::Sixteen), 1920 * 1080 * 6);
    }

    #[test]
    fn audio_presence_is_reported() {
        let video_only = parse_probe_json(&video_stream("")).expect("parse");
        assert!(!video_only.has_audio);

        let with_audio = r#"{"streams":[
            {"index":0,"codec_type":"video","width":64,"height":48,"pix_fmt":"yuv420p",
             "r_frame_rate":"10/1","avg_frame_rate":"10/1"},
            {"index":1,"codec_type":"audio","codec_name":"aac"}],
            "format":{"duration":"1.0"}}"#;
        assert!(parse_probe_json(with_audio).expect("parse").has_audio);
    }

    #[test]
    fn a_file_with_no_video_stream_is_an_error() {
        let audio_only = r#"{"streams":[{"index":0,"codec_type":"audio","codec_name":"aac"}],
                             "format":{"duration":"1.0"}}"#;
        assert!(parse_probe_json(audio_only).is_err());
    }

    #[test]
    fn malformed_output_is_an_error_rather_than_a_panic() {
        assert!(parse_probe_json("not json at all").is_err());
        assert!(parse_probe_json("{}").is_err());
    }

    // --- against the real ffprobe ---

    #[test]
    fn probing_a_generated_clip_matches_what_was_asked_for() {
        let (tools, _) = crate::locate(None).expect("ffmpeg must be installed");
        let dir = tempfile::tempdir().expect("temp dir");
        let clip = dir.path().join("clip.mp4");
        crate::testing::make_test_clip(&tools, &clip, 64, 48, 10, 10.0);

        let info = probe(&tools, &clip).expect("probe");
        assert_eq!((info.width, info.height), (64, 48));
        assert_eq!(info.fps, 10.0);
        assert_eq!(info.estimated_frame_count(), Some(10));
        assert!(info.has_audio, "the fixture includes an audio track");
        assert_eq!(info.source_depth(), SourceDepth::Eight);
    }

    #[test]
    fn probing_a_missing_file_is_an_error() {
        let (tools, _) = crate::locate(None).expect("ffmpeg must be installed");
        let err = probe(&tools, Path::new("/definitely/not/here.mp4")).expect_err("should fail");
        assert!(
            !err.to_string().is_empty(),
            "the error must say something useful"
        );
    }
}
