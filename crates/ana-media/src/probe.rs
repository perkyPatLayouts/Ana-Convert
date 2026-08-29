// Ana-Convert — anaglyph 3D to full-colour stereo recovery
// SPDX-License-Identifier: GPL-3.0-or-later

//! Reading a file's shape with `ffprobe`.
//!
//! Parsing is kept separate from running the process so the awkward cases —
//! missing frame counts, unset frame rates, high bit depths — can be tested
//! against fixtures rather than requiring a file that exhibits each one.

use std::path::Path;

use ana_core::params::MAX_DIMENSION;

use crate::{file_arg, FfmpegTools, MediaError};

/// What one source file contains.
#[derive(Debug, Clone, PartialEq)]
pub struct VideoInfo {
    pub width: usize,
    pub height: usize,
    /// Frames per second, from `avg_frame_rate` where it is usable.
    pub fps: f64,
    /// Frame count as reported by the container, when it reports one at all.
    pub frame_count: Option<u64>,
    pub duration_secs: Option<f64>,
    /// Pixel aspect ratio: the shape of one stored pixel.
    ///
    /// Not always square. A DVD-sourced transfer routinely stores 708x276 with
    /// 8:9 pixels, meaning it displays narrower than its stored dimensions
    /// suggest. Raw video carries no such metadata, so this has to be read here
    /// and put back at encode time or every output comes out stretched.
    pub sample_aspect: f64,
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
    /// The shape the frame is meant to be seen at, accounting for pixel shape.
    pub fn display_aspect(&self) -> f64 {
        if self.height == 0 {
            return 1.0;
        }
        self.width as f64 * self.sample_aspect / self.height as f64
    }

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
        .arg(file_arg(path))
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

    // Believed without question, these size every buffer the decode allocates,
    // and they are whatever the container says they are.
    if width > MAX_DIMENSION || height > MAX_DIMENSION {
        return Err(MediaError::ImplausibleGeometry { width, height });
    }

    // avg_frame_rate first, deliberately. r_frame_rate is the lowest rate that
    // can represent every timestamp, which on a real 29.97 disc rip reads as a
    // round 30 — enough to seek nearly six frames wrong three minutes in, and
    // to drift the audio across a feature. avg_frame_rate is frames over
    // duration, which is exactly the frame-to-time mapping seeking needs.
    // It is "0/0" on some sources, hence the fallback.
    let fps = ["avg_frame_rate", "r_frame_rate"]
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
        sample_aspect: pixel_aspect(video, width, height),
        frame_count: uint(video, "nb_frames").filter(|&n| n > 0),
        duration_secs,
        bit_depth,
        pix_fmt,
        has_audio: streams.iter().any(|s| is_type(s, "audio")),
    })
}

/// The shape of one stored pixel.
///
/// Preferring `sample_aspect_ratio` because it states the fact directly. Some
/// containers give only `display_aspect_ratio` — what the picture should look
/// like — which the pixel shape can be worked back out of. Failing both, square.
fn pixel_aspect(video: &serde_json::Value, width: usize, height: usize) -> f64 {
    let stated = |key: &str| {
        video
            .get(key)
            .and_then(|v| v.as_str())
            .and_then(parse_rational)
    };
    if let Some(sar) = stated("sample_aspect_ratio") {
        return sar;
    }
    if let Some(dar) = stated("display_aspect_ratio") {
        if width > 0 {
            return dar * height as f64 / width as f64;
        }
    }
    1.0
}

