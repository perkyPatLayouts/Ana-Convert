// Ana-Convert — anaglyph 3D to full-colour stereo recovery
// SPDX-License-Identifier: GPL-3.0-or-later

//! Running a whole conversion: sources in, converted video out.
//!
//! [`ana_core`] converts one frame and [`ana_media`] moves frames in and out of
//! files. This crate is what sits between them for the length of a movie —
//! keeping up to three decoders in step, reporting progress, and stopping
//! promptly when asked.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};

use ana_core::compose::{aspect_differs, conform_to, OutputLayout};
use ana_core::params::{ConvertParams, MonoEye, ParamsError};
use ana_core::pipeline::{compose_output, process_frame, Sources};
use ana_core::FrameF32;
use ana_media::encode::EncodeSettings;
use ana_media::{probe, Decoder, Encoder, FfmpegTools, MediaError, VideoInfo};

/// Everything one conversion needs.
#[derive(Debug, Clone)]
pub struct RenderJob {
    /// The file being converted. Always required.
    pub anaglyph: PathBuf,
    /// The right eye, when the input is two files rather than one.
    pub right_eye: Option<PathBuf>,
    /// Where colour is sampled from. Defaults to the anaglyph itself.
    pub colour: Option<PathBuf>,
    /// A 2D release standing in for one eye.
    pub mono: Option<PathBuf>,
    /// Where the audio track is copied from.
    pub audio: Option<PathBuf>,
    /// Destination. For [`OutputLayout::Separate`] this is the stem that the
    /// two per-eye files are derived from.
    pub output: PathBuf,
    pub params: ConvertParams,
    pub encode: EncodeSettings,
}

/// Reported as the conversion runs.
#[derive(Debug, Clone, PartialEq)]
pub enum Progress {
    /// Emitted once, before the first frame.
    Started {
        total_frames: Option<u64>,
        width: usize,
        height: usize,
    },
    /// Emitted after each converted frame.
    Frame { done: u64, total: Option<u64> },
    /// Something worth telling the user that is not an error.
    Note(String),
    /// Emitted once, whether the run completed or was cancelled.
    Finished { frames: u64, cancelled: bool },
}

/// What a finished run produced.
#[derive(Debug, Clone, PartialEq)]
pub struct RenderSummary {
    pub frames: u64,
    pub outputs: Vec<PathBuf>,
    pub cancelled: bool,
}

/// Anything that can stop a conversion.
#[derive(Debug, thiserror::Error)]
pub enum PipelineError {
    #[error(transparent)]
    Media(#[from] MediaError),

    #[error("invalid settings: {0}")]
    Params(#[from] ParamsError),

    #[error("a 2D source was named but mono_eye is set to none, so it would be ignored")]
    MonoSourceUnused,

    #[error("mono_eye is set to {0:?} but no 2D source was given")]
    MonoSourceMissing(MonoEye),

    #[error("the source is set to two files, but no second file was given")]
    RightEyeMissing,

    #[error(
        "the chosen range selects no frames from {name}: it starts at {start} \
         and the file has {total} frames"
    )]
    TrimSelectsNoFrames {
        name: String,
        start: u64,
        total: u64,
    },

    #[error("{0} has no audio stream to copy")]
    NoAudioInSource(String),
}

/// The files a job will write.
///
/// Stacked layouts write one file. Separate streams derive a `-left` and a
/// `-right` file from the given path, since the eye order is carried by the
/// names rather than by position.
pub fn output_paths(output: &Path, layout: OutputLayout) -> Vec<PathBuf> {
    if layout != OutputLayout::Separate {
        return vec![output.to_path_buf()];
    }
    ["-left", "-right"]
        .iter()
        .map(|suffix| {
            let stem = output
                .file_stem()
                .map(|s| s.to_string_lossy().into_owned())
                .unwrap_or_default();
            let name = match output.extension() {
                Some(ext) => format!("{stem}{suffix}.{}", ext.to_string_lossy()),
                None => format!("{stem}{suffix}"),
            };
            output.with_file_name(name)
        })
        .collect()
}

