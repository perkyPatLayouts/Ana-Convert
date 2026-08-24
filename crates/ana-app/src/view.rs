// Ana-Convert — anaglyph 3D to full-colour stereo recovery
// SPDX-License-Identifier: GPL-3.0-or-later

//! What the preview pane is showing.
//!
//! Five ways of looking at one recovered pair, each answering a different
//! question: is this eye right, do they line up, does it still read through the
//! glasses, and where exactly is the disparity.

use ana_core::compose::{stack_horizontal, EyeOrder};
use ana_core::extract::{encode_anaglyph, AnaglyphFormat};
use ana_core::pipeline::StereoPair;
use ana_core::FrameF32;

/// Which view the preview pane is showing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ViewMode {
    /// The recovered left eye alone.
    #[default]
    Left,
    /// The recovered right eye alone.
    Right,
    /// Both eyes, as the output will be packed.
    SideBySide,
    /// The pair muxed back into an anaglyph — the check you can make with the
    /// glasses still on your desk.
    Anaglyph,
    /// Absolute difference between the eyes, which is where the depth is.
    Difference,
}

impl ViewMode {
    /// Every mode, in the order they should appear in a toolbar.
    pub const ALL: [ViewMode; 5] = [
        Self::Left,
        Self::Right,
        Self::SideBySide,
        Self::Anaglyph,
        Self::Difference,
    ];

    /// Short label for a button.
    pub fn label(self) -> &'static str {
        match self {
            Self::Left => "Left",
            Self::Right => "Right",
            Self::SideBySide => "Side by side",
            Self::Anaglyph => "Anaglyph",
            Self::Difference => "Difference",
        }
    }

    /// The shape this view should be drawn at, given one eye's shape.
    ///
    /// The preview must honour pixel shape as much as the render does — tuning
    /// against a stretched picture is tuning against the wrong picture.
    pub fn display_aspect(self, eye_aspect: f64) -> f64 {
        match self {
            Self::SideBySide => eye_aspect * 2.0,
            Self::Left | Self::Right | Self::Anaglyph | Self::Difference => eye_aspect,
        }
    }

    /// One line explaining what the view is for.
    pub fn hint(self) -> &'static str {
        match self {
            Self::Left => "The recovered left eye on its own",
            Self::Right => "The recovered right eye on its own",
            Self::SideBySide => "Both eyes, packed as the output will be",
            Self::Anaglyph => "Re-encoded to anaglyph, to check through the glasses",
            Self::Difference => "Where the two eyes disagree — this is the depth",
        }
    }
}

/// Builds the image for a view.
pub fn compose_view(
    pair: &StereoPair,
    mode: ViewMode,
    format: AnaglyphFormat,
    order: EyeOrder,
) -> FrameF32 {
    match mode {
        ViewMode::Left => pair.left.clone(),
        ViewMode::Right => pair.right.clone(),
        ViewMode::SideBySide => match order {
            EyeOrder::LeftFirst => stack_horizontal(&pair.left, &pair.right),
            EyeOrder::RightFirst => stack_horizontal(&pair.right, &pair.left),
        },
        ViewMode::Anaglyph => encode_anaglyph(&pair.left, &pair.right, format),
        ViewMode::Difference => difference(&pair.left, &pair.right),
    }
}

/// How much a difference is amplified before display.
///
/// Disparity between two views of the same scene is small over most of a
/// frame; shown one-to-one it reads as black and tells you nothing.
const DIFFERENCE_GAIN: f32 = 4.0;

/// Absolute per-channel difference, amplified so small disparities are visible.
fn difference(left: &FrameF32, right: &FrameF32) -> FrameF32 {
    assert!(
        left.same_size(right),
        "the two eyes must be the same size to compare"
    );
    let data: Vec<f32> = left
        .as_slice()
        .iter()
        .zip(right.as_slice())
        .map(|(a, b)| ((a - b).abs() * DIFFERENCE_GAIN).min(1.0))
        .collect();
    FrameF32::from_planar(left.width(), left.height(), left.channels(), data)
}

#[cfg(test)]
mod tests {
    use super::*;
    use ana_core::transfer::luminance;

    fn flat(w: usize, h: usize, rgb: [f32; 3]) -> FrameF32 {
        FrameF32::from_rgb_planes(
            w,
            h,
            &vec![rgb[0]; w * h],
            &vec![rgb[1]; w * h],
            &vec![rgb[2]; w * h],
        )
    }

    fn pair() -> StereoPair {
        StereoPair {
            left: flat(4, 3, [0.8, 0.2, 0.2]),
            right: flat(4, 3, [0.2, 0.6, 0.9]),
        }
    }

    fn view(mode: ViewMode) -> FrameF32 {
        compose_view(&pair(), mode, AnaglyphFormat::RedCyan, EyeOrder::LeftFirst)
    }

    fn first_pixel(f: &FrameF32) -> [f32; 3] {
        let (r, g, b) = f.rgb_planes();
        [r[0], g[0], b[0]]
    }

