// Ana-Convert — anaglyph 3D to full-colour stereo recovery
// SPDX-License-Identifier: GPL-3.0-or-later

//! Painting colour back onto an extracted eye.
//!
//! Each eye survives the anaglyph as brightness only. Colour comes from a
//! heavily blurred reference — the 2D release if there is one, otherwise the
//! anaglyph itself — on the standing assumption that human vision reads
//! detail from luminance and is far more forgiving about where colour sits.
//!
//! The original script did this with AviSynth's `MergeChroma` in YV12, which
//! forced a trip through subsampled chroma. Working in linear-light float
//! instead makes two genuinely different reconstructions available, and they
//! disagree most in shadows and on saturated colours.

use crate::extract::EyeProjection;
use crate::frame::FrameF32;

/// Guards the shadow divide and sets how fast the ratio method falls back to
/// neutral grey where the colour reference has no light to scale.
const SHADOW_FLOOR: f32 = 1e-4;

/// How to reconcile the reference colour with the eye's surviving signal.
///
/// Both modes drive the reference until its own projected value matches the
/// eye's, and both are exact when the reference is perfect — they differ only
/// in how they distribute the correction across the three channels.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
#[derive(Default)]
pub enum ColourRestore {
    /// Multiply, preserving the reference's chromaticity exactly. Reads as
    /// "the same colour, brighter or darker".
    #[default]
    Scale,
    /// Add, preserving the reference's colour *differences* — the linear-light
    /// descendant of the original's `MergeChroma`. Holds saturation better in
    /// shadows and can drive channels negative in bright, saturated areas.
    Offset,
}