/// Evaluates ffprobe's rationals. Frame rates arrive as `"24000/1001"` and
/// aspect ratios as `"8:9"`, so both separators are accepted.
///
/// `None` for unusable values like `"0/0"`, `"0:1"` or `"N/A"`, so the caller
/// can fall back to something sensible.
fn parse_rational(text: &str) -> Option<f64> {
    let (num, den) = text.split_once(['/', ':'])?;
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
    use std::path::PathBuf;

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
        let json = video_stream("").replace(
            r#""avg_frame_rate":"24/1""#,
            r#""avg_frame_rate":"24000/1001""#,
        );
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
    fn an_unusable_frame_rate_falls_back_to_the_other_field() {
        let json =
            video_stream("").replace(r#""avg_frame_rate":"24/1""#, r#""avg_frame_rate":"0/0""#);
        let info = parse_probe_json(&json).expect("parse");
        assert_eq!(info.fps, 24.0, "should have fallen back to r_frame_rate");
    }

    #[test]
    fn the_average_rate_wins_when_the_two_disagree() {
        // Taken from a real disc rip: r_frame_rate claims a round 30 while the
        // file actually runs at 29.9687. r_frame_rate is the lowest rate that
        // can represent every timestamp, not the rate the film runs at, so
        // trusting it puts frame 5850 nearly six frames early — enough to seek
        // to the wrong shot and to drift the audio over a feature.
        let json = video_stream("")
            .replace(r#""r_frame_rate":"24/1""#, r#""r_frame_rate":"30/1""#)
            .replace(
                r#""avg_frame_rate":"24/1""#,
                r#""avg_frame_rate":"181650000/6061319""#,
            );
        let info = parse_probe_json(&json).expect("parse");
        assert!(
            (info.fps - 29.9687).abs() < 0.001,
            "expected the true 29.9687, got {}",
            info.fps
        );
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
    fn square_pixels_are_assumed_when_nothing_says_otherwise() {
        let info = parse_probe_json(&video_stream("")).expect("parse");
        assert_eq!(info.sample_aspect, 1.0);
        assert!((info.display_aspect() - 16.0 / 9.0).abs() < 1e-6);
    }

    #[test]
    fn non_square_pixels_are_read_and_change_the_display_shape() {
        // Straight from a real transfer: 708x276 stored, 8:9 pixels, so it
        // displays at 2.28:1 rather than the 2.57:1 the stored size implies.
        let json = video_stream(r#","sample_aspect_ratio":"8:9""#)
            .replace(r#""width":1920"#, r#""width":708"#)
            .replace(r#""height":1080"#, r#""height":276"#);
        let info = parse_probe_json(&json).expect("parse");
        assert!(
            (info.sample_aspect - 8.0 / 9.0).abs() < 1e-6,
            "got {}",
            info.sample_aspect
        );
        assert!(
            (info.display_aspect() - 472.0 / 207.0).abs() < 1e-3,
            "expected 2.28:1, got {}",
            info.display_aspect()
        );
    }

    #[test]
    fn a_declared_display_shape_is_used_when_the_pixel_shape_is_missing() {
        // Some containers state only what the picture should look like, not
        // what shape its pixels are. Assuming square in that case gets the
        // whole frame wrong — a 720x480 transfer meant to be seen at 4:3 has
        // 8:9 pixels, and treating them as square makes it 11% too wide.
        let json = video_stream(r#","display_aspect_ratio":"4:3""#)
            .replace(r#""width":1920"#, r#""width":720"#)
            .replace(r#""height":1080"#, r#""height":480"#);
        let info = parse_probe_json(&json).expect("parse");
        assert!(
            (info.sample_aspect - 8.0 / 9.0).abs() < 1e-6,
            "expected 8:9 pixels derived from the 4:3 display shape, got {}",
            info.sample_aspect
        );
        assert!((info.display_aspect() - 4.0 / 3.0).abs() < 1e-6);
    }

    #[test]
    fn a_stated_pixel_shape_wins_over_a_stated_display_shape() {
        // Both present and disagreeing: the pixel shape is the primary fact.
        let json = video_stream(r#","sample_aspect_ratio":"1:1","display_aspect_ratio":"4:3""#);
        let info = parse_probe_json(&json).expect("parse");
        assert_eq!(info.sample_aspect, 1.0);
    }

    #[test]
    fn an_unset_pixel_aspect_falls_back_to_square() {
        for value in [
            r#","sample_aspect_ratio":"0:1""#,
            r#","sample_aspect_ratio":"N/A""#,
        ] {
            let info = parse_probe_json(&video_stream(value)).expect("parse");
            assert_eq!(info.sample_aspect, 1.0, "for {value}");
        }
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
    fn an_implausible_frame_size_is_refused_rather_than_believed() {
        // These numbers come out of the file's own metadata, so a corrupt or
        // hostile container can claim anything — and whatever it claims becomes
        // the size of every buffer the decode allocates.
        let json = video_stream("").replace(r#""width":1920"#, r#""width":4000000000"#);
        assert!(
            parse_probe_json(&json).is_err(),
            "a four-billion-pixel width was taken at face value"
        );
    }

    #[test]
    fn an_eight_k_frame_is_still_accepted() {
        // The bound must sit well above anything a real transfer contains.
        let json = video_stream("")
            .replace(r#""width":1920"#, r#""width":7680"#)
            .replace(r#""height":1080"#, r#""height":4320"#);
        let info = parse_probe_json(&json).expect("8K is a real size");
        assert_eq!((info.width, info.height), (7680, 4320));
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
    fn a_filename_is_never_taken_as_an_ffmpeg_protocol() {
        // ffmpeg resolves protocol prefixes inside input names, so a file whose
        // name begins with one — `cache:`, `concat:`, `http:` — would be fetched
        // through that protocol rather than opened. A name is a name.
        let (tools, _) = crate::locate(None).expect("ffmpeg must be installed");
        let dir = tempfile::tempdir().expect("temp dir");
        let clip = dir.path().join("clip.mkv");
        crate::testing::make_test_clip(&tools, &clip, 64, 48, 5, 10.0);

        let disguised = PathBuf::from(format!("cache:file:{}", clip.display()));
        assert!(
            probe(&tools, &disguised).is_err(),
            "the cache: protocol was resolved instead of looking for a file of that name"
        );
    }

    #[test]
    fn a_path_containing_a_colon_still_probes() {
        // The guard against the fix above overreaching: colons are legal in
        // file names and must keep working.
        let (tools, _) = crate::locate(None).expect("ffmpeg must be installed");
        let dir = tempfile::tempdir().expect("temp dir");
        let clip = dir.path().join("my:film.mkv");
        crate::testing::make_test_clip(&tools, &clip, 64, 48, 5, 10.0);

        assert_eq!(probe(&tools, &clip).expect("probe").width, 64);
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