    #[test]
    fn the_single_eye_views_show_that_eye_untouched() {
        assert_eq!(first_pixel(&view(ViewMode::Left)), [0.8, 0.2, 0.2]);
        assert_eq!(first_pixel(&view(ViewMode::Right)), [0.2, 0.6, 0.9]);
    }

    #[test]
    fn single_eye_views_keep_the_source_size() {
        let v = view(ViewMode::Left);
        assert_eq!((v.width(), v.height()), (4, 3));
    }

    #[test]
    fn side_by_side_is_twice_as_wide_with_the_left_eye_first() {
        let v = view(ViewMode::SideBySide);
        assert_eq!((v.width(), v.height()), (8, 3));
        assert_eq!(v.plane(0)[0], 0.8, "left eye on the left");
        assert_eq!(v.plane(0)[4], 0.2, "right eye on the right");
    }

    #[test]
    fn side_by_side_honours_the_eye_order() {
        let v = compose_view(
            &pair(),
            ViewMode::SideBySide,
            AnaglyphFormat::RedCyan,
            EyeOrder::RightFirst,
        );
        assert_eq!(v.plane(0)[0], 0.2, "right eye now leads");
    }

    #[test]
    fn the_anaglyph_view_muxes_the_pair_back_together() {
        // Red from the left eye, cyan from the right — what the glasses expect.
        let v = view(ViewMode::Anaglyph);
        assert_eq!((v.width(), v.height()), (4, 3));
        assert_eq!(first_pixel(&v), [0.8, 0.6, 0.9]);
    }

    #[test]
    fn the_anaglyph_view_follows_the_chosen_format() {
        let v = compose_view(
            &pair(),
            ViewMode::Anaglyph,
            AnaglyphFormat::GreenMagenta,
            EyeOrder::LeftFirst,
        );
        assert_eq!(
            first_pixel(&v),
            [0.2, 0.2, 0.9],
            "green from left, magenta from right"
        );
    }

    #[test]
    fn identical_eyes_produce_a_black_difference() {
        let same = StereoPair {
            left: flat(2, 2, [0.5, 0.5, 0.5]),
            right: flat(2, 2, [0.5, 0.5, 0.5]),
        };
        let v = compose_view(
            &same,
            ViewMode::Difference,
            AnaglyphFormat::RedCyan,
            EyeOrder::LeftFirst,
        );
        assert!(
            v.as_slice().iter().all(|&s| s.abs() < 1e-6),
            "no disparity should read as black"
        );
    }

    #[test]
    fn differing_eyes_light_the_difference_up() {
        let v = view(ViewMode::Difference);
        assert!(
            luminance(v.plane(0)[0], v.plane(1)[0], v.plane(2)[0]) > 0.1,
            "a real difference must be visible, got {:?}",
            first_pixel(&v)
        );
    }

    #[test]
    fn the_difference_is_symmetric_and_never_negative() {
        // Which eye is subtracted from which must not change the picture, and
        // a negative sample would be clipped to black and read as agreement.
        let swapped = StereoPair {
            left: pair().right,
            right: pair().left,
        };
        let a = view(ViewMode::Difference);
        let b = compose_view(
            &swapped,
            ViewMode::Difference,
            AnaglyphFormat::RedCyan,
            EyeOrder::LeftFirst,
        );
        for (x, y) in a.as_slice().iter().zip(b.as_slice()) {
            assert!((x - y).abs() < 1e-6, "asymmetric difference: {x} vs {y}");
            assert!(*x >= 0.0, "negative difference sample: {x}");
        }
    }

    #[test]
    fn a_larger_difference_reads_brighter() {
        let small = StereoPair {
            left: flat(2, 2, [0.5, 0.5, 0.5]),
            right: flat(2, 2, [0.55, 0.5, 0.5]),
        };
        let big = StereoPair {
            left: flat(2, 2, [0.5, 0.5, 0.5]),
            right: flat(2, 2, [0.9, 0.5, 0.5]),
        };
        let f = |p: &StereoPair| {
            compose_view(
                p,
                ViewMode::Difference,
                AnaglyphFormat::RedCyan,
                EyeOrder::LeftFirst,
            )
            .plane(0)[0]
        };
        assert!(
            f(&big) > f(&small),
            "{} should exceed {}",
            f(&big),
            f(&small)
        );
    }

    #[test]
    fn side_by_side_is_drawn_twice_as_wide_as_one_eye() {
        assert!((ViewMode::SideBySide.display_aspect(2.28) - 4.56).abs() < 1e-9);
    }

    #[test]
    fn every_other_view_is_drawn_at_one_eyes_shape() {
        for mode in [
            ViewMode::Left,
            ViewMode::Right,
            ViewMode::Anaglyph,
            ViewMode::Difference,
        ] {
            assert!((mode.display_aspect(2.28) - 2.28).abs() < 1e-9, "{mode:?}");
        }
    }

    #[test]
    fn every_mode_has_a_label_and_a_hint() {
        for mode in ViewMode::ALL {
            assert!(!mode.label().is_empty(), "{mode:?} needs a label");
            assert!(!mode.hint().is_empty(), "{mode:?} needs a hint");
        }
    }
}
