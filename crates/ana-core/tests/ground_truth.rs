// Ana-Convert — anaglyph 3D to full-colour stereo recovery
// SPDX-License-Identifier: GPL-3.0-or-later

//! Recovery accuracy against a synthetic scene we know the true answer for.
//!
//! The original AviSynth implementation could only ever be judged by eye: real
//! anaglyph releases have no surviving full-colour master to compare against.
//! Here the stereo pair is generated first and the anaglyph derived from it, so
//! every recovered pixel has a ground truth to be scored against.
//!
//! Set `ANA_DUMP=<dir>` to write the scenes and recoveries out as PNGs.

use ana_core::compose::OutputLayout;
use ana_core::extract::{encode_anaglyph, AnaglyphFormat};
use ana_core::frame::FrameF32;
use ana_core::params::{ConvertParams, MonoEye};
use ana_core::pipeline::{process_frame, Sources};
use ana_core::transfer::luminance;

const WIDTH: usize = 160;
const HEIGHT: usize = 120;

// --- the scene -------------------------------------------------------------

/// A coloured box sitting at some stereo depth.
struct Box3d {
    x: isize,
    y: isize,
    w: isize,
    h: isize,
    colour: [f32; 3],
    /// Horizontal shift applied to the right eye. Positive sits behind the
    /// screen, negative in front of it.
    disparity: isize,
}

fn scene() -> Vec<Box3d> {
    vec![
        Box3d {
            x: 10,
            y: 15,
            w: 40,
            h: 35,
            colour: [0.85, 0.15, 0.15],
            disparity: -6,
        },
        Box3d {
            x: 62,
            y: 22,
            w: 34,
            h: 46,
            colour: [0.15, 0.70, 0.25],
            disparity: 3,
        },
        Box3d {
            x: 108,
            y: 12,
            w: 40,
            h: 30,
            colour: [0.20, 0.30, 0.90],
            disparity: 8,
        },
        Box3d {
            x: 28,
            y: 66,
            w: 46,
            h: 38,
            colour: [0.90, 0.80, 0.20],
            disparity: 5,
        },
        Box3d {
            x: 88,
            y: 72,
            w: 44,
            h: 34,
            colour: [0.75, 0.35, 0.80],
            disparity: -4,
        },
        Box3d {
            x: 55,
            y: 50,
            w: 22,
            h: 22,
            colour: [0.95, 0.95, 0.95],
            disparity: 0,
        },
    ]
}

/// Renders one eye. `shift` is 0 for the left eye and 1 for the right.
fn render_eye(shift: isize) -> FrameF32 {
    let mut r = vec![0.0f32; WIDTH * HEIGHT];
    let mut g = vec![0.0f32; WIDTH * HEIGHT];
    let mut b = vec![0.0f32; WIDTH * HEIGHT];

    // A gently varying background, so the frame is not mostly flat colour.
    for y in 0..HEIGHT {
        for x in 0..WIDTH {
            let i = y * WIDTH + x;
            let v = y as f32 / HEIGHT as f32;
            r[i] = 0.10 + 0.25 * v;
            g[i] = 0.14 + 0.20 * (1.0 - v);
            b[i] = 0.30 + 0.30 * v;
        }
    }

    for boxen in scene() {
        let offset = boxen.disparity * shift;
        for dy in 0..boxen.h {
            for dx in 0..boxen.w {
                let (px, py) = (boxen.x + dx + offset, boxen.y + dy);
                if px < 0 || py < 0 || px >= WIDTH as isize || py >= HEIGHT as isize {
                    continue;
                }
                let i = py as usize * WIDTH + px as usize;
                r[i] = boxen.colour[0];
                g[i] = boxen.colour[1];
                b[i] = boxen.colour[2];
            }
        }
    }
    FrameF32::from_rgb_planes(WIDTH, HEIGHT, &r, &g, &b)
}

// --- scoring ---------------------------------------------------------------

