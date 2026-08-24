// Ana-Convert — anaglyph 3D to full-colour stereo recovery
// SPDX-License-Identifier: GPL-3.0-or-later

//! Taking apart a stereo pair that is already packed into one frame.
//!
//! The reverse of [`crate::compose`]. Useful in its own right — a side-by-side
//! file is the input when you want an anaglyph back out, or when you just want
//! one eye as a flat 2D file.
//!
//! The wrinkle is anamorphic packing. Broadcast and disc stereo usually squeeze
//! each eye to half width (or half height) so the pair fits in one ordinary
//! frame; split naively, everyone comes out half as wide as they should be.

use crate::compose::{resize, EyeOrder};
use crate::pipeline::StereoPair;
use crate::FrameF32;

/// How two eyes are arranged inside one frame.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum StereoPacking {
    /// Two eyes across the frame.
    #[default]
    SideBySide,
    /// Two eyes down the frame.
    TopBottom,
}

impl StereoPacking {
    pub fn label(self) -> &'static str {
        match self {
            Self::SideBySide => "Side by side",
            Self::TopBottom => "Top and bottom",
        }
    }
}

/// Splits a packed frame into its two eyes.
///
/// With `anamorphic`, each eye is stretched back to the full frame's size,
/// undoing the squeeze that let the pair share one frame. Without it each eye
/// keeps the dimensions it occupied, which is right for full-resolution packing.
pub fn split_packed(
    frame: &FrameF32,
    packing: StereoPacking,
    order: EyeOrder,
    anamorphic: bool,
) -> StereoPair {
    let (w, h) = (frame.width(), frame.height());
    let (half_w, half_h) = (w / 2, h / 2);

    // Both eyes get exactly half, and an odd frame loses the row or column
    // straddling the seam rather than handing back a mismatched pair. Everything
    // downstream — muxing back to an anaglyph, differencing the two views —
    // requires them to be the same size, and preview frames are scaled to
    // whatever a pane offers, so odd sizes turn up constantly.
    let (first, second) = match packing {
        StereoPacking::SideBySide => (
            crop(frame, 0, 0, half_w, h),
            crop(frame, w - half_w, 0, half_w, h),
        ),
        StereoPacking::TopBottom => (
            crop(frame, 0, 0, w, half_h),
            crop(frame, 0, h - half_h, w, half_h),
        ),
    };

    // Undo the squeeze that let two eyes share one frame.
    let (first, second) = if anamorphic {
        (resize(&first, w, h), resize(&second, w, h))
    } else {
        (first, second)
    };

    match order {
        EyeOrder::LeftFirst => StereoPair {
            left: first,
            right: second,
        },
        EyeOrder::RightFirst => StereoPair {
            left: second,
            right: first,
        },
    }
}

/// Lifts a rectangle out of a frame.
fn crop(frame: &FrameF32, x0: usize, y0: usize, width: usize, height: usize) -> FrameF32 {
    let (w, channels) = (frame.width(), frame.channels());
    let mut out = FrameF32::filled(width, height, channels, 0.0);
    for c in 0..channels {
        let src = frame.plane(c);
        let dst = out.plane_mut(c);
        for y in 0..height {
            let from = (y0 + y) * w + x0;
            dst[y * width..(y + 1) * width].copy_from_slice(&src[from..from + width]);
        }
    }
    out
}

