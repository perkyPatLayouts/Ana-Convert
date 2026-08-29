// Ana-Convert — anaglyph 3D to full-colour stereo recovery
// SPDX-License-Identifier: GPL-3.0-or-later

//! The sample clip shipped for people to learn the app on.
//!
//! Every other test builds its own fixture and throws it away, so nothing
//! otherwise notices if the committed sample is regenerated wrong, encoded at
//! the wrong size, or quietly stops being a stereo pair. It is the first file
//! most people will open, and the one the guide's instructions are written
//! against — so its shape is checked here rather than trusted.

use std::path::PathBuf;
use std::sync::atomic::AtomicBool;

use ana_core::compose::OutputLayout;
use ana_core::packed::StereoPacking;
use ana_core::params::{ConvertParams, InputMode};
use ana_media::encode::{EncodeSettings, VideoCodec};
use ana_pipeline::{render, RenderJob};

/// What `docs/make-sample.py` is built to produce.
const WIDTH: usize = 1280;
const HEIGHT: usize = 360;
const FRAMES: u64 = 250;
const FPS: f64 = 25.0;

fn sample() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../docs/sample/stereo-sample-sbs.mp4")
}

#[test]
fn the_sample_is_the_shape_the_guide_says_it_is() {
    let (tools, _) = ana_media::locate(None).expect("ffmpeg must be installed");
    let info = ana_media::probe(&tools, &sample()).expect("the sample should probe");

    assert_eq!((info.width, info.height), (WIDTH, HEIGHT));
    assert_eq!(info.estimated_frame_count(), Some(FRAMES));
    assert!((info.fps - FPS).abs() < 0.001, "got {} fps", info.fps);
    assert!(
        (info.sample_aspect - 1.0).abs() < 1e-6,
        "square pixels, so nobody has to reach for the anamorphic switch first"
    );
}

#[test]
fn the_sample_converts_as_a_side_by_side_pair() {
    let (tools, _) = ana_media::locate(None).expect("ffmpeg must be installed");
    let dir = tempfile::tempdir().expect("temp dir");
    let output = dir.path().join("left.mkv");

    let job = RenderJob {
        anaglyph: sample(),
        right_eye: None,
        colour: None,
        mono: None,
        audio: None,
        output: output.clone(),
        params: ConvertParams {
            input: InputMode::packed(StereoPacking::SideBySide, false),
            layout: OutputLayout::LeftOnly,
            ..Default::default()
        },
        encode: EncodeSettings {
            codec: VideoCodec::H264,
            fps: FPS,
            ..Default::default()
        },
    };

    let summary = render(&tools, &job, &mut |_| {}, &AtomicBool::new(false))
        .expect("the sample should convert");
    assert_eq!(summary.frames, FRAMES);

    // One eye out of a side-by-side frame is half its width. Getting the full
    // width back would mean the pair was never taken apart.
    let out = ana_media::probe(&tools, &output).expect("probe the result");
    assert_eq!((out.width, out.height), (WIDTH / 2, HEIGHT));
}

#[test]
fn the_two_eyes_of_the_sample_differ() {
    // A stereo sample whose eyes match is a 2D sample. The scene is built with
    // objects in front of and behind the screen, so the halves must disagree.
    let (tools, _) = ana_media::locate(None).expect("ffmpeg must be installed");
    let info = ana_media::probe(&tools, &sample()).expect("probe");
    let frame = ana_media::grab_frame(&tools, &sample(), &info, FRAMES / 2).expect("grab");

    let (r, g, b) = frame.rgb_planes();
    let half = WIDTH / 2;
    let mut differing = 0usize;
    for y in 0..HEIGHT {
        for x in 0..half {
            let (l, rt) = (y * WIDTH + x, y * WIDTH + x + half);
            if (r[l] - r[rt]).abs() > 0.02
                || (g[l] - g[rt]).abs() > 0.02
                || (b[l] - b[rt]).abs() > 0.02
            {
                differing += 1;
            }
        }
    }
    // The scene measures a little over 5%, so this has room to spare without
    // being so loose that a flat sample would slip through.
    let fraction = differing as f32 / (half * HEIGHT) as f32;
    assert!(
        fraction > 0.01,
        "only {:.2}% of the frame differs between the eyes — there is no \
         parallax in this sample",
        fraction * 100.0
    );
}
