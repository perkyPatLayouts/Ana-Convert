// Ana-Convert — anaglyph 3D to full-colour stereo recovery
// SPDX-License-Identifier: GPL-3.0-or-later

//! Writing float frames back out as video.
//!
//! Frames go to ffmpeg as `rgb48le`. The pipeline works in float and the
//! recovered eyes routinely sit in narrow tonal ranges after cross-talk
//! correction and grading, so handing the encoder 8-bit samples would band
//! exactly where the interesting detail is.
//!
//! Audio is stream-copied from a nominated source rather than re-encoded:
//! the conversion has no business touching it.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};

use ana_core::FrameF32;

use crate::{file_arg, FfmpegTools, MediaError};

/// Which encoder to use.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
#[derive(Default)]
pub enum VideoCodec {
    /// Hardware H.264. Fast on Apple Silicon; the sensible default for review
    /// copies and for anything going to a headset.
    #[default]
    H264VideoToolbox,
    /// Hardware HEVC. Better compression, less universally playable.
    HevcVideoToolbox,
    /// Software H.264. Slower, but identical on every platform, which matters
    /// for anything that needs to be reproducible.
    H264,
    /// Software HEVC.
    Hevc,
    /// ProRes HQ, for keeping a master to grade or re-encode later.
    ProRes,
}

/// Which ProRes profile to write.
///
/// The profile is what carries a ProRes file's rate; there is no quality knob
/// beside it. They run from a review copy to a master with an alpha channel.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ProResProfile {
    /// Smallest. For offline review, not for grading.
    Proxy,
    /// Light. Still an editing format, still lossy in ways grading will find.
    Lt,
    /// The middle of the range.
    Standard,
    /// What a master is normally kept at, and the default here.
    #[default]
    Hq,
    /// 4444: higher chroma resolution, and the first profile with alpha.
    Quad,
}

impl ProResProfile {
    pub const ALL: [ProResProfile; 5] =
        [Self::Proxy, Self::Lt, Self::Standard, Self::Hq, Self::Quad];

    /// The number `prores_ks` wants.
    fn number(self) -> u8 {
        match self {
            Self::Proxy => 0,
            Self::Lt => 1,
            Self::Standard => 2,
            Self::Hq => 3,
            Self::Quad => 4,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Proxy => "Proxy",
            Self::Lt => "LT",
            Self::Standard => "Standard",
            Self::Hq => "HQ",
            Self::Quad => "4444",
        }
    }
}

impl VideoCodec {
    /// The ffmpeg encoder name.
    pub fn ffmpeg_name(self) -> &'static str {
        match self {
            Self::H264VideoToolbox => "h264_videotoolbox",
            Self::HevcVideoToolbox => "hevc_videotoolbox",
            Self::H264 => "libx264",
            Self::Hevc => "libx265",
            Self::ProRes => "prores_ks",
        }
    }

    /// True for encoders that only exist on Apple platforms.
    pub fn is_apple_only(self) -> bool {
        matches!(self, Self::H264VideoToolbox | Self::HevcVideoToolbox)
    }

    /// The pixel format to hand the encoder.
    fn output_pix_fmt(self) -> &'static str {
        match self {
            // 10-bit 4:2:2 keeps a ProRes master worth having.
            Self::ProRes => "yuv422p10le",
            _ => "yuv420p",
        }
    }

    /// True where a bitrate is a meaningful thing to ask for.
    ///
    /// ProRes rate comes from the profile, so a bitrate beside it would either
    /// be ignored or quietly argue with it.
    fn takes_a_bitrate(self) -> bool {
        self != Self::ProRes
    }

    /// Quality arguments for a 0..=100 perceptual quality setting.
    fn quality_args(self, quality: u8) -> Vec<String> {
        let quality = quality.min(100);
        match self {
            // CRF runs backwards: lower is better. 100 maps to a visually
            // transparent 14, 0 to a deliberately poor 35.
            Self::H264 | Self::Hevc => {
                let crf = 35 - (f32::from(quality) * 0.21).round() as i32;
                vec!["-crf".into(), crf.to_string()]
            }
            // VideoToolbox takes the same 1..=100 scale we present.
            Self::H264VideoToolbox | Self::HevcVideoToolbox => {
                vec!["-q:v".into(), quality.max(1).to_string()]
            }
            // Quality is carried by the profile, not by a knob beside it.
            Self::ProRes => Vec::new(),
        }
    }
}

