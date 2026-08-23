// Ana-Convert — anaglyph 3D to full-colour stereo recovery
// SPDX-License-Identifier: GPL-3.0-or-later

//! Opto-electronic transfer functions.
//!
//! Cross-talk subtraction and blur are physically additive operations on light,
//! so the pipeline converts to linear light before doing either. Decoded video
//! arrives gamma-encoded; these functions move between the two.
//!
//! All conversions are sign-preserving (odd functions): intermediate stages can
//! carry values below zero after a subtraction, and those must survive a
//! round-trip rather than being silently clamped here.

use crate::frame::FrameF32;
use rayon::prelude::*;

/// Which transfer function the encoded samples use.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
#[derive(Default)]
pub enum TransferFunction {
    /// IEC 61966-2-1. The default: consumer rips are judged on sRGB displays.
    #[default]
    Srgb,
    /// ITU-R BT.709 camera OETF.
    Bt709,
    /// No conversion — samples are already linear light.
    Linear,
}

impl TransferFunction {
    /// Converts one gamma-encoded sample to linear light.
    pub fn to_linear(self, encoded: f32) -> f32 {
        odd(encoded, |v| match self {
            Self::Linear => v,
            Self::Srgb => {
                if v <= 0.040_448_237 {
                    v / 12.92
                } else {
                    ((v + 0.055) / 1.055).powf(2.4)
                }
            }
            Self::Bt709 => {
                if v < 0.081 {
                    v / 4.5
                } else {
                    ((v + 0.099) / 1.099).powf(1.0 / 0.45)
                }
            }
        })
    }

    /// Converts one linear-light sample to gamma-encoded.
    pub fn from_linear(self, linear: f32) -> f32 {
        odd(linear, |v| match self {
            Self::Linear => v,
            Self::Srgb => {
                if v <= 0.003_130_8 {
                    v * 12.92
                } else {
                    1.055 * v.powf(1.0 / 2.4) - 0.055
                }
            }
            Self::Bt709 => {
                if v < 0.018 {
                    v * 4.5
                } else {
                    1.099 * v.powf(0.45) - 0.099
                }
            }
        })
    }

    /// True when the function is a no-op, so callers can skip whole passes.
    pub fn is_identity(self) -> bool {
        self == Self::Linear
    }
}

/// Applies `f` with odd symmetry, so negative headroom survives the conversion.
fn odd(value: f32, f: impl Fn(f32) -> f32) -> f32 {
    if value < 0.0 {
        -f(-value)
    } else {
        f(value)
    }
}

/// Converts a whole frame to linear light in place.
pub fn to_linear_frame(frame: &mut FrameF32, tf: TransferFunction) {
    if tf.is_identity() {
        return;
    }
    frame
        .as_mut_slice()
        .par_iter_mut()
        .for_each(|s| *s = tf.to_linear(*s));
}

/// Converts a whole frame back to gamma-encoded in place.
pub fn from_linear_frame(frame: &mut FrameF32, tf: TransferFunction) {
    if tf.is_identity() {
        return;
    }
    frame
        .as_mut_slice()
        .par_iter_mut()
        .for_each(|s| *s = tf.from_linear(*s));
}

/// Rec.709 luma weights, used wherever a single brightness figure is needed.
pub const LUMA_R: f32 = 0.2126;
pub const LUMA_G: f32 = 0.7152;
pub const LUMA_B: f32 = 0.0722;

/// Relative luminance of a linear-light RGB triple.
pub fn luminance(r: f32, g: f32, b: f32) -> f32 {
    LUMA_R * r + LUMA_G * g + LUMA_B * b
}

#[cfg(test)]
mod tests {
    use super::*;

    const EPS: f32 = 1e-5;

    fn assert_close(actual: f32, expected: f32, what: &str) {
        assert!(
            (actual - expected).abs() < EPS,
            "{what}: expected {expected}, got {actual}"
        );
    }

    #[test]
    fn srgb_maps_endpoints_to_themselves() {
        assert_close(TransferFunction::Srgb.to_linear(0.0), 0.0, "black");
        assert_close(TransferFunction::Srgb.to_linear(1.0), 1.0, "white");
    }