/// Peak signal-to-noise ratio in dB over all three channels.
fn psnr(a: &FrameF32, b: &FrameF32) -> f32 {
    let mse: f64 = a
        .as_slice()
        .iter()
        .zip(b.as_slice())
        .map(|(&x, &y)| {
            let d = (x.clamp(0.0, 1.0) - y.clamp(0.0, 1.0)) as f64;
            d * d
        })
        .sum::<f64>()
        / a.as_slice().len() as f64;
    if mse <= 0.0 {
        return f32::INFINITY;
    }
    (10.0 * (1.0 / mse).log10()) as f32
}

/// PSNR of the luminance channel alone — the part the algorithm is meant to
/// recover exactly, as distinct from colour, which it can only approximate.
fn luma_psnr(a: &FrameF32, b: &FrameF32) -> f32 {
    let (ar, ag, ab) = a.rgb_planes();
    let (br, bg, bb) = b.rgb_planes();
    let mse: f64 = (0..a.plane_len())
        .map(|i| {
            let x = luminance(ar[i], ag[i], ab[i]).clamp(0.0, 1.0);
            let y = luminance(br[i], bg[i], bb[i]).clamp(0.0, 1.0);
            let d = (x - y) as f64;
            d * d
        })
        .sum::<f64>()
        / a.plane_len() as f64;
    if mse <= 0.0 {
        return f32::INFINITY;
    }
    (10.0 * (1.0 / mse).log10()) as f32
}

fn dump(name: &str, frame: &FrameF32) {
    let Ok(dir) = std::env::var("ANA_DUMP") else {
        return;
    };
    std::fs::create_dir_all(&dir).expect("create dump dir");
    let path = format!("{dir}/{name}.ppm");
    let mut out = format!("P6\n{} {}\n255\n", frame.width(), frame.height()).into_bytes();
    out.extend_from_slice(&frame.to_rgb8());
    std::fs::write(&path, out).expect("write dump");
    eprintln!("wrote {path}");
}

// --- the tests -------------------------------------------------------------

/// Params for the realistic best case: a frame-accurate 2D release exists, so
/// it supplies colour for both eyes.
fn params_with_reference(format: AnaglyphFormat) -> ConvertParams {
    ConvertParams {
        input_format: format,
        layout: OutputLayout::Separate,
        ..Default::default()
    }
}

/// Defaults, but with colour blur switched off — the case where restoration
/// should be able to reproduce the reference eye exactly.
fn params_unblurred(format: AnaglyphFormat) -> ConvertParams {
    ConvertParams {
        decimate_horiz: 100.0,
        decimate_vert: 100.0,
        ..params_with_reference(format)
    }
}

#[test]
fn a_perfect_unblurred_reference_reproduces_that_eye_almost_exactly() {
    // End-to-end proof that the chain is sound. The left eye's red channel
    // survives the anaglyph intact and the reference *is* the left eye, so
    // with no colour blur in the way the output should be the original back.
    // Everything below this quality is the cost of blur, not of the pipeline.
    for format in [AnaglyphFormat::RedCyan, AnaglyphFormat::GreenMagenta] {
        let (left, right) = (render_eye(0), render_eye(1));
        let anaglyph = encode_anaglyph(&left, &right, format);
        let pair = process_frame(
            Sources {
                primary: &anaglyph,
                right_eye: None,
                colour: Some(&left),
                mono: None,
            },
            &params_unblurred(format),
        );
        let score = psnr(&pair.left, &left);
        eprintln!("{format:?} unblurred left eye: {score:.1} dB");
        assert!(
            score >= 40.0,
            "{format:?} left eye only reached {score:.1} dB"
        );
    }
}

#[test]
fn a_real_colour_reference_beats_letting_the_anaglyph_colour_itself() {
    // The reason the original script asks for the 2D release at all.
    let (left, right) = (render_eye(0), render_eye(1));
    let anaglyph = encode_anaglyph(&left, &right, AnaglyphFormat::RedCyan);
    let params = params_with_reference(AnaglyphFormat::RedCyan);

    let with_ref = process_frame(
        Sources {
            primary: &anaglyph,
            right_eye: None,
            colour: Some(&left),
            mono: None,
        },
        &params,
    );
    let self_coloured = process_frame(Sources::from_anaglyph(&anaglyph), &params);

    let a = psnr(&with_ref.left, &left);
    let b = psnr(&self_coloured.left, &left);
    eprintln!("left eye — {a:.1} dB with a 2D reference, {b:.1} dB self-coloured");
    assert!(a > b, "a real reference must help: {a:.1} vs {b:.1} dB");
}