/// The arguments describing the video encode, with no input or output among
/// them.
///
/// Built apart from the process that runs them so what reaches ffmpeg can be
/// asserted on directly, the way probing keeps its parsing separate from its
/// process.
fn video_args(settings: &EncodeSettings) -> Vec<String> {
    let codec = settings.codec;
    let mut args = vec!["-c:v".into(), codec.ffmpeg_name().into()];

    // A bitrate and a quality are two ways of asking for the same thing, and
    // sending both leaves the encoder to choose between them. Asking for one
    // means the other is not sent.
    match settings.bitrate_kbps {
        Some(kbps) if codec.takes_a_bitrate() => {
            args.extend(["-b:v".into(), format!("{kbps}k")]);
        }
        _ => args.extend(codec.quality_args(settings.quality)),
    }

    if codec == VideoCodec::ProRes {
        args.extend([
            "-profile:v".into(),
            settings.prores_profile.number().to_string(),
        ]);
    }

    if let Some(frames) = settings.keyframe_interval {
        args.extend(["-g".into(), frames.to_string()]);
    }

    args.extend(["-pix_fmt".into(), codec.output_pix_fmt().into()]);
    args
}

/// How to write the output file.
#[derive(Debug, Clone, PartialEq)]
pub struct EncodeSettings {
    pub codec: VideoCodec,
    /// Perceptual quality, 0..=100. Ignored when a bitrate is set.
    pub quality: u8,
    /// A fixed video bitrate in kbit/s, for delivery to something that needs a
    /// known size. `None` — the default — lets quality decide, which is the
    /// better answer whenever the size is not the constraint.
    pub bitrate_kbps: Option<u32>,
    /// Frames between keyframes. `None` leaves the encoder's own choice alone.
    /// Shorter means more seekable and larger.
    pub keyframe_interval: Option<u32>,
    /// Which ProRes profile to write. Ignored by every other codec.
    pub prores_profile: ProResProfile,
    pub fps: f64,
    /// File to copy an audio track from, if any.
    pub audio_from: Option<PathBuf>,
    /// The shape the finished frame should be seen at.
    ///
    /// Raw frames carry no pixel-aspect information, so without this ffmpeg
    /// assumes square pixels and anything from a non-square source — most
    /// disc transfers — comes out stretched. `None` means square.
    pub display_aspect: Option<f64>,
}

impl Default for EncodeSettings {
    fn default() -> Self {
        Self {
            codec: VideoCodec::default(),
            quality: 75,
            bitrate_kbps: None,
            keyframe_interval: None,
            prores_profile: ProResProfile::default(),
            fps: 24.0,
            audio_from: None,
            display_aspect: None,
        }
    }
}

/// Writes frames into a video file.
///
/// Call [`Encoder::finish`] to close the stream and check the result. Dropping
/// without finishing aborts the encode and kills the child process.
pub struct Encoder {
    child: Option<Child>,
    stdin: Option<ChildStdin>,
    path: PathBuf,
    width: usize,
    height: usize,
    frames_written: u64,
}

impl Encoder {
    /// Starts an encoder writing to `path`.
    pub fn create(
        tools: &FfmpegTools,
        path: &Path,
        width: usize,
        height: usize,
        settings: &EncodeSettings,
    ) -> Result<Self, MediaError> {
        let mut command = Command::new(&tools.ffmpeg);
        command.args([
            "-y",
            "-v",
            "error",
            "-f",
            "rawvideo",
            "-pix_fmt",
            "rgb48le",
            "-s",
            &format!("{width}x{height}"),
            "-r",
            &format!("{}", settings.fps),
            "-i",
            "-",
        ]);

        if let Some(audio) = &settings.audio_from {
            command.arg("-i").arg(file_arg(audio)).args([
                "-map",
                "0:v:0",
                "-map",
                "1:a:0",
                "-c:a",
                "copy",
                // The audio source is usually the full-length original, so
                // stop when the shorter of the two runs out.
                "-shortest",
            ]);
        } else {
            command.args(["-map", "0:v:0"]);
        }

        if let Some(aspect) = settings.display_aspect {
            if aspect.is_finite() && aspect > 0.0 {
                command.args(["-aspect", &format!("{aspect:.10}")]);
            }
        }

        command.args(video_args(settings)).arg(file_arg(path));

        let mut child = command
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|source| MediaError::Io {
                path: tools.ffmpeg.clone(),
                source,
            })?;