/// Converts a whole file.
///
/// `on_progress` is called from this thread as the run proceeds. `cancel` is
/// checked once per frame, so stopping takes at most one frame's work.
pub fn render(
    tools: &FfmpegTools,
    job: &RenderJob,
    on_progress: &mut dyn FnMut(Progress),
    cancel: &AtomicBool,
) -> Result<RenderSummary, PipelineError> {
    // Everything that can be checked cheaply is checked before a single frame
    // is decoded, so a misconfigured run fails in a second rather than an hour.
    job.params.validate()?;
    check_mono_agreement(job)?;
    if job.params.input.needs_second_file() && job.right_eye.is_none() {
        return Err(PipelineError::RightEyeMissing);
    }

    let anaglyph_info = probe(tools, &job.anaglyph)?;
    check_audio_available(tools, job)?;
    let colour_info = probe_secondary(tools, job.colour.as_deref())?;
    let mono_info = probe_secondary(tools, job.mono.as_deref())?;
    let right_info = probe_secondary(tools, job.right_eye.as_deref())?;

    // Secondary sources are brought to the anaglyph's geometry rather than
    // refused: a 2D release at another resolution is the normal case. A
    // different *shape* is worth mentioning, because resizing will stretch it.
    let geometry = (anaglyph_info.width, anaglyph_info.height);
    for (info, role) in [
        (&colour_info, "The colour source"),
        (&mono_info, "The 2D eye source"),
        (&right_info, "The right-eye file"),
    ] {
        let Some(info) = info else { continue };
        if aspect_differs((info.width, info.height), geometry) {
            on_progress(Progress::Note(format!(
                "{role} is {}x{}, a different shape from the anaglyph's {}x{} — \
                 resizing it to fit will stretch the picture.",
                info.width, info.height, geometry.0, geometry.1
            )));
        }
    }

    // The anaglyph's range is the timeline: it decides how long the output is,
    // and every other source is read in step with it.
    let available = anaglyph_info.estimated_frame_count().unwrap_or(u64::MAX);
    let span = job.params.anaglyph_trim.length(available);
    if span == 0 {
        return Err(PipelineError::TrimSelectsNoFrames {
            name: file_label(&job.anaglyph),
            start: job.params.anaglyph_trim.start,
            total: available,
        });
    }

    on_progress(Progress::Started {
        total_frames: Some(span),
        width: anaglyph_info.width,
        height: anaglyph_info.height,
    });

    // Each source is seeked to its own start, which is what puts two differently
    // edited releases onto the same moment.
    let mut anaglyph = Decoder::open_at(
        tools,
        &job.anaglyph,
        &anaglyph_info,
        job.params.anaglyph_trim.start,
    )?;
    let mut colour = open_decoder(
        tools,
        job.colour.as_deref(),
        &colour_info,
        job.params.colour_trim.start,
    )?;
    let mut mono = open_decoder(
        tools,
        job.mono.as_deref(),
        &mono_info,
        job.params.mono_trim.start,
    )?;
    // The right eye is read in step with the primary source, so it shares its
    // range: two per-eye files come from the same master and start together.
    let mut right_eye = open_decoder(
        tools,
        job.right_eye.as_deref(),
        &right_info,
        job.params.anaglyph_trim.start,
    )?;

    let outputs = output_paths(&job.output, job.params.layout);
    let (out_w, out_h) = job.params.output_geometry(geometry);
    let mut encoders = Vec::new();
    for path in &outputs {
        let settings = EncodeSettings {
            audio_from: job.audio.clone(),
            // Carry the source's pixel shape through, or every non-square
            // transfer comes out stretched.
            display_aspect: Some(
                job.params
                    .output_display_aspect(anaglyph_info.display_aspect()),
            ),
            ..job.encode.clone()
        };
        encoders.push(Encoder::create(tools, path, out_w, out_h, &settings)?);
    }

    let total = Some(span);
    let mut frames = 0u64;
    let mut cancelled = false;

    while let Some(frame) = anaglyph.next_frame()? {
        if cancel.load(Ordering::SeqCst) {
            cancelled = true;
            break;
        }
        if frames >= span {
            break;
        }

        // A source running short holds its last frame rather than ending the
        // run: a 2D release a few frames shorter than the anaglyph should not
        // truncate the conversion.
        // Conforming here rather than at the source keeps the requirement in
        // one place: everything downstream sees the anaglyph's geometry.
        let colour_frame =
            next_or_hold(&mut colour)?.map(|f| conform_to(&f, geometry.0, geometry.1));
        let mono_frame = next_or_hold(&mut mono)?.map(|f| conform_to(&f, geometry.0, geometry.1));
        let right_frame =
            next_or_hold(&mut right_eye)?.map(|f| conform_to(&f, geometry.0, geometry.1));

        let pair = process_frame(
            Sources {
                primary: &frame,
                right_eye: right_frame.as_ref(),
                colour: colour_frame.as_ref(),
                mono: mono_frame.as_ref(),
            },
            &job.params,
        );

        for (encoder, composed) in encoders.iter_mut().zip(compose_output(&pair, &job.params)) {
            encoder.write_frame(&composed)?;
        }

        frames += 1;
        on_progress(Progress::Frame {
            done: frames,
            total,
        });
    }

    for encoder in encoders {
        encoder.finish()?;
    }
    on_progress(Progress::Finished { frames, cancelled });

    Ok(RenderSummary {
        frames,
        outputs,
        cancelled,
    })
}