#[test]
fn less_colour_blur_means_better_colour() {
    // Colour error under default settings is dominated by the blur, and the
    // control must actually trade one against the other.
    let (left, right) = (render_eye(0), render_eye(1));
    let anaglyph = encode_anaglyph(&left, &right, AnaglyphFormat::RedCyan);
    let score = |decimate: f32| {
        let pair = process_frame(
            Sources {
                primary: &anaglyph,
                right_eye: None,
                colour: Some(&left),
                mono: None,
            },
            &ConvertParams {
                decimate_horiz: decimate,
                decimate_vert: decimate,
                ..params_with_reference(AnaglyphFormat::RedCyan)
            },
        );
        psnr(&pair.left, &left)
    };
    let (heavy, light, none) = (score(5.0), score(25.0), score(100.0));
    eprintln!("left eye by colour blur — 5%: {heavy:.1} dB, 25%: {light:.1} dB, off: {none:.1} dB");
    assert!(
        light > heavy,
        "less blur must improve colour: {light:.1} vs {heavy:.1}"
    );
    assert!(
        none > light,
        "no blur must be best of all: {none:.1} vs {light:.1}"
    );
}

#[test]
fn luminance_is_recovered_far_more_accurately_than_colour() {
    // The algorithm's central bet: brightness comes back nearly exactly and
    // colour is only approximated. If that stops holding, the bet has failed.
    let (left, right) = (render_eye(0), render_eye(1));
    let anaglyph = encode_anaglyph(&left, &right, AnaglyphFormat::RedCyan);
    let pair = process_frame(
        Sources {
            primary: &anaglyph,
            right_eye: None,
            colour: Some(&left),
            mono: None,
        },
        &params_with_reference(AnaglyphFormat::RedCyan),
    );

    dump("rc_truth_left", &left);
    dump("rc_truth_right", &right);
    dump("rc_anaglyph", &anaglyph);
    dump("rc_recovered_left", &pair.left);
    dump("rc_recovered_right", &pair.right);

    let colour_psnr = psnr(&pair.right, &right);
    let luma = luma_psnr(&pair.right, &right);
    eprintln!("right eye — colour {colour_psnr:.1} dB, luma {luma:.1} dB");
    assert!(luma >= 26.0, "luma recovery only reached {luma:.1} dB");
    assert!(
        luma > colour_psnr,
        "luma ({luma:.1}) should beat colour ({colour_psnr:.1})"
    );
}

#[test]
fn default_settings_hold_their_measured_quality() {
    // A regression guard, not a quality target. The scene is deliberately
    // adversarial — hard-edged saturated blocks at up to 8px disparity, far
    // harsher than film — and currently scores 17.4 dB (left) and 18.0 dB
    // (right) with the default Offset reconstruction.
    //
    // Scale scores about 3 dB higher here, and is still the better choice on a
    // clean source. It is not the default because this scene does not model
    // what actually breaks on real film: compressed, grainy, cyan-cast shadows,
    // where Scale's divide turns noise into visible speckle and Offset does not.
    // Synthetic PSNR is the wrong judge of that, so the lower number wins.
    let (left, right) = (render_eye(0), render_eye(1));
    let anaglyph = encode_anaglyph(&left, &right, AnaglyphFormat::RedCyan);
    let pair = process_frame(
        Sources {
            primary: &anaglyph,
            right_eye: None,
            colour: Some(&left),
            mono: None,
        },
        &params_with_reference(AnaglyphFormat::RedCyan),
    );
    let (l, r) = (psnr(&pair.left, &left), psnr(&pair.right, &right));
    eprintln!("RC defaults — left {l:.1} dB, right {r:.1} dB");
    assert!(l >= 15.5, "left eye regressed to {l:.1} dB");
    assert!(r >= 16.0, "right eye regressed to {r:.1} dB");
}

