// Ana-Convert — anaglyph 3D to full-colour stereo recovery
// SPDX-License-Identifier: GPL-3.0-or-later

//! The preview's central claim, tested.
//!
//! The app converts a shrunken frame so that sliders stay responsive. That is
//! only defensible if what it shows matches what the full-size render will
//! produce — otherwise every value dialled in against it is wrong, which is a
//! worse failure than being slow.
//!
//! These tests compare a preview-sized conversion against the full-size one
//! reduced to the same dimensions. They need no ffmpeg and no window.

use ana_app::preview::scale_params_for_preview;
use ana_core::compose::resize;
use ana_core::extract::{encode_anaglyph, AnaglyphFormat};
use ana_core::params::ConvertParams;
use ana_core::pipeline::{process_frame, Sources};
use ana_core::transfer::luminance;
use ana_core::FrameF32;

const WIDTH: usize = 480;
const HEIGHT: usize = 320;

/// A scene with hard edges, saturated blocks and real disparity — the content
/// that punishes a blur mismatch hardest.
fn stereo_pair() -> (FrameF32, FrameF32) {
    let blocks: [(isize, isize, isize, isize, [f32; 3], isize); 5] = [
        (40, 40, 120, 90, [0.85, 0.18, 0.15], -14),
        (200, 60, 110, 120, [0.15, 0.70, 0.25], 9),
        (340, 30, 100, 80, [0.20, 0.30, 0.90], 18),
        (90, 190, 140, 100, [0.90, 0.80, 0.20], 12),
        (280, 200, 130, 90, [0.75, 0.35, 0.80], -10),
    ];

    let render = |shift: isize| {
        let mut planes = [
            vec![0.0f32; WIDTH * HEIGHT],
            vec![0.0f32; WIDTH * HEIGHT],
            vec![0.0f32; WIDTH * HEIGHT],
        ];
        for y in 0..HEIGHT {
            for x in 0..WIDTH {
                let i = y * WIDTH + x;
                let v = y as f32 / HEIGHT as f32;
                planes[0][i] = 0.12 + 0.22 * v;
                planes[1][i] = 0.16 + 0.18 * (1.0 - v);
                planes[2][i] = 0.28 + 0.28 * v;
            }
        }
        for (bx, by, bw, bh, colour, disparity) in blocks {
            for dy in 0..bh {
                for dx in 0..bw {
                    let (px, py) = (bx + dx + disparity * shift, by + dy);
                    if px < 0 || py < 0 || px >= WIDTH as isize || py >= HEIGHT as isize {
                        continue;
                    }
                    let i = py as usize * WIDTH + px as usize;
                    for c in 0..3 {
                        planes[c][i] = colour[c];
                    }
                }
            }
        }
        FrameF32::from_rgb_planes(WIDTH, HEIGHT, &planes[0], &planes[1], &planes[2])
    };

    (render(0), render(1))
}

fn anaglyph() -> FrameF32 {
    let (left, right) = stereo_pair();
    encode_anaglyph(&left, &right, AnaglyphFormat::RedCyan)
}

/// Converts at full size, then reduces the result — what the render produces,
/// seen small.
fn full_then_shrink(params: &ConvertParams, scale: f32) -> FrameF32 {
    let source = anaglyph();
    let pair = process_frame(Sources::from_anaglyph(&source), params);
    let (w, h) = target_size(scale);
    resize(&pair.left, w, h)
}

/// Converts at preview size with rescaled blur — what the app shows.
fn shrink_then_convert(params: &ConvertParams, scale: f32) -> FrameF32 {
    let (w, h) = target_size(scale);
    let small = resize(&anaglyph(), w, h);
    let scaled = scale_params_for_preview(params, scale);
    process_frame(Sources::from_anaglyph(&small), &scaled).left
}

fn target_size(scale: f32) -> (usize, usize) {
    (
        (WIDTH as f32 * scale).round() as usize,
        (HEIGHT as f32 * scale).round() as usize,
    )
}

/// Mean absolute luminance difference, which is what an eye judging a preview
/// is actually comparing.
fn mean_luma_difference(a: &FrameF32, b: &FrameF32) -> f32 {
    assert_eq!((a.width(), a.height()), (b.width(), b.height()));
    let (ar, ag, ab) = a.rgb_planes();
    let (br, bg, bb) = b.rgb_planes();
    let total: f32 = (0..a.plane_len())
        .map(|i| {
            let x = luminance(ar[i], ag[i], ab[i]).clamp(0.0, 1.0);
            let y = luminance(br[i], bg[i], bb[i]).clamp(0.0, 1.0);
            (x - y).abs()
        })
        .sum();
    total / a.plane_len() as f32
}

#[test]
fn a_half_size_preview_matches_the_full_render() {
    let params = ConvertParams::default();
    let shown = shrink_then_convert(&params, 0.5);
    let truth = full_then_shrink(&params, 0.5);
    let error = mean_luma_difference(&shown, &truth);
    eprintln!("half size: mean luma difference {error:.4}");
    assert!(
        error < 0.04,
        "preview drifted from the render by {error:.4} — tuning against it would mislead"
    );
}

#[test]
fn a_quarter_size_preview_still_matches() {
    let params = ConvertParams::default();
    let shown = shrink_then_convert(&params, 0.25);
    let truth = full_then_shrink(&params, 0.25);
    let error = mean_luma_difference(&shown, &truth);
    eprintln!("quarter size: mean luma difference {error:.4}");
    assert!(error < 0.05, "preview drifted by {error:.4}");
}

#[test]
fn rescaling_the_blur_is_what_makes_it_match() {
    // Without the rescale a half-size preview shows half the blur, and the
    // whole arrangement quietly lies. This is the test that would catch it.
    let params = ConvertParams::default();
    let truth = full_then_shrink(&params, 0.5);

    let corrected = mean_luma_difference(&shrink_then_convert(&params, 0.5), &truth);

    let (w, h) = target_size(0.5);
    let naive_frame = process_frame(
        Sources::from_anaglyph(&resize(&anaglyph(), w, h)),
        &params, // deliberately not rescaled
    )
    .left;
    let naive = mean_luma_difference(&naive_frame, &truth);

    eprintln!("rescaled {corrected:.4} vs unrescaled {naive:.4}");
    assert!(
        corrected < naive,
        "rescaling must improve the match: {corrected:.4} vs {naive:.4}"
    );
}

#[test]
fn a_full_size_preview_is_the_render_exactly() {
    let params = ConvertParams::default();
    let shown = shrink_then_convert(&params, 1.0);
    let truth = full_then_shrink(&params, 1.0);
    assert_eq!(
        shown.as_slice(),
        truth.as_slice(),
        "at full size the preview must be the render itself"
    );
}

#[test]
fn heavier_blur_settings_also_survive_the_preview() {
    // The blur control is the one most likely to be dragged to an extreme, so
    // the agreement has to hold across its range, not just at the default.
    for decimate in [1.0, 2.0, 5.0, 25.0] {
        let params = ConvertParams {
            decimate_horiz: decimate,
            decimate_vert: decimate,
            ..Default::default()
        };
        let error = mean_luma_difference(
            &shrink_then_convert(&params, 0.5),
            &full_then_shrink(&params, 0.5),
        );
        eprintln!("decimate {decimate}: mean luma difference {error:.4}");
        assert!(error < 0.05, "decimate {decimate} drifted by {error:.4}");
    }
}
