// Ana-Convert — anaglyph 3D to full-colour stereo recovery
// SPDX-License-Identifier: GPL-3.0-or-later

//! Sources that are not the same size as the anaglyph.
//!
//! A 2D release of the same film is very often a different resolution from the
//! anaglyph rip — a 1080p transfer beside a 708-wide one is entirely ordinary.
//! Opening one used to panic the app inside `restore_colour`, which asserts
//! that the eye and the colour reference share geometry.
//!
//! These tests drive the same decode-and-convert path the preview uses.

use ana_core::compose::conform_to;
use ana_core::params::{ConvertParams, MonoEye};
use ana_core::pipeline::{compose_output, process_frame, Sources};
use ana_core::FrameF32;
use ana_media::testing::make_silent_clip;
use ana_media::{grab_frame, locate, probe, FfmpegTools, VideoInfo};

struct Clip {
    path: std::path::PathBuf,
    info: VideoInfo,
}

fn clip(tools: &FfmpegTools, dir: &std::path::Path, name: &str, w: usize, h: usize) -> Clip {
    let path = dir.join(name);
    make_silent_clip(tools, &path, w, h, 6, 10.0);
    let info = probe(tools, &path).expect("probe");
    Clip { path, info }
}

/// What the preview does: decode each source, bring the secondary ones to the
/// anaglyph's geometry, convert.
fn preview(
    tools: &FfmpegTools,
    anaglyph: &Clip,
    other: Option<&Clip>,
    params: &ConvertParams,
    mono: bool,
) -> Vec<FrameF32> {
    let base = grab_frame(tools, &anaglyph.path, &anaglyph.info, 2).expect("grab anaglyph");
    let (w, h) = (base.width(), base.height());
    let secondary = other.map(|c| {
        let f = grab_frame(tools, &c.path, &c.info, 2).expect("grab secondary");
        conform_to(&f, w, h)
    });

    let pair = process_frame(
        Sources {
            primary: &base,
            right_eye: None,
            colour: if mono { None } else { secondary.as_ref() },
            mono: if mono { secondary.as_ref() } else { None },
        },
        params,
    );
    compose_output(&pair, params)
}

fn tools() -> FfmpegTools {
    locate(None).expect("ffmpeg must be installed").0
}

#[test]
fn a_larger_colour_source_converts_instead_of_panicking() {
    let t = tools();
    let dir = tempfile::tempdir().expect("temp dir");
    let anaglyph = clip(&t, dir.path(), "ana.mp4", 128, 64);
    let colour = clip(&t, dir.path(), "colour.mp4", 640, 480);

    let frames = preview(
        &t,
        &anaglyph,
        Some(&colour),
        &ConvertParams::default(),
        false,
    );
    assert_eq!(frames.len(), 1);
    assert_eq!(
        (frames[0].width(), frames[0].height()),
        (256, 64),
        "the output keeps the anaglyph's geometry, side by side"
    );
    assert!(frames[0].as_slice().iter().all(|s| s.is_finite()));
}

#[test]
fn a_smaller_colour_source_also_converts() {
    let t = tools();
    let dir = tempfile::tempdir().expect("temp dir");
    let anaglyph = clip(&t, dir.path(), "ana.mp4", 256, 128);
    let colour = clip(&t, dir.path(), "colour.mp4", 64, 32);

    let frames = preview(
        &t,
        &anaglyph,
        Some(&colour),
        &ConvertParams::default(),
        false,
    );
    assert!(frames[0].as_slice().iter().all(|s| s.is_finite()));
}

#[test]
fn a_larger_2d_eye_source_converts() {
    // The mono path substitutes the frame wholesale, so a mismatch would show
    // up in composition rather than in restoration — a different panic, same
    // cause.
    let t = tools();
    let dir = tempfile::tempdir().expect("temp dir");
    let anaglyph = clip(&t, dir.path(), "ana.mp4", 128, 64);
    let mono = clip(&t, dir.path(), "mono.mp4", 640, 480);

    let params = ConvertParams {
        mono_eye: MonoEye::Left,
        ..Default::default()
    };
    let frames = preview(&t, &anaglyph, Some(&mono), &params, true);
    assert_eq!((frames[0].width(), frames[0].height()), (256, 64));
}

#[test]
fn an_oddly_shaped_source_still_converts() {
    // A very tall source against a wide one: the shape is wrong and the result
    // will be stretched, but that is a warning, not a reason to stop.
    let t = tools();
    let dir = tempfile::tempdir().expect("temp dir");
    let anaglyph = clip(&t, dir.path(), "ana.mp4", 320, 64);
    let colour = clip(&t, dir.path(), "colour.mp4", 64, 320);

    let frames = preview(
        &t,
        &anaglyph,
        Some(&colour),
        &ConvertParams::default(),
        false,
    );
    assert!(frames[0].as_slice().iter().all(|s| s.is_finite()));
}