#[test]
fn recovery_still_works_without_any_colour_reference() {
    // The common case: no 2D release exists, so the anaglyph colours itself.
    let (left, right) = (render_eye(0), render_eye(1));
    let anaglyph = encode_anaglyph(&left, &right, AnaglyphFormat::RedCyan);
    let pair = process_frame(
        Sources::from_anaglyph(&anaglyph),
        &params_with_reference(AnaglyphFormat::RedCyan),
    );

    dump("rc_selfcolour_left", &pair.left);
    dump("rc_selfcolour_right", &pair.right);

    // Without a reference the eye's green and blue are simply unknown — only
    // its red channel survived the anaglyph — so luma cannot reach the ~30 dB
    // a real reference buys. What must still hold is that the output is a sane
    // full-colour frame carrying the eye's own signal intact.
    let luma = luma_psnr(&pair.left, &left);
    eprintln!("RC self-coloured — left luma {luma:.1} dB");
    assert!(
        pair.left.as_slice().iter().all(|s| s.is_finite()),
        "self-coloured output must still be a valid frame"
    );
    assert!(luma >= 16.0, "self-coloured luma regressed to {luma:.1} dB");
}

#[test]
fn a_mono_source_gives_that_eye_back_untouched() {
    let (left, right) = (render_eye(0), render_eye(1));
    let anaglyph = encode_anaglyph(&left, &right, AnaglyphFormat::RedCyan);
    let pair = process_frame(
        Sources {
            primary: &anaglyph,
            right_eye: None,
            colour: Some(&left),
            mono: Some(&left),
        },
        &ConvertParams {
            mono_eye: MonoEye::Left,
            ..params_with_reference(AnaglyphFormat::RedCyan)
        },
    );
    assert_eq!(
        pair.left.as_slice(),
        left.as_slice(),
        "a supplied 2D eye must pass through bit for bit"
    );
}

#[test]
fn cross_talk_correction_recovers_a_deliberately_leaky_anaglyph() {
    // Ghosting happens in the *channels*, not in the images: an imperfect red
    // filter passes a little green, so each eye picks up the other's projected
    // signal. Mixing the full-colour images before encoding instead would
    // contaminate the red channel with the right eye's red — a different
    // quantity entirely, and not one this correction claims to undo.
    let (left, right) = (render_eye(0), render_eye(1));
    let leak = 0.15;
    let clean = encode_anaglyph(&left, &right, AnaglyphFormat::RedCyan);
    let (cr, cg, cb) = clean.rgb_planes();
    let leaky_r: Vec<f32> = cr
        .iter()
        .zip(cg)
        .map(|(&r, &g)| r * (1.0 - leak) + g * leak)
        .collect();
    let leaky_g: Vec<f32> = cg
        .iter()
        .zip(cr)
        .map(|(&g, &r)| g * (1.0 - leak) + r * leak)
        .collect();
    let anaglyph = FrameF32::from_rgb_planes(WIDTH, HEIGHT, &leaky_r, &leaky_g, cb);

    let base = params_with_reference(AnaglyphFormat::RedCyan);
    let uncorrected = process_frame(
        Sources {
            primary: &anaglyph,
            right_eye: None,
            colour: Some(&left),
            mono: None,
        },
        &base,
    );
    let corrected = process_frame(
        Sources {
            primary: &anaglyph,
            right_eye: None,
            colour: Some(&left),
            mono: None,
        },
        &ConvertParams {
            leak_correct_left: leak * 100.0,
            leak_correct_right: leak * 100.0,
            ..base
        },
    );

    let before = luma_psnr(&uncorrected.right, &right);
    let after = luma_psnr(&corrected.right, &right);
    eprintln!("leaky master — right eye luma {before:.1} dB uncorrected, {after:.1} dB corrected");
    assert!(
        after > before,
        "correction must improve a leaky source: {after:.1} vs {before:.1} dB"
    );
}