        let stdin = child.stdin.take();
        Ok(Self {
            child: Some(child),
            stdin,
            path: path.to_path_buf(),
            width,
            height,
            frames_written: 0,
        })
    }

    /// Writes one frame. Its geometry must match what the encoder was opened for.
    pub fn write_frame(&mut self, frame: &FrameF32) -> Result<(), MediaError> {
        if frame.width() != self.width || frame.height() != self.height {
            return Err(MediaError::FrameSizeMismatch {
                expected: (self.width, self.height),
                got: (frame.width(), frame.height()),
            });
        }

        let samples = frame.to_rgb16();
        let mut bytes = Vec::with_capacity(samples.len() * 2);
        for s in samples {
            bytes.extend_from_slice(&s.to_le_bytes());
        }

        let stdin = self.stdin.as_mut().ok_or_else(|| MediaError::ToolFailed {
            tool: "ffmpeg",
            message: "the encoder has already been finished".into(),
        })?;

        stdin.write_all(&bytes).map_err(|source| MediaError::Io {
            path: self.path.clone(),
            source,
        })?;
        self.frames_written += 1;
        Ok(())
    }

    /// How many frames have been written.
    pub fn frames_written(&self) -> u64 {
        self.frames_written
    }

    /// Closes the stream and waits for ffmpeg to finish writing the file.
    pub fn finish(mut self) -> Result<(), MediaError> {
        // Closing stdin is what tells ffmpeg the stream is complete; without it
        // the wait below would block forever.
        drop(self.stdin.take());

        let Some(child) = self.child.take() else {
            return Ok(());
        };
        let out = child.wait_with_output().map_err(|source| MediaError::Io {
            path: self.path.clone(),
            source,
        })?;

        if out.status.success() {
            Ok(())
        } else {
            Err(MediaError::ToolFailed {
                tool: "ffmpeg",
                message: String::from_utf8_lossy(&out.stderr).trim().to_string(),
            })
        }
    }
}