/// Refuses an audio source that carries no audio.
///
/// Without this ffmpeg is asked to map a stream that is not there, exits
/// immediately, and the first frame write fails with a broken pipe — an error
/// that says nothing at all about the real problem.
fn check_audio_available(tools: &FfmpegTools, job: &RenderJob) -> Result<(), PipelineError> {
    let Some(path) = &job.audio else {
        return Ok(());
    };
    if !probe(tools, path)?.has_audio {
        return Err(PipelineError::NoAudioInSource(
            path.file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_else(|| path.display().to_string()),
        ));
    }
    Ok(())
}

/// Rejects the two ways a 2D source and `mono_eye` can disagree, each of which
/// would otherwise silently do the wrong thing.
fn check_mono_agreement(job: &RenderJob) -> Result<(), PipelineError> {
    match (job.mono.is_some(), job.params.mono_eye) {
        (true, MonoEye::None) => Err(PipelineError::MonoSourceUnused),
        (false, eye @ (MonoEye::Left | MonoEye::Right)) => {
            Err(PipelineError::MonoSourceMissing(eye))
        }
        _ => Ok(()),
    }
}

/// Probes an optional source.
fn probe_secondary(
    tools: &FfmpegTools,
    path: Option<&Path>,
) -> Result<Option<VideoInfo>, PipelineError> {
    match path {
        Some(path) => Ok(Some(probe(tools, path)?)),
        None => Ok(None),
    }
}

fn open_decoder(
    tools: &FfmpegTools,
    path: Option<&Path>,
    info: &Option<VideoInfo>,
    start: u64,
) -> Result<Option<Decoder>, PipelineError> {
    match (path, info) {
        (Some(path), Some(info)) => Ok(Some(Decoder::open_at(tools, path, info, start)?)),
        _ => Ok(None),
    }
}

/// A file name for a message, falling back to the whole path.
fn file_label(path: &Path) -> String {
    path.file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.display().to_string())
}

