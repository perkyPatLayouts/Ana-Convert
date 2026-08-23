// Ana-Convert — anaglyph 3D to full-colour stereo recovery
// SPDX-License-Identifier: GPL-3.0-or-later

//! Per-eye brightness, contrast and saturation.
//!
//! Recovery routinely leaves one eye darker or flatter than the other — the
//! two anaglyph channels never carried equal energy to begin with — so each
//! eye gets its own correction.
//!
//! Unlike the rest of the pipeline this stage runs on *gamma-encoded* samples,
//! matching where `Tweak` sat in the original script. Brightness as a linear
//! -light offset would crush shadows, and it would throw away the feel every
//! existing per-movie parameter was dialled in against.

use crate::frame::FrameF32;
use crate::transfer::luminance;

/// Contrast pivots here, so mid-grey stays put as contrast changes.
const CONTRAST_PIVOT: f32 = 0.5;

/// One eye's correction. Defaults are the no-op.
#[derive(Debug, Clone, Copy, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub struct Grade {
    /// Added to every channel. Normalised, so the original's -255..255 becomes
    /// -1.0..1.0.
    pub brightness: f32,
    /// Multiplier about mid-grey. 1.0 is unchanged.
    pub contrast: f32,
    /// Multiplier on the distance from neutral. 1.0 is unchanged, 0.0 is grey.
    pub saturation: f32,
}

impl Default for Grade {
    fn default() -> Self {
        Self {
            brightness: 0.0,
            contrast: 1.0,
            saturation: 1.0,
        }
    }
}

impl Grade {
    /// True when the grade would leave every sample untouched.
    pub fn is_identity(&self) -> bool {
        self.brightness == 0.0 && self.contrast == 1.0 && self.saturation == 1.0
    }
}