    #[test]
    fn srgb_mid_grey_is_about_twenty_one_percent_light() {
        // The defining property of a gamma-encoded signal: perceptual mid-grey
        // carries roughly a fifth of the light of white.
        assert_close(
            TransferFunction::Srgb.to_linear(0.5),
            0.214_041,
            "0.5 encoded",
        );
    }

    #[test]
    fn srgb_uses_the_linear_toe_below_the_breakpoint() {
        assert_close(
            TransferFunction::Srgb.to_linear(0.02),
            0.02 / 12.92,
            "toe segment",
        );
    }

    #[test]
    fn bt709_mid_grey_differs_from_srgb() {
        let bt = TransferFunction::Bt709.to_linear(0.5);
        assert_close(bt, 0.259_589, "0.5 encoded under bt709");
        assert!(
            (bt - TransferFunction::Srgb.to_linear(0.5)).abs() > 0.04,
            "the two curves must not be interchangeable"
        );
    }

    #[test]
    fn bt709_uses_the_linear_toe_below_the_breakpoint() {
        assert_close(TransferFunction::Bt709.from_linear(0.01), 0.045, "toe");
    }

    #[test]
    fn linear_transfer_is_the_identity() {
        for v in [-2.0, 0.0, 0.37, 1.0, 4.2] {
            assert_close(TransferFunction::Linear.to_linear(v), v, "to_linear");
            assert_close(TransferFunction::Linear.from_linear(v), v, "from_linear");
        }
        assert!(TransferFunction::Linear.is_identity());
    }

    #[test]
    fn every_curve_round_trips_within_tolerance() {
        for tf in [
            TransferFunction::Srgb,
            TransferFunction::Bt709,
            TransferFunction::Linear,
        ] {
            for step in 0..=100 {
                let encoded = step as f32 / 100.0;
                let back = tf.from_linear(tf.to_linear(encoded));
                assert!(
                    (back - encoded).abs() < 1e-4,
                    "{tf:?} round trip at {encoded}: got {back}"
                );
            }
        }
    }

    #[test]
    fn negative_headroom_survives_conversion() {
        // Cross-talk subtraction can drive samples below zero before rescaling;
        // clamping here would destroy information the next stage still needs.
        let tf = TransferFunction::Srgb;
        assert_close(tf.to_linear(-0.5), -tf.to_linear(0.5), "odd symmetry");
        assert_close(
            tf.from_linear(tf.to_linear(-0.5)),
            -0.5,
            "negative round trip",
        );
    }

    #[test]
    fn above_white_headroom_survives_conversion() {
        let tf = TransferFunction::Srgb;
        assert!(tf.to_linear(1.5) > 1.0, "superwhite must stay above white");
        assert_close(
            tf.from_linear(tf.to_linear(1.5)),
            1.5,
            "superwhite round trip",
        );
    }

    #[test]
    fn frame_conversion_changes_samples_and_round_trips() {
        let original = FrameF32::from_rgb_planes(2, 1, &[0.5, 0.2], &[0.8, 0.1], &[0.3, 0.9]);
        let mut frame = original.clone();

        to_linear_frame(&mut frame, TransferFunction::Srgb);
        assert!(
            frame.plane(0)[0] < original.plane(0)[0] - 0.1,
            "linearising must actually darken mid tones, got {}",
            frame.plane(0)[0]
        );

        from_linear_frame(&mut frame, TransferFunction::Srgb);
        for (a, b) in frame.as_slice().iter().zip(original.as_slice()) {
            assert!((a - b).abs() < 1e-4, "frame round trip: {a} vs {b}");
        }
    }

    #[test]
    fn frame_conversion_is_skipped_for_linear() {
        let mut frame = FrameF32::filled(2, 2, 3, 0.5);
        to_linear_frame(&mut frame, TransferFunction::Linear);
        assert!(frame.as_slice().iter().all(|&s| s == 0.5));
    }

    #[test]
    fn luminance_weights_sum_to_one() {
        assert_close(luminance(1.0, 1.0, 1.0), 1.0, "white luminance");
        assert!(
            luminance(0.0, 1.0, 0.0) > luminance(1.0, 0.0, 0.0),
            "green must carry more luminance than red"
        );
    }
}