/// The size each eye will have once split.
pub fn eye_size(frame: (usize, usize), packing: StereoPacking, anamorphic: bool) -> (usize, usize) {
    let (w, h) = frame;
    if anamorphic {
        return (w, h);
    }
    match packing {
        StereoPacking::SideBySide => (w / 2, h),
        StereoPacking::TopBottom => (w, h / 2),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::compose::{stack_horizontal, stack_vertical};

    /// A frame whose left and right halves are told apart by their red channel.
    fn halves(w: usize, h: usize, left: f32, right: f32) -> FrameF32 {
        let mut r = vec![0.0f32; w * h];
        for y in 0..h {
            for x in 0..w {
                r[y * w + x] = if x < w / 2 { left } else { right };
            }
        }
        FrameF32::from_rgb_planes(w, h, &r, &vec![0.5; w * h], &vec![0.25; w * h])
    }

    fn first(frame: &FrameF32) -> f32 {
        frame.plane(0)[0]
    }

    #[test]
    fn side_by_side_splits_across_the_middle() {
        let packed = halves(8, 4, 0.9, 0.1);
        let pair = split_packed(
            &packed,
            StereoPacking::SideBySide,
            EyeOrder::LeftFirst,
            false,
        );
        assert_eq!((pair.left.width(), pair.left.height()), (4, 4));
        assert_eq!(first(&pair.left), 0.9, "left eye came from the left half");
        assert_eq!(first(&pair.right), 0.1);
    }

    #[test]
    fn side_by_side_honours_a_reversed_eye_order() {
        let packed = halves(8, 4, 0.9, 0.1);
        let pair = split_packed(
            &packed,
            StereoPacking::SideBySide,
            EyeOrder::RightFirst,
            false,
        );
        assert_eq!(first(&pair.left), 0.1, "the right eye was stored first");
        assert_eq!(first(&pair.right), 0.9);
    }

    #[test]
    fn top_bottom_splits_across_the_middle() {
        // Build it by stacking, so the test does not depend on the split's own
        // idea of where the middle is.
        let top = FrameF32::filled(6, 3, 3, 0.8);
        let bottom = FrameF32::filled(6, 3, 3, 0.2);
        let packed = stack_vertical(&top, &bottom);
        let pair = split_packed(
            &packed,
            StereoPacking::TopBottom,
            EyeOrder::LeftFirst,
            false,
        );
        assert_eq!((pair.left.width(), pair.left.height()), (6, 3));
        assert_eq!(first(&pair.left), 0.8);
        assert_eq!(first(&pair.right), 0.2);
    }

    #[test]
    fn splitting_undoes_stacking_exactly() {
        // The strongest statement available: this is the inverse of compose.
        let left =
            FrameF32::from_rgb_planes(3, 2, &[0.1, 0.2, 0.3, 0.4, 0.5, 0.6], &[0.0; 6], &[0.0; 6]);
        let right =
            FrameF32::from_rgb_planes(3, 2, &[0.9, 0.8, 0.7, 0.6, 0.5, 0.4], &[0.0; 6], &[0.0; 6]);

        let pair = split_packed(
            &stack_horizontal(&left, &right),
            StereoPacking::SideBySide,
            EyeOrder::LeftFirst,
            false,
        );
        assert_eq!(pair.left.as_slice(), left.as_slice());
        assert_eq!(pair.right.as_slice(), right.as_slice());

        let pair = split_packed(
            &stack_vertical(&left, &right),
            StereoPacking::TopBottom,
            EyeOrder::LeftFirst,
            false,
        );
        assert_eq!(pair.left.as_slice(), left.as_slice());
        assert_eq!(pair.right.as_slice(), right.as_slice());
    }

    #[test]
    fn anamorphic_side_by_side_stretches_each_eye_back_to_full_width() {
        // A 1920x1080 frame holding two 960x1080 eyes: each one has to come
        // back out at 1920x1080 or everybody is half as wide as they should be.
        let packed = FrameF32::new_rgb(1920, 1080);
        let pair = split_packed(
            &packed,
            StereoPacking::SideBySide,
            EyeOrder::LeftFirst,
            true,
        );
        assert_eq!((pair.left.width(), pair.left.height()), (1920, 1080));
        assert_eq!((pair.right.width(), pair.right.height()), (1920, 1080));
    }

    #[test]
    fn anamorphic_top_bottom_stretches_each_eye_back_to_full_height() {
        let packed = FrameF32::new_rgb(1920, 1080);
        let pair = split_packed(&packed, StereoPacking::TopBottom, EyeOrder::LeftFirst, true);
        assert_eq!((pair.left.width(), pair.left.height()), (1920, 1080));
    }

    #[test]
    fn anamorphic_stretching_keeps_the_content_in_place() {
        // Stretching must not also shuffle which eye is which.
        let packed = halves(8, 4, 0.9, 0.1);
        let pair = split_packed(
            &packed,
            StereoPacking::SideBySide,
            EyeOrder::LeftFirst,
            true,
        );
        assert!(
            (first(&pair.left) - 0.9).abs() < 0.05,
            "got {}",
            first(&pair.left)
        );
        assert!(
            (first(&pair.right) - 0.1).abs() < 0.05,
            "got {}",
            first(&pair.right)
        );
    }

    #[test]
    fn a_flat_field_survives_the_anamorphic_stretch() {
        let packed = FrameF32::filled(64, 16, 3, 0.6);
        let pair = split_packed(
            &packed,
            StereoPacking::SideBySide,
            EyeOrder::LeftFirst,
            true,
        );
        for &s in pair.left.as_slice() {
            assert!((s - 0.6).abs() < 1e-4, "drifted to {s}");
        }
    }

    #[test]
    fn eye_size_matches_what_splitting_produces() {
        // The pipeline sizes its encoders from eye_size before decoding a
        // frame, so the two must never disagree.
        for packing in [StereoPacking::SideBySide, StereoPacking::TopBottom] {
            for anamorphic in [false, true] {
                let packed = FrameF32::new_rgb(64, 32);
                let pair = split_packed(&packed, packing, EyeOrder::LeftFirst, anamorphic);
                assert_eq!(
                    eye_size((64, 32), packing, anamorphic),
                    (pair.left.width(), pair.left.height()),
                    "{packing:?} anamorphic={anamorphic}"
                );
            }
        }
    }

    #[test]
    fn an_odd_size_still_splits_into_two_equal_eyes() {
        // The eyes must match, not merely both exist. An earlier version of
        // this test only checked they were non-empty, so a 601-wide frame
        // splitting into 300 and 301 went unnoticed until muxing the pair back
        // into an anaglyph asserted on the mismatch and took the app down.
        //
        // Preview frames are scaled to whatever width a pane happens to offer,
        // so odd sizes are routine rather than exotic.
        for w in [9usize, 599, 601, 1279] {
            let pair = split_packed(
                &FrameF32::new_rgb(w, 5),
                StereoPacking::SideBySide,
                EyeOrder::LeftFirst,
                false,
            );
            assert_eq!(
                pair.left.width(),
                pair.right.width(),
                "width {w} split unevenly"
            );
            assert!(pair.left.width() >= 1, "width {w} lost an eye entirely");
            assert_eq!(pair.left.height(), 5);
        }

        for h in [9usize, 599, 601] {
            let pair = split_packed(
                &FrameF32::new_rgb(5, h),
                StereoPacking::TopBottom,
                EyeOrder::LeftFirst,
                false,
            );
            assert_eq!(
                pair.left.height(),
                pair.right.height(),
                "height {h} split unevenly"
            );
        }
    }

    #[test]
    fn an_oddly_sized_pair_can_be_muxed_back_into_an_anaglyph() {
        // The exact path that crashed: split a packed frame, then re-mux it.
        use crate::extract::{encode_anaglyph, AnaglyphFormat};
        for w in [601usize, 1279] {
            let pair = split_packed(
                &FrameF32::new_rgb(w, 33),
                StereoPacking::SideBySide,
                EyeOrder::LeftFirst,
                false,
            );
            let out = encode_anaglyph(&pair.left, &pair.right, AnaglyphFormat::RedCyan);
            assert_eq!(out.height(), 33, "width {w}");
        }
    }

    #[test]
    fn an_odd_split_drops_the_seam_rather_than_the_picture_edge() {
        // With no clean middle, the ambiguous column is the one straddling the
        // seam. Losing that is far better than losing a column of picture.
        let mut r = vec![0.0f32; 5];
        r[0] = 0.1; // first column of the left eye
        r[2] = 0.5; // the straddling middle
        r[4] = 0.9; // last column of the right eye
        let packed = FrameF32::from_rgb_planes(5, 1, &r, &[0.0; 5], &[0.0; 5]);
        let pair = split_packed(
            &packed,
            StereoPacking::SideBySide,
            EyeOrder::LeftFirst,
            false,
        );
        assert_eq!(
            pair.left.plane(0)[0],
            0.1,
            "left eye keeps its first column"
        );
        assert_eq!(
            pair.right.plane(0)[pair.right.width() - 1],
            0.9,
            "right eye keeps its last column"
        );
    }
}