/// Applies contrast, then brightness, then saturation, in place.
///
/// The order matches the original: contrast scales about mid-grey before the
/// brightness offset shifts the result. Nothing is clamped — headroom is kept
/// until output quantisation.
pub fn apply_grade(frame: &mut FrameF32, grade: &Grade) {
    assert_eq!(frame.channels(), 3, "grading operates on RGB frames");
    if grade.is_identity() {
        return;
    }

    let len = frame.plane_len();
    let (r, rest) = frame.as_mut_slice().split_at_mut(len);
    let (g, b) = rest.split_at_mut(len);

    for i in 0..len {
        let mut px = [r[i], g[i], b[i]];

        if grade.contrast != 1.0 {
            for c in &mut px {
                *c = (*c - CONTRAST_PIVOT) * grade.contrast + CONTRAST_PIVOT;
            }
        }
        if grade.brightness != 0.0 {
            for c in &mut px {
                *c += grade.brightness;
            }
        }
        if grade.saturation != 1.0 {
            let grey = luminance(px[0], px[1], px[2]);
            for c in &mut px {
                *c = grey + (*c - grey) * grade.saturation;
            }
        }

        [r[i], g[i], b[i]] = px;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn grade_one(c: [f32; 3], grade: &Grade) -> [f32; 3] {
        let mut frame = FrameF32::from_rgb_planes(1, 1, &[c[0]], &[c[1]], &[c[2]]);
        apply_grade(&mut frame, grade);
        let (r, g, b) = frame.rgb_planes();
        [r[0], g[0], b[0]]
    }

    fn assert_close(actual: [f32; 3], expected: [f32; 3], what: &str) {
        for c in 0..3 {
            assert!(
                (actual[c] - expected[c]).abs() < 1e-5,
                "{what}: channel {c} expected {}, got {} (full {actual:?})",
                expected[c],
                actual[c]
            );
        }
    }

    #[test]
    fn the_default_grade_is_a_no_op() {
        let grade = Grade::default();
        assert!(grade.is_identity());
        assert_close(
            grade_one([0.8, 0.4, 0.2], &grade),
            [0.8, 0.4, 0.2],
            "default",
        );
    }

    #[test]
    fn brightness_offsets_every_channel_equally() {
        let grade = Grade {
            brightness: 0.1,
            ..Default::default()
        };
        assert_close(
            grade_one([0.8, 0.4, 0.2], &grade),
            [0.9, 0.5, 0.3],
            "brightness",
        );
    }

    #[test]
    fn negative_brightness_darkens() {
        let grade = Grade {
            brightness: -0.2,
            ..Default::default()
        };
        assert_close(
            grade_one([0.5, 0.5, 0.5], &grade),
            [0.3, 0.3, 0.3],
            "darken",
        );
    }

    #[test]
    fn contrast_pivots_around_mid_grey() {
        let grade = Grade {
            contrast: 2.0,
            ..Default::default()
        };
        assert_close(
            grade_one([0.5, 0.5, 0.5], &grade),
            [0.5, 0.5, 0.5],
            "pivot holds",
        );
        assert_close(
            grade_one([0.6, 0.4, 0.5], &grade),
            [0.7, 0.3, 0.5],
            "expansion",
        );
    }

    #[test]
    fn zero_contrast_flattens_everything_to_mid_grey() {
        let grade = Grade {
            contrast: 0.0,
            ..Default::default()
        };
        assert_close(grade_one([0.9, 0.1, 0.4], &grade), [0.5, 0.5, 0.5], "flat");
    }

    #[test]
    fn zero_saturation_leaves_neutral_grey_at_the_same_luminance() {
        let grade = Grade {
            saturation: 0.0,
            ..Default::default()
        };
        let out = grade_one([0.8, 0.4, 0.2], &grade);
        let grey = luminance(0.8, 0.4, 0.2);
        assert_close(out, [grey, grey, grey], "desaturated");
    }

    #[test]
    fn saturation_scales_the_distance_from_neutral() {
        let grade = Grade {
            saturation: 2.0,
            ..Default::default()
        };
        let out = grade_one([0.8, 0.4, 0.2], &grade);
        let grey = luminance(0.8, 0.4, 0.2);
        assert_close(
            out,
            [
                grey + 2.0 * (0.8 - grey),
                grey + 2.0 * (0.4 - grey),
                grey + 2.0 * (0.2 - grey),
            ],
            "boosted",
        );
    }

    #[test]
    fn saturation_does_not_change_luminance() {
        for sat in [0.0, 0.5, 1.4, 2.0] {
            let grade = Grade {
                saturation: sat,
                ..Default::default()
            };
            let out = grade_one([0.8, 0.4, 0.2], &grade);
            let before = luminance(0.8, 0.4, 0.2);
            let after = luminance(out[0], out[1], out[2]);
            assert!(
                (before - after).abs() < 1e-5,
                "saturation {sat} moved luminance {before} -> {after}"
            );
        }
    }

    #[test]
    fn contrast_is_applied_before_brightness() {
        // Reversing the order would give 0.9 rather than 0.8 here.
        let grade = Grade {
            brightness: 0.1,
            contrast: 2.0,
            saturation: 1.0,
        };
        let out = grade_one([0.6, 0.6, 0.6], &grade);
        assert_close(out, [0.8, 0.8, 0.8], "contrast then brightness");
    }

    #[test]
    fn grading_keeps_headroom_instead_of_clamping() {
        let grade = Grade {
            brightness: 0.5,
            ..Default::default()
        };
        let out = grade_one([0.9, 0.9, 0.9], &grade);
        assert!(out[0] > 1.0, "expected headroom above white, got {out:?}");
    }

    #[test]
    fn an_identity_grade_leaves_a_whole_frame_untouched() {
        let original = FrameF32::from_rgb_planes(3, 1, &[0.1, 0.5, 0.9], &[0.2; 3], &[0.3; 3]);
        let mut frame = original.clone();
        apply_grade(&mut frame, &Grade::default());
        assert_eq!(frame.as_slice(), original.as_slice());
    }

    #[test]
    fn grading_applies_across_a_whole_plane() {
        let mut frame = FrameF32::from_rgb_planes(2, 1, &[0.1, 0.5], &[0.1, 0.5], &[0.1, 0.5]);
        apply_grade(
            &mut frame,
            &Grade {
                brightness: 0.25,
                ..Default::default()
            },
        );
        let r = frame.plane(0);
        assert!(
            (r[0] - 0.35).abs() < 1e-5 && (r[1] - 0.75).abs() < 1e-5,
            "got {r:?}"
        );
    }

    #[test]
    fn geometry_is_preserved() {
        let mut frame = FrameF32::new_rgb(6, 4);
        apply_grade(
            &mut frame,
            &Grade {
                contrast: 1.5,
                ..Default::default()
            },
        );
        assert_eq!((frame.width(), frame.height(), frame.channels()), (6, 4, 3));
    }
}
