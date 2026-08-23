// Ana-Convert — anaglyph 3D to full-colour stereo recovery
// SPDX-License-Identifier: GPL-3.0-or-later

//! Cross-talk (ghosting) correction between the two extracted eyes.
//!
//! Anaglyph mastering always leaks a little of each eye into the other, which
//! shows as a ghost of the opposite view. Correction subtracts a fraction of
//! the opposite eye and then rescales back to full range.
//!
//! The rescale divisor assumes the two eyes are highly correlated — which they
//! are, being two views of the same scene — so a pixel where both eyes are at
//! full white returns to full white. Uncorrelated pixels can land above 1.0;
//! that headroom is deliberate and is clamped only at output.

use crate::frame::FrameF32;

/// Smallest divisor allowed when rescaling, so a leak of 100% cannot produce
/// infinities or NaNs.
const MIN_DIVISOR: f32 = 1e-3;

/// Subtracts cross-talk between the eyes in place.
///
/// `leak_left` is the fraction of the right eye bleeding into the left, and
/// `leak_right` the reverse. Both are fractions, not percentages.
///
/// Order matters and matches the original AviSynth script: the left eye is
/// corrected first, and the right eye is then corrected against the *already
/// corrected* left.
pub fn correct_crosstalk(
    left: &mut FrameF32,
    right: &mut FrameF32,
    leak_left: f32,
    leak_right: f32,
) {
    subtract_and_rescale(left, right, leak_left);
    // Deliberately reads `left` after it has been corrected.
    subtract_and_rescale(right, left, leak_right);
}

/// `target = clamp0(target - leak * other) / (1 - leak)`, applied in place.
fn subtract_and_rescale(target: &mut FrameF32, other: &FrameF32, leak: f32) {
    if leak == 0.0 {
        return;
    }
    assert!(
        target.same_size(other),
        "cross-talk correction needs matching geometry"
    );
    let scale = 1.0 / (1.0 - leak).max(MIN_DIVISOR);
    let other = other.plane(0);
    for (t, &o) in target.plane_mut(0).iter_mut().zip(other) {
        *t = (*t - leak * o).max(0.0) * scale;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn grey(values: &[f32]) -> FrameF32 {
        FrameF32::from_planar(values.len(), 1, 1, values.to_vec())
    }

    fn correct(l: f32, r: f32, leak_left: f32, leak_right: f32) -> (f32, f32) {
        let (mut left, mut right) = (grey(&[l]), grey(&[r]));
        correct_crosstalk(&mut left, &mut right, leak_left, leak_right);
        (left.plane(0)[0], right.plane(0)[0])
    }

    fn assert_close(actual: f32, expected: f32, what: &str) {
        assert!(
            (actual - expected).abs() < 1e-6,
            "{what}: expected {expected}, got {actual}"
        );
    }

    #[test]
    fn zero_leak_leaves_both_eyes_untouched() {
        let (l, r) = correct(0.6, 0.4, 0.0, 0.0);
        assert_close(l, 0.6, "left");
        assert_close(r, 0.4, "right");
    }

    #[test]
    fn left_loses_a_fraction_of_the_right_eye_then_rescales() {
        let (l, _) = correct(0.5, 0.4, 0.1, 0.0);
        assert_close(l, (0.5 - 0.1 * 0.4) / 0.9, "left after 10% correction");
    }

    #[test]
    fn right_loses_a_fraction_of_the_left_eye_then_rescales() {
        let (_, r) = correct(0.4, 0.5, 0.0, 0.1);
        assert_close(r, (0.5 - 0.1 * 0.4) / 0.9, "right after 10% correction");
    }

    #[test]
    fn correlated_content_keeps_its_full_range() {
        // Both eyes at full white must stay at full white, otherwise correction
        // would darken every bright scene.
        let (l, r) = correct(1.0, 1.0, 0.2, 0.3);
        assert_close(l, 1.0, "left white");
        assert_close(r, 1.0, "right white");
    }

    #[test]
    fn right_is_corrected_against_the_already_corrected_left() {
        // This ordering comes straight from the original script. Using the
        // uncorrected left here would give 0.475 instead.
        let (_, r) = correct(0.6, 0.5, 0.2, 0.2);
        let corrected_left = (0.6 - 0.2 * 0.5) / 0.8;
        assert_close(r, (0.5 - 0.2 * corrected_left) / 0.8, "right");
    }

    #[test]
    fn subtraction_clamps_at_black_rather_than_going_negative() {
        let (l, _) = correct(0.1, 1.0, 0.5, 0.0);
        assert_close(l, 0.0, "left driven below black");
    }

    #[test]
    fn negative_leak_adds_the_opposite_eye_back() {
        // The original allowed -100..100; a negative value mixes the other eye
        // in rather than out, which helps some badly mastered sources.
        let (l, _) = correct(0.4, 0.2, -0.25, 0.0);
        assert_close(l, (0.4 + 0.25 * 0.2) / 1.25, "left with negative leak");
    }

    #[test]
    fn total_leak_stays_finite() {
        let (l, r) = correct(0.5, 0.5, 1.0, 1.0);
        assert!(l.is_finite() && r.is_finite(), "got left={l}, right={r}");
    }

    #[test]
    fn correction_applies_across_the_whole_plane() {
        let (mut left, mut right) = (grey(&[0.5, 0.5, 0.5]), grey(&[0.0, 0.5, 1.0]));
        correct_crosstalk(&mut left, &mut right, 0.2, 0.0);
        let l = left.plane(0);
        assert!(
            l[0] > l[1] && l[1] > l[2],
            "more right-eye signal must remove more from the left: {l:?}"
        );
    }

    #[test]
    fn geometry_is_preserved() {
        let (mut left, mut right) = (FrameF32::new_grey(4, 3), FrameF32::new_grey(4, 3));
        correct_crosstalk(&mut left, &mut right, 0.1, 0.1);
        assert_eq!((left.width(), left.height()), (4, 3));
        assert_eq!((right.width(), right.height()), (4, 3));
    }
}