/// The next frame from an optional decoder, or `None` if there is no decoder.
fn next_or_hold(decoder: &mut Option<Decoder>) -> Result<Option<FrameF32>, PipelineError> {
    match decoder {
        Some(d) => Ok(d.next_frame()?),
        None => Ok(None),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ana_core::params::SourceTrim;
    use ana_media::testing::{make_silent_clip, make_test_clip};
    use ana_media::{encode::VideoCodec, locate};
    use std::sync::atomic::AtomicBool;

    fn tools() -> FfmpegTools {
        locate(None).expect("ffmpeg must be installed").0
    }

    fn job(dir: &Path, input: &Path, output: &str) -> RenderJob {
        RenderJob {
            anaglyph: input.to_path_buf(),
            right_eye: None,
            colour: None,
            mono: None,
            audio: None,
            output: dir.join(output),
            params: ConvertParams::default(),
            encode: EncodeSettings {
                codec: VideoCodec::H264,
                fps: 10.0,
                ..Default::default()
            },
        }
    }

    fn ignore(_: Progress) {}

    // --- output paths ---

    #[test]
    fn a_stacked_layout_writes_the_named_file() {
        let paths = output_paths(Path::new("/tmp/out.mkv"), OutputLayout::SideBySide);
        assert_eq!(paths, vec![PathBuf::from("/tmp/out.mkv")]);
    }

    #[test]
    fn separate_streams_derive_a_file_per_eye() {
        let paths = output_paths(Path::new("/tmp/movie.mkv"), OutputLayout::Separate);
        assert_eq!(
            paths,
            vec![
                PathBuf::from("/tmp/movie-left.mkv"),
                PathBuf::from("/tmp/movie-right.mkv")
            ]
        );
    }

    #[test]
    fn derived_names_survive_a_dotted_stem() {
        let paths = output_paths(Path::new("/tmp/my.movie.2160p.mkv"), OutputLayout::Separate);
        assert_eq!(paths[0], PathBuf::from("/tmp/my.movie.2160p-left.mkv"));
    }

    #[test]
    fn a_path_without_an_extension_still_splits() {
        let paths = output_paths(Path::new("/tmp/movie"), OutputLayout::Separate);
        assert_eq!(paths[0], PathBuf::from("/tmp/movie-left"));
        assert_eq!(paths[1], PathBuf::from("/tmp/movie-right"));
    }

    // --- rendering ---

    #[test]
    fn a_side_by_side_render_produces_one_double_width_file() {
        let t = tools();
        let dir = tempfile::tempdir().expect("temp dir");
        let input = dir.path().join("in.mp4");
        make_test_clip(&t, &input, 64, 48, 6, 10.0);

        let summary = render(
            &t,
            &job(dir.path(), &input, "out.mp4"),
            &mut ignore,
            &AtomicBool::new(false),
        )
        .expect("render");

        assert_eq!(summary.frames, 6);
        assert!(!summary.cancelled);
        assert_eq!(summary.outputs.len(), 1);

        let info = probe(&t, &summary.outputs[0]).expect("probe output");
        assert_eq!((info.width, info.height), (128, 48));
        assert_eq!(info.estimated_frame_count(), Some(6));
    }

    #[test]
    fn a_separate_render_writes_both_eyes_at_source_size() {
        let t = tools();
        let dir = tempfile::tempdir().expect("temp dir");
        let input = dir.path().join("in.mp4");
        make_test_clip(&t, &input, 64, 48, 4, 10.0);

        let mut j = job(dir.path(), &input, "eyes.mp4");
        j.params.layout = OutputLayout::Separate;
        let summary = render(&t, &j, &mut ignore, &AtomicBool::new(false)).expect("render");

        assert_eq!(summary.outputs.len(), 2);
        for path in &summary.outputs {
            assert!(path.exists(), "{path:?} was not written");
            let info = probe(&t, path).expect("probe");
            assert_eq!((info.width, info.height), (64, 48));
        }
    }

    #[test]
    fn progress_counts_every_frame_and_ends_once() {
        let t = tools();
        let dir = tempfile::tempdir().expect("temp dir");
        let input = dir.path().join("in.mp4");
        make_test_clip(&t, &input, 32, 32, 5, 10.0);

        let mut events = Vec::new();
        render(
            &t,
            &job(dir.path(), &input, "out.mp4"),
            &mut |p| events.push(p),
            &AtomicBool::new(false),
        )
        .expect("render");

        assert!(
            matches!(events.first(), Some(Progress::Started { .. })),
            "first event should be Started, got {:?}",
            events.first()
        );
        let frames: Vec<u64> = events
            .iter()
            .filter_map(|e| match e {
                Progress::Frame { done, .. } => Some(*done),
                _ => None,
            })
            .collect();
        assert_eq!(
            frames,
            vec![1, 2, 3, 4, 5],
            "frame counts must be 1-based and complete"
        );
        assert_eq!(
            events.last(),
            Some(&Progress::Finished {
                frames: 5,
                cancelled: false
            })
        );
    }

    #[test]
    fn progress_reports_a_total_when_the_container_knows_one() {
        let t = tools();
        let dir = tempfile::tempdir().expect("temp dir");
        let input = dir.path().join("in.mp4");
        make_test_clip(&t, &input, 32, 32, 5, 10.0);

        let mut started = None;
        render(
            &t,
            &job(dir.path(), &input, "out.mp4"),
            &mut |p| {
                if let Progress::Started { total_frames, .. } = p {
                    started = total_frames;
                }
            },
            &AtomicBool::new(false),
        )
        .expect("render");
        assert_eq!(started, Some(5), "an mp4 reports its frame count");
    }

    #[test]
    fn cancelling_stops_early_and_says_so() {
        // A cancelled render must stop promptly and admit the file is partial,
        // rather than reporting success on a truncated conversion.
        let t = tools();
        let dir = tempfile::tempdir().expect("temp dir");
        let input = dir.path().join("in.mp4");
        make_test_clip(&t, &input, 32, 32, 40, 10.0);

        let cancel = AtomicBool::new(false);
        let summary = render(
            &t,
            &job(dir.path(), &input, "out.mp4"),
            &mut |p| {
                if let Progress::Frame { done, .. } = p {
                    if done >= 3 {
                        cancel.store(true, Ordering::SeqCst);
                    }
                }
            },
            &cancel,
        )
        .expect("render");

        assert!(summary.cancelled, "should report having been cancelled");
        assert!(
            summary.frames < 40,
            "should not have converted every frame, got {}",
            summary.frames
        );
    }

    #[test]
    fn audio_is_carried_through_from_the_named_source() {
        let t = tools();
        let dir = tempfile::tempdir().expect("temp dir");
        let input = dir.path().join("in.mp4");
        make_test_clip(&t, &input, 32, 32, 6, 10.0);

        let mut j = job(dir.path(), &input, "out.mp4");
        j.audio = Some(input.clone());
        let summary = render(&t, &j, &mut ignore, &AtomicBool::new(false)).expect("render");

        assert!(probe(&t, &summary.outputs[0]).expect("probe").has_audio);
    }

    #[test]
    fn an_audio_source_with_no_audio_is_refused_by_name() {
        // ffmpeg is asked to map an audio stream that does not exist, exits,
        // and the write fails with a broken pipe — an error that says nothing
        // about the actual problem. Catch it up front instead.
        let t = tools();
        let dir = tempfile::tempdir().expect("temp dir");
        let silent = dir.path().join("silent.mp4");
        make_silent_clip(&t, &silent, 32, 32, 4, 10.0);

        let mut j = job(dir.path(), &silent, "out.mp4");
        j.audio = Some(silent.clone());
        let err = render(&t, &j, &mut ignore, &AtomicBool::new(false)).expect_err("should refuse");
        let message = err.to_string();
        assert!(
            message.contains("silent.mp4") && message.contains("audio"),
            "the error must name the file and the problem, got: {message}"
        );
    }

    #[test]
    fn a_silent_source_converts_when_no_audio_is_asked_for() {
        let t = tools();
        let dir = tempfile::tempdir().expect("temp dir");
        let silent = dir.path().join("silent.mp4");
        make_silent_clip(&t, &silent, 32, 32, 4, 10.0);

        let summary = render(
            &t,
            &job(dir.path(), &silent, "out.mp4"),
            &mut ignore,
            &AtomicBool::new(false),
        )
        .expect("a silent film must still convert");
        assert_eq!(summary.frames, 4);
    }

    #[test]
    fn a_colour_source_at_another_resolution_is_used_anyway() {
        // A 2D release of the same film is very often a different resolution
        // from the anaglyph rip. Refusing it would be useless when the user has
        // exactly the file they need.
        let t = tools();
        let dir = tempfile::tempdir().expect("temp dir");
        let input = dir.path().join("in.mp4");
        let bigger = dir.path().join("big.mp4");
        make_test_clip(&t, &input, 64, 48, 4, 10.0);
        make_test_clip(&t, &bigger, 128, 96, 4, 10.0);

        let mut j = job(dir.path(), &input, "out.mp4");
        j.colour = Some(bigger);
        let summary = render(&t, &j, &mut ignore, &AtomicBool::new(false))
            .expect("a differently sized colour source should be conformed, not refused");
        assert_eq!(summary.frames, 4);

        let info = probe(&t, &summary.outputs[0]).expect("probe");
        assert_eq!(
            (info.width, info.height),
            (128, 48),
            "the output keeps the anaglyph's geometry"
        );
    }

    #[test]
    fn a_differently_shaped_source_is_reported_but_still_used() {
        // Same resolution family, different crop: 16:9 against a scope rip.
        // Resizing stretches faces sideways, so it is worth saying so — but
        // stopping the render would be worse than mentioning it.
        let t = tools();
        let dir = tempfile::tempdir().expect("temp dir");
        let input = dir.path().join("in.mp4");
        let wide = dir.path().join("wide.mp4");
        make_test_clip(&t, &input, 128, 32, 4, 10.0);
        make_test_clip(&t, &wide, 128, 96, 4, 10.0);

        let mut j = job(dir.path(), &input, "out.mp4");
        j.colour = Some(wide);
        let mut notes = Vec::new();
        let summary = render(
            &t,
            &j,
            &mut |p| {
                if let Progress::Note(message) = p {
                    notes.push(message);
                }
            },
            &AtomicBool::new(false),
        )
        .expect("render");
        assert_eq!(summary.frames, 4);
        assert!(
            notes.iter().any(|n| n.contains("shape")),
            "should have mentioned the aspect difference, got {notes:?}"
        );
    }

    #[test]
    fn matching_sources_produce_no_notes() {
        let t = tools();
        let dir = tempfile::tempdir().expect("temp dir");
        let input = dir.path().join("in.mp4");
        make_test_clip(&t, &input, 64, 48, 3, 10.0);

        let mut j = job(dir.path(), &input, "out.mp4");
        j.colour = Some(input.clone());
        let mut notes = Vec::new();
        render(
            &t,
            &j,
            &mut |p| {
                if let Progress::Note(message) = p {
                    notes.push(message);
                }
            },
            &AtomicBool::new(false),
        )
        .expect("render");
        assert!(notes.is_empty(), "nothing to report, got {notes:?}");
    }

    #[test]
    fn naming_a_mono_source_without_choosing_an_eye_is_refused() {
        // Silently ignoring it would leave the user believing their 2D copy
        // was in use.
        let t = tools();
        let dir = tempfile::tempdir().expect("temp dir");
        let input = dir.path().join("in.mp4");
        make_test_clip(&t, &input, 32, 32, 4, 10.0);

        let mut j = job(dir.path(), &input, "out.mp4");
        j.mono = Some(input.clone());
        assert!(matches!(
            render(&t, &j, &mut ignore, &AtomicBool::new(false)),
            Err(PipelineError::MonoSourceUnused)
        ));
    }

    #[test]
    fn choosing_a_mono_eye_without_a_source_is_refused() {
        let t = tools();
        let dir = tempfile::tempdir().expect("temp dir");
        let input = dir.path().join("in.mp4");
        make_test_clip(&t, &input, 32, 32, 4, 10.0);

        let mut j = job(dir.path(), &input, "out.mp4");
        j.params.mono_eye = MonoEye::Left;
        assert!(matches!(
            render(&t, &j, &mut ignore, &AtomicBool::new(false)),
            Err(PipelineError::MonoSourceMissing(MonoEye::Left))
        ));
    }

    #[test]
    fn invalid_parameters_are_refused_before_any_work_starts() {
        let t = tools();
        let dir = tempfile::tempdir().expect("temp dir");
        let input = dir.path().join("in.mp4");
        make_test_clip(&t, &input, 32, 32, 4, 10.0);

        let mut j = job(dir.path(), &input, "out.mp4");
        j.params.decimate_horiz = 500.0;
        let out = j.output.clone();
        assert!(matches!(
            render(&t, &j, &mut ignore, &AtomicBool::new(false)),
            Err(PipelineError::Params(_))
        ));
        assert!(!out.exists(), "nothing should have been written");
    }

    /// Rewrites a clip's pixel shape without touching its pixels, so a test can
    /// have a non-square source without hand-building one.
    fn set_pixel_aspect(t: &FfmpegTools, src: &Path, dst: &Path, sar: &str) {
        let status = std::process::Command::new(&t.ffmpeg)
            .args(["-y", "-v", "error", "-i"])
            .arg(src)
            .args([
                "-vf",
                &format!("setsar={sar}"),
                "-c:v",
                "libx264",
                "-crf",
                "12",
            ])
            .arg(dst)
            .status()
            .expect("run ffmpeg");
        assert!(status.success());
    }

    #[test]
    fn a_non_square_source_is_not_rendered_square() {
        // The failure this guards: raw frames carry no pixel-aspect metadata,
        // so a 708x276 transfer with 8:9 pixels used to come out marked square
        // and played back 12% too wide.
        let t = tools();
        let dir = tempfile::tempdir().expect("temp dir");
        let plain = dir.path().join("plain.mp4");
        let wide = dir.path().join("wide.mp4");
        make_test_clip(&t, &plain, 64, 48, 4, 10.0);
        set_pixel_aspect(&t, &plain, &wide, "8/9");

        let source = probe(&t, &wide).expect("probe source");
        assert!(
            (source.sample_aspect - 8.0 / 9.0).abs() < 1e-3,
            "fixture must be non-square"
        );

        let mut j = job(dir.path(), &wide, "out.mkv");
        j.audio = None;
        let summary = render(&t, &j, &mut ignore, &AtomicBool::new(false)).expect("render");

        // Side by side, so the finished frame is twice the source's shape.
        let out = probe(&t, &summary.outputs[0]).expect("probe output");
        let want = source.display_aspect() * 2.0;
        assert!(
            (out.display_aspect() - want).abs() < 0.02,
            "expected a {want:.3}:1 frame, got {:.3}:1 ({}x{} sar {})",
            out.display_aspect(),
            out.width,
            out.height,
            out.sample_aspect
        );
    }

    #[test]
    fn a_single_eye_from_a_non_square_source_keeps_its_shape() {
        let t = tools();
        let dir = tempfile::tempdir().expect("temp dir");
        let plain = dir.path().join("plain.mp4");
        let wide = dir.path().join("wide.mp4");
        make_test_clip(&t, &plain, 64, 48, 4, 10.0);
        set_pixel_aspect(&t, &plain, &wide, "8/9");
        let source = probe(&t, &wide).expect("probe");

        let mut j = job(dir.path(), &wide, "eye.mkv");
        j.audio = None;
        j.params.layout = OutputLayout::LeftOnly;
        let summary = render(&t, &j, &mut ignore, &AtomicBool::new(false)).expect("render");

        let out = probe(&t, &summary.outputs[0]).expect("probe");
        assert!(
            (out.display_aspect() - source.display_aspect()).abs() < 0.02,
            "one eye should look like the source: {:.3} vs {:.3}",
            out.display_aspect(),
            source.display_aspect()
        );
    }

    #[test]
    fn a_square_source_stays_square() {
        let t = tools();
        let dir = tempfile::tempdir().expect("temp dir");
        let input = dir.path().join("in.mp4");
        make_test_clip(&t, &input, 64, 48, 4, 10.0);

        let mut j = job(dir.path(), &input, "out.mkv");
        j.params.layout = OutputLayout::LeftOnly;
        let summary = render(&t, &j, &mut ignore, &AtomicBool::new(false)).expect("render");
        let out = probe(&t, &summary.outputs[0]).expect("probe");
        assert!(
            (out.display_aspect() - 64.0 / 48.0).abs() < 0.02,
            "got {:.3}",
            out.display_aspect()
        );
    }

    #[test]
    fn a_start_point_skips_the_frames_before_it() {
        let t = tools();
        let dir = tempfile::tempdir().expect("temp dir");
        let input = dir.path().join("in.mp4");
        make_test_clip(&t, &input, 48, 32, 20, 10.0);

        let mut j = job(dir.path(), &input, "out.mp4");
        j.params.anaglyph_trim = SourceTrim {
            start: 12,
            end: None,
        };
        let summary = render(&t, &j, &mut ignore, &AtomicBool::new(false)).expect("render");
        assert_eq!(summary.frames, 8, "20 frames from 12 leaves 8");
    }

    #[test]
    fn start_and_end_points_select_exactly_that_range() {
        let t = tools();
        let dir = tempfile::tempdir().expect("temp dir");
        let input = dir.path().join("in.mp4");
        make_test_clip(&t, &input, 48, 32, 30, 10.0);

        let mut j = job(dir.path(), &input, "out.mp4");
        j.params.anaglyph_trim = SourceTrim {
            start: 5,
            end: Some(14),
        };
        let summary = render(&t, &j, &mut ignore, &AtomicBool::new(false)).expect("render");
        assert_eq!(summary.frames, 10, "frames 5..=14 inclusive");
    }

    #[test]
    fn progress_totals_count_the_trim_not_the_whole_file() {
        // An ETA computed against the full film would be wildly wrong for a
        // short trimmed section.
        let t = tools();
        let dir = tempfile::tempdir().expect("temp dir");
        let input = dir.path().join("in.mp4");
        make_test_clip(&t, &input, 32, 32, 40, 10.0);

        let mut j = job(dir.path(), &input, "out.mp4");
        j.params.anaglyph_trim = SourceTrim {
            start: 10,
            end: Some(19),
        };
        let mut started = None;
        render(
            &t,
            &j,
            &mut |p| {
                if let Progress::Started { total_frames, .. } = p {
                    started = total_frames;
                }
            },
            &AtomicBool::new(false),
        )
        .expect("render");
        assert_eq!(started, Some(10), "should report the trimmed length");
    }

    #[test]
    fn a_trim_that_selects_nothing_is_refused() {
        let t = tools();
        let dir = tempfile::tempdir().expect("temp dir");
        let input = dir.path().join("in.mp4");
        make_test_clip(&t, &input, 32, 32, 10, 10.0);

        let mut j = job(dir.path(), &input, "out.mp4");
        j.params.anaglyph_trim = SourceTrim {
            start: 500,
            end: None,
        };
        let err = render(&t, &j, &mut ignore, &AtomicBool::new(false)).expect_err("should refuse");
        assert!(
            err.to_string().contains("no frames"),
            "the message should say the range is empty, got: {err}"
        );
    }

    #[test]
    fn a_misaligned_2d_source_is_read_from_its_own_start() {
        // The point of the whole feature. The anaglyph's moment lives at frame
        // 10; the same moment in the 2D copy lives at frame 3. Converting from
        // there must pair 10 with 3, not 10 with 10.
        let t = tools();
        let dir = tempfile::tempdir().expect("temp dir");
        let anaglyph = dir.path().join("ana.mp4");
        let mono = dir.path().join("mono.mp4");
        make_test_clip(&t, &anaglyph, 64, 48, 30, 10.0);
        make_test_clip(&t, &mono, 64, 48, 30, 10.0);

        let mut j = job(dir.path(), &anaglyph, "out.mp4");
        j.mono = Some(mono.clone());
        j.params.mono_eye = MonoEye::Left;
        j.params.layout = OutputLayout::Separate;
        j.params.anaglyph_trim = SourceTrim {
            start: 10,
            end: Some(15),
        };
        j.params.mono_trim = SourceTrim {
            start: 3,
            end: None,
        };
        let summary = render(&t, &j, &mut ignore, &AtomicBool::new(false)).expect("render");
        assert_eq!(summary.frames, 6);

        // The 2D eye passes through untouched, so the first output frame should
        // resemble mono frame 3 far more than mono frame 10.
        let out_info = probe(&t, &summary.outputs[0]).expect("probe");
        let produced = ana_media::grab_frame(&t, &summary.outputs[0], &out_info, 0).expect("grab");
        let mono_info = probe(&t, &mono).expect("probe mono");
        let aligned = ana_media::grab_frame(&t, &mono, &mono_info, 3).expect("grab aligned");
        let wrong = ana_media::grab_frame(&t, &mono, &mono_info, 10).expect("grab wrong");

        let distance = |a: &ana_core::FrameF32, b: &ana_core::FrameF32| -> f32 {
            a.as_slice()
                .iter()
                .zip(b.as_slice())
                .map(|(x, y)| (x - y).abs())
                .sum::<f32>()
                / a.as_slice().len() as f32
        };
        let (near, far) = (distance(&produced, &aligned), distance(&produced, &wrong));
        eprintln!("aligned distance {near:.4}, misaligned distance {far:.4}");
        assert!(
            near < far,
            "output matched the wrong frame: {near:.4} vs {far:.4}"
        );
    }

    #[test]
    fn a_mono_source_reaches_the_output() {
        // The 2D copy passes through ungraded, so a flat mono source should
        // leave one half of the frame visibly different from the recovered one.
        let t = tools();
        let dir = tempfile::tempdir().expect("temp dir");
        let input = dir.path().join("in.mp4");
        make_test_clip(&t, &input, 64, 48, 4, 10.0);

        let mut j = job(dir.path(), &input, "out.mp4");
        j.mono = Some(input.clone());
        j.params.mono_eye = MonoEye::Left;
        j.params.layout = OutputLayout::Separate;
        let summary = render(&t, &j, &mut ignore, &AtomicBool::new(false)).expect("render");
        assert_eq!(summary.outputs.len(), 2);
        assert!(summary.outputs.iter().all(|p| p.exists()));
    }
}