/// Rebuilds a full-colour eye from its surviving signal and a colour reference.
///
/// `projection` must be the same one [`crate::extract`] used to produce `eye` —
/// that shared definition is what makes the reconstruction exact when the
/// reference is accurate. Both inputs must be linear light and the same size.
/// The result is deliberately unclamped; headroom survives until output.
pub fn restore_colour(
    eye: &FrameF32,
    colour: &FrameF32,
    projection: EyeProjection,
    mode: ColourRestore,
) -> FrameF32 {
    assert_eq!(eye.channels(), 1, "the eye must be a single plane");
    assert_eq!(colour.channels(), 3, "the colour reference must be RGB");
    assert!(
        eye.same_size(colour),
        "eye and colour reference must share geometry"
    );

    let (w, h) = (eye.width(), eye.height());
    let signal = eye.plane(0);
    let (cr, cg, cb) = colour.rgb_planes();
    let mut out = FrameF32::new_rgb(w, h);
    let len = out.plane_len();
    let (r, rest) = out.as_mut_slice().split_at_mut(len);
    let (g, b) = rest.split_at_mut(len);

    // Left sequential deliberately. This loop is memory-bandwidth bound, not
    // compute bound — spreading it over rayon measured no faster on a 10-core
    // machine and cost a good deal of clarity.
    match mode {
        ColourRestore::Scale => {
            for i in 0..len {
                // Lifting both sides by SHADOW_FLOOR keeps the divide finite and
                // decays smoothly to neutral grey as the reference darkens,
                // instead of letting black swallow the eye's detail. Because
                // the projection's weights sum to one, the lift cancels and the
                // result still projects back to exactly `signal[i]`.
                let reference = projection.apply(cr[i], cg[i], cb[i]);
                let scale = signal[i] / (reference + SHADOW_FLOOR);
                r[i] = (cr[i] + SHADOW_FLOOR) * scale;
                g[i] = (cg[i] + SHADOW_FLOOR) * scale;
                b[i] = (cb[i] + SHADOW_FLOOR) * scale;
            }
        }
        ColourRestore::Offset => {
            for i in 0..len {
                let reference = projection.apply(cr[i], cg[i], cb[i]);
                let offset = signal[i] - reference;
                r[i] = cr[i] + offset;
                g[i] = cg[i] + offset;
                b[i] = cb[i] + offset;
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::extract::{projections, AnaglyphFormat};

    const BOTH: [ColourRestore; 2] = [ColourRestore::Scale, ColourRestore::Offset];

    fn rc_left() -> EyeProjection {
        projections(AnaglyphFormat::RedCyan).0
    }

    fn restore_one(
        signal: f32,
        c: [f32; 3],
        projection: EyeProjection,
        mode: ColourRestore,
    ) -> [f32; 3] {
        let eye = FrameF32::from_planar(1, 1, 1, vec![signal]);
        let colour = FrameF32::from_rgb_planes(1, 1, &[c[0]], &[c[1]], &[c[2]]);
        let out = restore_colour(&eye, &colour, projection, mode);
        let (r, g, b) = out.rgb_planes();
        [r[0], g[0], b[0]]
    }

    #[test]
    fn output_always_projects_back_to_the_eye_signal() {
        // The defining contract. Extraction read this value out of the
        // anaglyph; restoration must put a frame back that would yield it
        // again, or the recovered eye is simply the wrong brightness.
        for format in [
            AnaglyphFormat::RedCyan,
            AnaglyphFormat::GreenMagenta,
            AnaglyphFormat::RedBlue,
        ] {
            for (name, p) in [
                ("left", projections(format).0),
                ("right", projections(format).1),
            ] {
                for mode in BOTH {
                    for signal in [0.05, 0.4, 0.95] {
                        let out = restore_one(signal, [0.8, 0.4, 0.2], p, mode);
                        let back = p.apply(out[0], out[1], out[2]);
                        assert!(
                            (back - signal).abs() < 1e-4,
                            "{format:?} {name} {mode:?}: {signal} came back as {back}"
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn a_perfect_reference_reconstructs_the_original_exactly() {
        // With an accurate colour reference the eye's own channel already
        // agrees with it, so nothing should move at all.
        let truth = [0.85, 0.15, 0.15];
        for mode in BOTH {
            let p = rc_left();
            let signal = p.apply(truth[0], truth[1], truth[2]);
            let out = restore_one(signal, truth, p, mode);
            for c in 0..3 {
                assert!(
                    (out[c] - truth[c]).abs() < 1e-3,
                    "{mode:?} channel {c}: expected {}, got {} ({out:?})",
                    truth[c],
                    out[c]
                );
            }
        }
    }

    #[test]
    fn a_saturated_colour_is_not_rendered_at_channel_brightness() {
        // The bug the ground-truth test caught: treating an eye's red channel
        // as its luminance renders a red box several times too bright.
        let truth = [0.85, 0.15, 0.15];
        let p = rc_left();
        let out = restore_one(
            p.apply(truth[0], truth[1], truth[2]),
            truth,
            p,
            ColourRestore::Scale,
        );
        assert!(
            (out[1] - 0.15).abs() < 1e-3,
            "green must stay dark in a red object, got {out:?}"
        );
    }

    #[test]
    fn scale_mode_preserves_the_reference_chromaticity() {
        let out = restore_one(0.4, [0.8, 0.4, 0.2], rc_left(), ColourRestore::Scale);
        assert!(
            (out[0] / out[1] - 2.0).abs() < 1e-3 && (out[1] / out[2] - 2.0).abs() < 1e-3,
            "channel ratios must stay 4:2:1, got {out:?}"
        );
    }

    #[test]
    fn scale_mode_is_linear_in_the_eye_signal() {
        let dim = restore_one(0.2, [0.8, 0.4, 0.2], rc_left(), ColourRestore::Scale);
        let bright = restore_one(0.4, [0.8, 0.4, 0.2], rc_left(), ColourRestore::Scale);
        for c in 0..3 {
            assert!(
                (bright[c] - 2.0 * dim[c]).abs() < 1e-4,
                "channel {c}: {} vs 2x{}",
                bright[c],
                dim[c]
            );
        }
    }

    #[test]
    fn offset_mode_preserves_colour_differences() {
        let c = [0.8, 0.4, 0.2];
        let out = restore_one(0.5, c, rc_left(), ColourRestore::Offset);
        assert!(
            ((out[0] - out[1]) - (c[0] - c[1])).abs() < 1e-5,
            "differences must survive, got {out:?}"
        );
    }

    #[test]
    fn the_two_modes_disagree_away_from_the_exact_case() {
        let scale = restore_one(0.4, [0.8, 0.4, 0.2], rc_left(), ColourRestore::Scale);
        let offset = restore_one(0.4, [0.8, 0.4, 0.2], rc_left(), ColourRestore::Offset);
        assert!(
            (scale[1] - offset[1]).abs() > 0.01,
            "modes should not coincide: {scale:?} vs {offset:?}"
        );
    }

    #[test]
    fn a_black_reference_falls_back_to_neutral_grey() {
        let out = restore_one(0.4, [0.0, 0.0, 0.0], rc_left(), ColourRestore::Scale);
        for c in 0..3 {
            assert!(
                (out[c] - 0.4).abs() < 1e-2,
                "black reference must not swallow the eye, got {out:?}"
            );
        }
    }

    #[test]
    fn a_black_reference_produces_no_nan_or_infinity() {
        for mode in BOTH {
            let out = restore_one(0.4, [0.0, 0.0, 0.0], rc_left(), mode);
            assert!(
                out.iter().all(|v| v.is_finite()),
                "{mode:?} produced {out:?}"
            );
        }
    }

    #[test]
    fn a_black_eye_produces_black_output_in_scale_mode() {
        let out = restore_one(0.0, [0.8, 0.4, 0.2], rc_left(), ColourRestore::Scale);
        assert!(out.iter().all(|v| v.abs() < 1e-3), "got {out:?}");
    }

    #[test]
    fn highlights_keep_their_headroom_rather_than_clipping() {
        let out = restore_one(1.5, [0.8, 0.8, 0.8], rc_left(), ColourRestore::Scale);
        assert!(
            out[0] > 1.0,
            "restore must not clamp; output quantisation does that. Got {out:?}"
        );
    }

    #[test]
    fn geometry_and_channel_count_are_correct() {
        let eye = FrameF32::new_grey(5, 4);
        let colour = FrameF32::new_rgb(5, 4);
        let out = restore_colour(&eye, &colour, rc_left(), ColourRestore::Scale);
        assert_eq!((out.width(), out.height(), out.channels()), (5, 4, 3));
    }

    #[test]
    fn restoration_runs_per_pixel_across_a_plane() {
        let eye = FrameF32::from_planar(2, 1, 1, vec![0.2, 0.8]);
        let colour = FrameF32::from_rgb_planes(2, 1, &[0.5, 0.5], &[0.5, 0.5], &[0.5, 0.5]);
        let out = restore_colour(&eye, &colour, rc_left(), ColourRestore::Scale);
        let r = out.plane(0);
        assert!(
            (r[0] - 0.2).abs() < 1e-3 && (r[1] - 0.8).abs() < 1e-3,
            "got {r:?}"
        );
    }
}