impl Drop for Encoder {
    fn drop(&mut self) {
        drop(self.stdin.take());
        if let Some(mut child) = self.child.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::probe::probe;
    use crate::testing::make_test_clip;

    fn tools() -> FfmpegTools {
        crate::locate(None).expect("ffmpeg must be installed").0
    }

    fn solid(width: usize, height: usize, rgb: [f32; 3]) -> FrameF32 {
        FrameF32::from_rgb_planes(
            width,
            height,
            &vec![rgb[0]; width * height],
            &vec![rgb[1]; width * height],
            &vec![rgb[2]; width * height],
        )
    }

    /// Software H.264 so results do not depend on the hardware encoder.
    fn settings() -> EncodeSettings {
        EncodeSettings {
            codec: VideoCodec::H264,
            fps: 10.0,
            ..Default::default()
        }
    }

    /// The arguments as one string, for asserting on what reaches ffmpeg.
    fn args_for(settings: &EncodeSettings) -> String {
        video_args(settings).join(" ")
    }

    #[test]
    fn quality_is_the_default_rate_control() {
        let args = args_for(&EncodeSettings {
            codec: VideoCodec::H264,
            quality: 100,
            ..Default::default()
        });
        assert!(args.contains("-crf 14"), "got {args}");
        assert!(
            !args.contains("-b:v"),
            "a bitrate was set without being asked for"
        );
    }

    #[test]
    fn a_bitrate_replaces_the_quality_setting() {
        // The two are different ways of asking for the same thing, and passing
        // both lets the encoder pick — so choosing one has to mean the other
        // is not sent at all.
        let args = args_for(&EncodeSettings {
            codec: VideoCodec::H264,
            bitrate_kbps: Some(4500),
            ..Default::default()
        });
        assert!(args.contains("-b:v 4500k"), "got {args}");
        assert!(
            !args.contains("-crf"),
            "quality was sent alongside a bitrate: {args}"
        );
    }

    #[test]
    fn a_keyframe_interval_reaches_ffmpeg() {
        let args = args_for(&EncodeSettings {
            keyframe_interval: Some(48),
            ..Default::default()
        });
        assert!(args.contains("-g 48"), "got {args}");
    }

    #[test]
    fn no_keyframe_interval_leaves_the_encoder_to_decide() {
        assert!(!args_for(&EncodeSettings::default()).contains("-g "));
    }

    #[test]
    fn the_prores_profile_is_chosen_rather_than_fixed() {
        for (profile, number) in [
            (ProResProfile::Proxy, "0"),
            (ProResProfile::Lt, "1"),
            (ProResProfile::Standard, "2"),
            (ProResProfile::Hq, "3"),
            (ProResProfile::Quad, "4"),
        ] {
            let args = args_for(&EncodeSettings {
                codec: VideoCodec::ProRes,
                prores_profile: profile,
                ..Default::default()
            });
            assert!(
                args.contains(&format!("-profile:v {number}")),
                "{profile:?} should be profile {number}, got {args}"
            );
        }
    }

    #[test]
    fn prores_ignores_a_bitrate() {
        // ProRes rate is carried by the profile. Sending a bitrate as well
        // would either be ignored or quietly fight the profile.
        let args = args_for(&EncodeSettings {
            codec: VideoCodec::ProRes,
            bitrate_kbps: Some(4500),
            ..Default::default()
        });
        assert!(!args.contains("-b:v"), "got {args}");
    }

    #[test]
    fn the_defaults_are_what_the_app_shipped_with() {
        // The dialog exists to be ignored: someone who never opens it must get
        // exactly the encode they got before it was added.
        let args = args_for(&EncodeSettings::default());
        assert!(args.contains("-c:v h264_videotoolbox"), "got {args}");
        assert!(args.contains("-q:v 75"), "got {args}");
        assert!(args.contains("-pix_fmt yuv420p"), "got {args}");
    }

    #[test]
    fn crf_runs_backwards_from_the_quality_scale() {
        let best = VideoCodec::H264.quality_args(100);
        let worst = VideoCodec::H264.quality_args(0);
        assert_eq!(best, vec!["-crf".to_string(), "14".to_string()]);
        assert_eq!(worst, vec!["-crf".to_string(), "35".to_string()]);
    }

    #[test]
    fn videotoolbox_takes_the_quality_scale_directly() {
        assert_eq!(
            VideoCodec::H264VideoToolbox.quality_args(80),
            vec!["-q:v".to_string(), "80".to_string()]
        );
    }

    #[test]
    fn an_encoded_file_has_the_frames_and_size_it_was_given() {
        let t = tools();
        let dir = tempfile::tempdir().expect("temp dir");
        let out = dir.path().join("out.mp4");

        let mut encoder = Encoder::create(&t, &out, 64, 48, &settings()).expect("create");
        for i in 0..12 {
            let v = i as f32 / 12.0;
            encoder
                .write_frame(&solid(64, 48, [v, 0.5, 1.0 - v]))
                .expect("write");
        }
        assert_eq!(encoder.frames_written(), 12);
        encoder.finish().expect("finish");

        let info = probe(&t, &out).expect("probe the result");
        assert_eq!((info.width, info.height), (64, 48));
        assert_eq!(info.estimated_frame_count(), Some(12));
    }

    #[test]
    fn colours_survive_the_round_trip() {
        // Not a codec quality test — this catches channel swaps and byte-order
        // mistakes, which would show up as wildly wrong colour, not slight loss.
        let t = tools();
        let dir = tempfile::tempdir().expect("temp dir");
        let out = dir.path().join("out.mkv");

        let mut encoder = Encoder::create(&t, &out, 32, 32, &settings()).expect("create");
        for _ in 0..4 {
            encoder
                .write_frame(&solid(32, 32, [0.8, 0.2, 0.4]))
                .expect("write");
        }
        encoder.finish().expect("finish");

        let info = probe(&t, &out).expect("probe");
        let frame = crate::decode::grab_frame(&t, &out, &info, 1).expect("decode back");
        let (r, g, b) = frame.rgb_planes();
        assert!((r[0] - 0.8).abs() < 0.05, "red came back as {}", r[0]);
        assert!((g[0] - 0.2).abs() < 0.05, "green came back as {}", g[0]);
        assert!((b[0] - 0.4).abs() < 0.05, "blue came back as {}", b[0]);
    }

    #[test]
    fn audio_is_carried_over_from_the_nominated_source() {
        let t = tools();
        let dir = tempfile::tempdir().expect("temp dir");
        let source = dir.path().join("source.mp4");
        let out = dir.path().join("out.mp4");
        make_test_clip(&t, &source, 32, 32, 10, 10.0);

        let mut encoder = Encoder::create(
            &t,
            &out,
            32,
            32,
            &EncodeSettings {
                audio_from: Some(source.clone()),
                ..settings()
            },
        )
        .expect("create");
        for _ in 0..10 {
            encoder
                .write_frame(&solid(32, 32, [0.5, 0.5, 0.5]))
                .expect("write");
        }
        encoder.finish().expect("finish");

        assert!(
            probe(&t, &out).expect("probe").has_audio,
            "audio was dropped"
        );
    }

    #[test]
    fn a_display_aspect_reaches_the_written_file() {
        // The whole point: a 2:1 frame written from 128x128 pixels must be
        // marked as 2:1, not left for a player to assume square.
        let t = tools();
        let dir = tempfile::tempdir().expect("temp dir");
        let out = dir.path().join("wide.mkv");

        let mut encoder = Encoder::create(
            &t,
            &out,
            128,
            128,
            &EncodeSettings {
                display_aspect: Some(2.0),
                ..settings()
            },
        )
        .expect("create");
        for _ in 0..3 {
            encoder
                .write_frame(&solid(128, 128, [0.5, 0.5, 0.5]))
                .expect("write");
        }
        encoder.finish().expect("finish");

        let info = probe(&t, &out).expect("probe");
        assert_eq!(
            (info.width, info.height),
            (128, 128),
            "stored size is unchanged"
        );
        assert!(
            (info.display_aspect() - 2.0).abs() < 0.01,
            "expected a 2:1 display shape, got {}",
            info.display_aspect()
        );
    }

    #[test]
    fn no_display_aspect_leaves_square_pixels() {
        let t = tools();
        let dir = tempfile::tempdir().expect("temp dir");
        let out = dir.path().join("square.mkv");

        let mut encoder = Encoder::create(&t, &out, 64, 32, &settings()).expect("create");
        encoder
            .write_frame(&solid(64, 32, [0.5, 0.5, 0.5]))
            .expect("write");
        encoder.finish().expect("finish");

        let info = probe(&t, &out).expect("probe");
        assert!(
            (info.sample_aspect - 1.0).abs() < 1e-6,
            "got {}",
            info.sample_aspect
        );
    }

    #[test]
    fn no_audio_source_means_no_audio_track() {
        let t = tools();
        let dir = tempfile::tempdir().expect("temp dir");
        let out = dir.path().join("out.mp4");

        let mut encoder = Encoder::create(&t, &out, 32, 32, &settings()).expect("create");
        encoder
            .write_frame(&solid(32, 32, [0.5, 0.5, 0.5]))
            .expect("write");
        encoder.finish().expect("finish");

        assert!(!probe(&t, &out).expect("probe").has_audio);
    }

    #[test]
    fn an_audio_source_is_never_taken_as_an_ffmpeg_protocol() {
        // The audio source is an ffmpeg input like any other, so it needs the
        // same protection from protocol prefixes as the video does.
        let t = tools();
        let dir = tempfile::tempdir().expect("temp dir");
        let source = dir.path().join("source.mp4");
        let out = dir.path().join("out.mp4");
        make_test_clip(&t, &source, 32, 32, 10, 10.0);

        let disguised = PathBuf::from(format!("cache:file:{}", source.display()));
        let mut encoder = Encoder::create(
            &t,
            &out,
            32,
            32,
            &EncodeSettings {
                audio_from: Some(disguised),
                ..settings()
            },
        )
        .expect("create");
        let wrote = encoder.write_frame(&solid(32, 32, [0.5, 0.5, 0.5]));
        assert!(
            wrote.is_err() || encoder.finish().is_err(),
            "the cache: protocol was resolved instead of looking for a file of that name"
        );
    }

    #[test]
    fn a_path_containing_a_colon_can_still_be_written() {
        let t = tools();
        let dir = tempfile::tempdir().expect("temp dir");
        let out = dir.path().join("my:film.mkv");

        let mut encoder = Encoder::create(&t, &out, 32, 32, &settings()).expect("create");
        encoder
            .write_frame(&solid(32, 32, [0.5, 0.5, 0.5]))
            .expect("write");
        encoder.finish().expect("finish");

        assert!(out.exists(), "{} was not written", out.display());
    }

    #[test]
    fn every_prores_profile_is_one_ffmpeg_accepts() {
        // The argument tests prove which number is sent, not that the number
        // means anything. A profile ffmpeg rejects fails the whole encode.
        let t = tools();
        let dir = tempfile::tempdir().expect("temp dir");
        for profile in ProResProfile::ALL {
            let out = dir.path().join(format!("{}.mov", profile.label()));
            let mut encoder = Encoder::create(
                &t,
                &out,
                64,
                48,
                &EncodeSettings {
                    codec: VideoCodec::ProRes,
                    prores_profile: profile,
                    ..settings()
                },
            )
            .expect("create");
            encoder
                .write_frame(&solid(64, 48, [0.4, 0.6, 0.8]))
                .expect("write");
            encoder
                .finish()
                .unwrap_or_else(|e| panic!("{} was refused: {e}", profile.label()));
            assert!(out.exists(), "{} wrote nothing", profile.label());
        }
    }

    #[test]
    fn a_bitrate_actually_changes_the_size_of_the_file() {
        // That the flag reaches ffmpeg is one thing; that ffmpeg does something
        // with it is another.
        let t = tools();
        let dir = tempfile::tempdir().expect("temp dir");
        let mut sizes = Vec::new();
        for (name, kbps) in [("low.mp4", 100u32), ("high.mp4", 8000)] {
            let out = dir.path().join(name);
            let mut encoder = Encoder::create(
                &t,
                &out,
                320,
                240,
                &EncodeSettings {
                    codec: VideoCodec::H264,
                    bitrate_kbps: Some(kbps),
                    ..settings()
                },
            )
            .expect("create");
            for i in 0..30 {
                let v = (i % 7) as f32 / 7.0;
                encoder
                    .write_frame(&solid(320, 240, [v, 1.0 - v, 0.5]))
                    .expect("write");
            }
            encoder.finish().expect("finish");
            sizes.push(out.metadata().expect("size").len());
        }
        assert!(
            sizes[0] < sizes[1],
            "100 kbit/s produced {} bytes and 8000 kbit/s produced {} — the \
             bitrate is not reaching the encoder",
            sizes[0],
            sizes[1]
        );
    }

    #[test]
    fn a_frame_of_the_wrong_size_is_rejected() {
        // Better to fail here than to hand ffmpeg a misaligned stream and get
        // a file full of sheared frames.
        let t = tools();
        let dir = tempfile::tempdir().expect("temp dir");
        let out = dir.path().join("out.mp4");

        let mut encoder = Encoder::create(&t, &out, 64, 48, &settings()).expect("create");
        let err = encoder.write_frame(&solid(32, 32, [0.5, 0.5, 0.5]));
        assert!(err.is_err(), "a mismatched frame must be refused");
    }

    #[test]
    fn hevc_produces_an_hevc_stream() {
        let t = tools();
        let dir = tempfile::tempdir().expect("temp dir");
        let out = dir.path().join("out.mp4");

        let mut encoder = Encoder::create(
            &t,
            &out,
            32,
            32,
            &EncodeSettings {
                codec: VideoCodec::Hevc,
                ..settings()
            },
        )
        .expect("create");
        for _ in 0..4 {
            encoder
                .write_frame(&solid(32, 32, [0.3, 0.6, 0.9]))
                .expect("write");
        }
        encoder.finish().expect("finish");

        let out_json = std::process::Command::new(&t.ffprobe)
            .args(["-v", "error", "-show_streams", "-of", "json"])
            .arg(&out)
            .output()
            .expect("ffprobe");
        let text = String::from_utf8_lossy(&out_json.stdout);
        assert!(
            text.contains("hevc"),
            "expected an hevc stream, got: {text}"
        );
    }

    #[test]
    fn dropping_without_finishing_does_not_hang() {
        let t = tools();
        let dir = tempfile::tempdir().expect("temp dir");
        let out = dir.path().join("abandoned.mp4");

        let mut encoder = Encoder::create(&t, &out, 32, 32, &settings()).expect("create");
        encoder
            .write_frame(&solid(32, 32, [0.5, 0.5, 0.5]))
            .expect("write");
        drop(encoder);
        // Reaching this line at all is the assertion: a cancelled render must
        // not leave the app waiting on an orphaned ffmpeg.
    }

    #[test]
    fn writing_to_an_unwritable_path_fails_on_finish() {
        let t = tools();
        let out = Path::new("/definitely/not/a/directory/out.mp4");
        let mut encoder = Encoder::create(&t, out, 32, 32, &settings()).expect("spawn succeeds");
        // ffmpeg exits early, so the write may fail on a broken pipe or the
        // failure may only surface when the exit status is checked.
        let wrote = encoder.write_frame(&solid(32, 32, [0.5, 0.5, 0.5]));
        assert!(
            wrote.is_err() || encoder.finish().is_err(),
            "an unwritable destination must be reported somewhere"
        );
    }
}
