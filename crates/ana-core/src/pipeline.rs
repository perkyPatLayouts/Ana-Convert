// Ana-Convert — anaglyph 3D to full-colour stereo recovery
// SPDX-License-Identifier: GPL-3.0-or-later

//! The whole per-frame conversion, wired together.

use crate::blur::gaussian_blur;
use crate::compose::{stack_horizontal, stack_vertical, EyeOrder, OutputLayout};
use crate::extract::encode_anaglyph;
use crate::extract::{extract_eyes, projections};
use crate::frame::FrameF32;
use crate::grade::apply_grade;
use crate::leak::correct_crosstalk;
use crate::packed::split_packed;
use crate::params::{ConvertParams, InputMode, MonoEye};
use crate::restore::restore_colour;
use crate::transfer::{from_linear_frame, to_linear_frame};

/// The two recovered views, gamma-encoded and ready for output.
#[derive(Debug, Clone)]
pub struct StereoPair {
    pub left: FrameF32,
    pub right: FrameF32,
}

/// The decoded frames feeding one conversion step.
#[derive(Debug, Clone, Copy)]
pub struct Sources<'a> {
    /// The frame being converted. An anaglyph, a packed pair, or the left eye
    /// of a two-file pair, depending on the input mode — hence the neutral
    /// name, since it stopped being only an anaglyph some time ago.
    pub primary: &'a FrameF32,
    /// The right eye, when it comes from a second file.
    pub right_eye: Option<&'a FrameF32>,
    /// Where colour is sampled from. Falls back to the anaglyph itself, which
    /// works but leaves the anaglyph's own colour cast in the result.
    pub colour: Option<&'a FrameF32>,
    /// A 2D release frame standing in for one eye, already in sync.
    pub mono: Option<&'a FrameF32>,
}

impl<'a> Sources<'a> {
    /// The common case: recover everything from one frame.
    pub fn from_anaglyph(primary: &'a FrameF32) -> Self {
        Self {
            primary,
            right_eye: None,
            colour: None,
            mono: None,
        }
    }
}

/// Recovers both eyes from one anaglyph frame.
pub fn process_frame(sources: Sources<'_>, params: &ConvertParams) -> StereoPair {
    // A packed source needs no recovery at all — the two eyes are already
    // there, just sharing a frame. Everything below is for anaglyph input.
    // Two files: the pair is already in hand, one eye from each.
    if params.input == InputMode::TwoFiles {
        let mut pair = StereoPair {
            left: sources.primary.clone(),
            // Falling back to the same frame keeps the preview usable before
            // the second file has been chosen, rather than refusing to draw.
            right: sources.right_eye.unwrap_or(sources.primary).clone(),
        };
        finish_pair(&mut pair, sources, params);
        return pair;
    }

    if let InputMode::Packed {
        packing,
        order,
        anamorphic,
    } = params.input
    {
        let mut pair = split_packed(sources.primary, packing, order, anamorphic);
        finish_pair(&mut pair, sources, params);
        return pair;
    }

    let linear = params.work_in_linear_light;
    let tf = params.transfer;

    // 1. Bring the anaglyph and the colour reference into the working space.
    let mut anaglyph = sources.primary.clone();
    let mut colour = sources.colour.unwrap_or(sources.primary).clone();
    if linear {
        to_linear_frame(&mut anaglyph, tf);
        to_linear_frame(&mut colour, tf);
    }

    // 2. Pull each eye's surviving signal out of the colour channels. The same
    //    projections drive restoration in step 6 — they must not diverge.
    let (left_projection, right_projection) = projections(params.input_format);
    let (mut left, mut right) = extract_eyes(&anaglyph, params.input_format);

    // 3. Remove each eye's ghost from the other — in gamma space, even when the
    //    rest of the pipeline is linear.
    //
    //    Ghosting in a release is almost always baked in during mastering, as
    //    arithmetic on gamma-encoded channels. A gamma-domain mix is not linear,
    //    so subtracting it in linear light cannot invert it: on the synthetic
    //    leaky master in tests/ground_truth.rs, correcting in the wrong space
    //    makes the result 4 dB *worse* than leaving the leak alone, and
    //    correcting in gamma recovers essentially all of it. This also keeps the
    //    original script's leak percentages meaning what they always meant.
    if linear {
        from_linear_frame(&mut left, tf);
        from_linear_frame(&mut right, tf);
    }
    correct_crosstalk(
        &mut left,
        &mut right,
        params.leak_left_fraction(),
        params.leak_right_fraction(),
    );
    if linear {
        to_linear_frame(&mut left, tf);
        to_linear_frame(&mut right, tf);
    }

    // 4. Soften the peaking fringes that separation exposes.
    let defringe = |eye: FrameF32, sigma: f32| {
        if sigma > 0.0 {
            gaussian_blur(&eye, sigma, 0.0)
        } else {
            eye
        }
    };
    let left = defringe(left, params.defringe_sigma_left());
    let right = defringe(right, params.defringe_sigma_right());

    // 5. Smear the reference colour over the disparity it has to cover.
    let colour = gaussian_blur(&colour, params.colour_sigma_x(), params.colour_sigma_y());

    // 6. Recombine brightness with colour.
    let mut left = restore_colour(&left, &colour, left_projection, params.restore);
    let mut right = restore_colour(&right, &colour, right_projection, params.restore);

    // 7. Back to gamma-encoded, where grading is defined.
    if linear {
        from_linear_frame(&mut left, tf);
        from_linear_frame(&mut right, tf);
    }
    apply_grade(&mut left, &params.grade_left);
    apply_grade(&mut right, &params.grade_right);

    // 8. A 2D release, if supplied, overrides its eye entirely — including the
    //    grade, which exists to bring the *recovered* eye into line with it.
    if let Some(mono) = sources.mono {
        match params.mono_eye {
            MonoEye::Left => left = mono.clone(),
            MonoEye::Right => right = mono.clone(),
            MonoEye::None => {}
        }
    }

    if params.swap_eyes {
        std::mem::swap(&mut left, &mut right);
    }
    StereoPair { left, right }
}

/// Grading, 2D substitution and eye swapping — the steps every input mode ends
/// with, kept in one place so they cannot drift apart between modes.
fn finish_pair(pair: &mut StereoPair, sources: Sources<'_>, params: &ConvertParams) {
    apply_grade(&mut pair.left, &params.grade_left);
    apply_grade(&mut pair.right, &params.grade_right);
    if let Some(mono) = sources.mono {
        match params.mono_eye {
            MonoEye::Left => pair.left = mono.clone(),
            MonoEye::Right => pair.right = mono.clone(),
            MonoEye::None => {}
        }
    }
    if params.swap_eyes {
        std::mem::swap(&mut pair.left, &mut pair.right);
    }
}

/// Packs a recovered pair into the deliverable frames.
///
/// Returns one frame for a stacked layout, or two — always left then right —
/// for separate streams, where the file names carry the ordering instead.
pub fn compose_output(pair: &StereoPair, params: &ConvertParams) -> Vec<FrameF32> {
    let (first, second) = match params.eye_order {
        EyeOrder::LeftFirst => (&pair.left, &pair.right),
        EyeOrder::RightFirst => (&pair.right, &pair.left),
    };

    let frames = match params.layout {
        OutputLayout::SideBySide => vec![stack_horizontal(first, second)],
        OutputLayout::TopBottom => vec![stack_vertical(first, second)],
        // Ordering is carried by the output file names, so the eyes stay in
        // their natural order here.
        OutputLayout::Separate => vec![pair.left.clone(), pair.right.clone()],
        OutputLayout::Anaglyph => vec![encode_anaglyph(
            &pair.left,
            &pair.right,
            params.output_format,
        )],
        OutputLayout::LeftOnly => vec![pair.left.clone()],
        OutputLayout::RightOnly => vec![pair.right.clone()],
    };

    match params.output_size {
        Some((w, h)) => frames
            .iter()
            .map(|f| crate::compose::resize(f, w, h))
            .collect(),
        None => frames,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::compose::OutputLayout;
    use crate::extract::AnaglyphFormat;
    use crate::grade::Grade;
    use crate::transfer::luminance;

    fn flat(w: usize, h: usize, rgb: [f32; 3]) -> FrameF32 {
        FrameF32::from_rgb_planes(
            w,
            h,
            &vec![rgb[0]; w * h],
            &vec![rgb[1]; w * h],
            &vec![rgb[2]; w * h],
        )
    }

    fn first_pixel(frame: &FrameF32) -> [f32; 3] {
        let (r, g, b) = frame.rgb_planes();
        [r[0], g[0], b[0]]
    }

    /// Defaults, but with colour blur disabled so flat-field expectations are
    /// exact rather than approximate.
    fn unblurred() -> ConvertParams {
        ConvertParams {
            decimate_horiz: 100.0,
            decimate_vert: 100.0,
            ..Default::default()
        }
    }

    #[test]
    fn red_cyan_channels_reach_the_correct_eyes_through_the_whole_chain() {
        // A neutral colour reference means restoration cannot tint anything,
        // so each eye must come back at exactly the brightness its anaglyph
        // channel carried — 0.8 for the red left eye, 0.3 for the green right.
        let anaglyph = flat(4, 4, [0.8, 0.3, 0.3]);
        let colour = flat(4, 4, [0.5, 0.5, 0.5]);
        let pair = process_frame(
            Sources {
                primary: &anaglyph,
                right_eye: None,
                colour: Some(&colour),
                mono: None,
            },
            &unblurred(),
        );

        let left = first_pixel(&pair.left);
        let right = first_pixel(&pair.right);
        for c in 0..3 {
            assert!((left[c] - 0.8).abs() < 1e-3, "left channel {c}: {left:?}");
            assert!(
                (right[c] - 0.3).abs() < 1e-3,
                "right channel {c}: {right:?}"
            );
        }
    }

    #[test]
    fn green_magenta_channels_reach_the_correct_eyes() {
        let anaglyph = flat(4, 4, [0.3, 0.8, 0.3]);
        let colour = flat(4, 4, [0.5, 0.5, 0.5]);
        let pair = process_frame(
            Sources {
                primary: &anaglyph,
                right_eye: None,
                colour: Some(&colour),
                mono: None,
            },
            &ConvertParams {
                input_format: AnaglyphFormat::GreenMagenta,
                ..unblurred()
            },
        );
        assert!(
            (first_pixel(&pair.left)[0] - 0.8).abs() < 1e-3,
            "green must feed the left eye, got {:?}",
            first_pixel(&pair.left)
        );
        assert!(
            (first_pixel(&pair.right)[0] - 0.3).abs() < 1e-3,
            "the magenta pair must feed the right eye, got {:?}",
            first_pixel(&pair.right)
        );
    }

    #[test]
    fn the_anaglyph_supplies_colour_when_no_reference_is_given() {
        let anaglyph = flat(4, 4, [0.8, 0.3, 0.3]);
        let pair = process_frame(Sources::from_anaglyph(&anaglyph), &unblurred());
        let left = first_pixel(&pair.left);
        assert!(
            left[0] > left[1],
            "using the anaglyph for colour leaves its red cast, got {left:?}"
        );
    }

    #[test]
    fn swapping_exchanges_the_two_eyes() {
        let anaglyph = flat(4, 4, [0.8, 0.3, 0.3]);
        let colour = flat(4, 4, [0.5, 0.5, 0.5]);
        let sources = Sources {
            primary: &anaglyph,
            right_eye: None,
            colour: Some(&colour),
            mono: None,
        };
        let normal = process_frame(sources, &unblurred());
        let swapped = process_frame(
            sources,
            &ConvertParams {
                swap_eyes: true,
                ..unblurred()
            },
        );
        assert_eq!(first_pixel(&swapped.left), first_pixel(&normal.right));
        assert_eq!(first_pixel(&swapped.right), first_pixel(&normal.left));
    }

    #[test]
    fn a_mono_source_replaces_exactly_one_eye() {
        let anaglyph = flat(4, 4, [0.8, 0.3, 0.3]);
        let colour = flat(4, 4, [0.5, 0.5, 0.5]);
        let mono = flat(4, 4, [0.11, 0.22, 0.33]);
        let pair = process_frame(
            Sources {
                primary: &anaglyph,
                right_eye: None,
                colour: Some(&colour),
                mono: Some(&mono),
            },
            &ConvertParams {
                mono_eye: MonoEye::Left,
                ..unblurred()
            },
        );
        assert_eq!(
            first_pixel(&pair.left),
            [0.11, 0.22, 0.33],
            "left is the 2D copy"
        );
        assert!(
            (first_pixel(&pair.right)[0] - 0.3).abs() < 1e-3,
            "right is still recovered, got {:?}",
            first_pixel(&pair.right)
        );
    }

    #[test]
    fn a_mono_source_bypasses_grading() {
        // Grading exists to make the recovered eye match the 2D one, so the
        // 2D one must arrive untouched — as it did in the original script.
        let anaglyph = flat(4, 4, [0.8, 0.3, 0.3]);
        let mono = flat(4, 4, [0.5, 0.5, 0.5]);
        let pair = process_frame(
            Sources {
                primary: &anaglyph,
                right_eye: None,
                colour: None,
                mono: Some(&mono),
            },
            &ConvertParams {
                mono_eye: MonoEye::Right,
                grade_right: Grade {
                    brightness: 0.3,
                    ..Default::default()
                },
                ..unblurred()
            },
        );
        assert_eq!(first_pixel(&pair.right), [0.5, 0.5, 0.5]);
    }

    #[test]
    fn grading_reaches_the_recovered_eyes() {
        let anaglyph = flat(4, 4, [0.5, 0.5, 0.5]);
        let colour = flat(4, 4, [0.5, 0.5, 0.5]);
        let sources = Sources {
            primary: &anaglyph,
            right_eye: None,
            colour: Some(&colour),
            mono: None,
        };
        let plain = process_frame(sources, &unblurred());
        let brightened = process_frame(
            sources,
            &ConvertParams {
                grade_left: Grade {
                    brightness: 0.2,
                    ..Default::default()
                },
                ..unblurred()
            },
        );
        assert!(
            (first_pixel(&brightened.left)[0] - first_pixel(&plain.left)[0] - 0.2).abs() < 1e-4,
            "left should be 0.2 brighter"
        );
        assert_eq!(
            first_pixel(&brightened.right),
            first_pixel(&plain.right),
            "the right eye has its own grade and must not move"
        );
    }

    #[test]
    fn cross_talk_correction_reaches_the_pipeline() {
        let anaglyph = flat(4, 4, [0.8, 0.3, 0.3]);
        let colour = flat(4, 4, [0.5, 0.5, 0.5]);
        let sources = Sources {
            primary: &anaglyph,
            right_eye: None,
            colour: Some(&colour),
            mono: None,
        };
        let plain = process_frame(sources, &unblurred());
        let corrected = process_frame(
            sources,
            &ConvertParams {
                leak_correct_left: 25.0,
                ..unblurred()
            },
        );
        assert!(
            first_pixel(&corrected.left)[0] != first_pixel(&plain.left)[0],
            "a 25% correction must change the left eye"
        );
    }

    #[test]
    fn recovered_eyes_keep_the_source_geometry() {
        let anaglyph = FrameF32::new_rgb(9, 5);
        let pair = process_frame(Sources::from_anaglyph(&anaglyph), &ConvertParams::default());
        for (name, f) in [("left", &pair.left), ("right", &pair.right)] {
            assert_eq!((f.width(), f.height(), f.channels()), (9, 5, 3), "{name}");
        }
    }

    #[test]
    fn linear_light_and_gamma_space_give_different_results() {
        // If the toggle did nothing, the whole linear-light argument would be
        // decoration. The frame needs real contrast for the colour blur to
        // average across: on a flat field restoration is exact either way, so
        // the two spaces would legitimately agree.
        let split: Vec<f32> = (0..16)
            .map(|i| if i % 4 < 2 { 0.9 } else { 0.05 })
            .collect();
        let dim: Vec<f32> = split.iter().map(|v| v * 0.35).collect();
        let anaglyph = FrameF32::from_rgb_planes(4, 4, &split, &dim, &dim);
        let sources = Sources::from_anaglyph(&anaglyph);
        let linear = process_frame(
            sources,
            &ConvertParams {
                work_in_linear_light: true,
                ..ConvertParams::default()
            },
        );
        let gamma = process_frame(
            sources,
            &ConvertParams {
                work_in_linear_light: false,
                ..ConvertParams::default()
            },
        );
        // Compare green, not red: for a red/cyan left eye the red channel is
        // pinned to the extracted signal by the projection in either space, so
        // it agrees by construction. The colour blur's effect shows in the
        // channels restoration is free to move.
        assert!(
            (first_pixel(&linear.left)[1] - first_pixel(&gamma.left)[1]).abs() > 1e-3,
            "linear {:?} vs gamma {:?}",
            first_pixel(&linear.left),
            first_pixel(&gamma.left)
        );
    }

    // --- composition ---

    fn pair_of(left: [f32; 3], right: [f32; 3]) -> StereoPair {
        StereoPair {
            left: flat(2, 2, left),
            right: flat(2, 2, right),
        }
    }

    #[test]
    fn side_by_side_doubles_the_width_with_the_left_eye_first() {
        let pair = pair_of([1.0, 0.0, 0.0], [0.0, 0.0, 1.0]);
        let out = compose_output(&pair, &ConvertParams::default());
        assert_eq!(out.len(), 1);
        assert_eq!((out[0].width(), out[0].height()), (4, 2));
        assert_eq!(out[0].plane(0)[0], 1.0, "left eye occupies the left half");
        assert_eq!(out[0].plane(0)[2], 0.0, "right eye occupies the right half");
    }

    #[test]
    fn right_first_ordering_puts_the_right_eye_on_the_left() {
        let pair = pair_of([1.0, 0.0, 0.0], [0.0, 0.0, 1.0]);
        let out = compose_output(
            &pair,
            &ConvertParams {
                eye_order: EyeOrder::RightFirst,
                ..Default::default()
            },
        );
        assert_eq!(out[0].plane(0)[0], 0.0, "right eye now leads");
        assert_eq!(out[0].plane(0)[2], 1.0);
    }

    #[test]
    fn top_bottom_doubles_the_height() {
        let pair = pair_of([1.0, 0.0, 0.0], [0.0, 0.0, 1.0]);
        let out = compose_output(
            &pair,
            &ConvertParams {
                layout: OutputLayout::TopBottom,
                ..Default::default()
            },
        );
        assert_eq!(out.len(), 1);
        assert_eq!((out[0].width(), out[0].height()), (2, 4));
        assert_eq!(out[0].plane(0)[0], 1.0, "left eye on top");
    }

    #[test]
    fn separate_layout_returns_two_frames_at_source_size() {
        let pair = pair_of([1.0, 0.0, 0.0], [0.0, 0.0, 1.0]);
        let out = compose_output(
            &pair,
            &ConvertParams {
                layout: OutputLayout::Separate,
                ..Default::default()
            },
        );
        assert_eq!(out.len(), 2);
        assert_eq!((out[0].width(), out[0].height()), (2, 2));
        assert_eq!(out[0].plane(0)[0], 1.0, "first frame is the left eye");
        assert_eq!(out[1].plane(2)[0], 1.0, "second frame is the right eye");
    }

    #[test]
    fn separate_layout_ignores_eye_order_because_filenames_carry_it() {
        let pair = pair_of([1.0, 0.0, 0.0], [0.0, 0.0, 1.0]);
        let out = compose_output(
            &pair,
            &ConvertParams {
                layout: OutputLayout::Separate,
                eye_order: EyeOrder::RightFirst,
                ..Default::default()
            },
        );
        assert_eq!(out[0].plane(0)[0], 1.0, "left eye stays first");
    }

    #[test]
    fn output_size_resizes_the_composed_result() {
        let pair = pair_of([0.5, 0.5, 0.5], [0.5, 0.5, 0.5]);
        let out = compose_output(
            &pair,
            &ConvertParams {
                output_size: Some((16, 9)),
                ..Default::default()
            },
        );
        assert_eq!((out[0].width(), out[0].height()), (16, 9));
    }

    #[test]
    fn output_size_applies_to_each_separate_stream() {
        let pair = pair_of([0.5, 0.5, 0.5], [0.5, 0.5, 0.5]);
        let out = compose_output(
            &pair,
            &ConvertParams {
                layout: OutputLayout::Separate,
                output_size: Some((16, 9)),
                ..Default::default()
            },
        );
        assert_eq!(out.len(), 2);
        for f in &out {
            assert_eq!((f.width(), f.height()), (16, 9));
        }
    }

    // --- packed stereo input, and the reverse trip ---

    fn packed_source() -> FrameF32 {
        // A 8x4 frame: left half bright red, right half dark blue.
        crate::compose::stack_horizontal(&flat(4, 4, [0.9, 0.1, 0.1]), &flat(4, 4, [0.1, 0.1, 0.9]))
    }

    fn packed_params(anamorphic: bool) -> ConvertParams {
        ConvertParams {
            input: crate::params::InputMode::packed(
                crate::packed::StereoPacking::SideBySide,
                anamorphic,
            ),
            ..Default::default()
        }
    }

    #[test]
    fn a_packed_source_is_split_rather_than_recovered() {
        // No anaglyph maths should touch it: the eyes come out exactly as
        // stored, not colour-restored versions of themselves.
        let pair = process_frame(
            Sources::from_anaglyph(&packed_source()),
            &packed_params(false),
        );
        assert_eq!(first_pixel(&pair.left), [0.9, 0.1, 0.1]);
        assert_eq!(first_pixel(&pair.right), [0.1, 0.1, 0.9]);
        assert_eq!((pair.left.width(), pair.left.height()), (4, 4));
    }

    #[test]
    fn an_anamorphic_packed_source_comes_back_at_full_width() {
        let pair = process_frame(
            Sources::from_anaglyph(&packed_source()),
            &packed_params(true),
        );
        assert_eq!(
            (pair.left.width(), pair.left.height()),
            (8, 4),
            "each squeezed eye must be stretched back"
        );
    }

    #[test]
    fn a_packed_source_still_honours_grading_and_swapping() {
        let params = ConvertParams {
            swap_eyes: true,
            grade_left: Grade {
                brightness: 0.1,
                ..Default::default()
            },
            ..packed_params(false)
        };
        let pair = process_frame(Sources::from_anaglyph(&packed_source()), &params);
        // After the swap, the stored-first eye is on the right — and it was the
        // one graded, because grading happens before the swap.
        assert!(
            (first_pixel(&pair.right)[0] - 1.0).abs() < 1e-4,
            "0.9 plus 0.1 brightness, got {:?}",
            first_pixel(&pair.right)
        );
    }

    // --- two files, one per eye ---

    fn two_file_params() -> ConvertParams {
        ConvertParams {
            input: crate::params::InputMode::TwoFiles,
            ..Default::default()
        }
    }

    #[test]
    fn two_files_are_taken_as_the_two_eyes_untouched() {
        // Nothing to recover and nothing to split: each file is already a whole
        // eye, so both must arrive exactly as they were.
        let left = flat(4, 4, [0.9, 0.1, 0.1]);
        let right = flat(4, 4, [0.1, 0.1, 0.9]);
        let pair = process_frame(
            Sources {
                primary: &left,
                right_eye: Some(&right),
                colour: None,
                mono: None,
            },
            &two_file_params(),
        );
        assert_eq!(first_pixel(&pair.left), [0.9, 0.1, 0.1]);
        assert_eq!(first_pixel(&pair.right), [0.1, 0.1, 0.9]);
    }

    #[test]
    fn a_missing_second_file_shows_the_first_in_both_eyes() {
        // So the preview stays usable while the second file is still being
        // chosen, rather than refusing to draw anything.
        let left = flat(4, 4, [0.9, 0.1, 0.1]);
        let pair = process_frame(Sources::from_anaglyph(&left), &two_file_params());
        assert_eq!(first_pixel(&pair.left), first_pixel(&pair.right));
    }

    #[test]
    fn two_files_still_honour_grading_and_swapping() {
        let left = flat(4, 4, [0.9, 0.1, 0.1]);
        let right = flat(4, 4, [0.1, 0.1, 0.9]);
        let params = ConvertParams {
            swap_eyes: true,
            ..two_file_params()
        };
        let pair = process_frame(
            Sources {
                primary: &left,
                right_eye: Some(&right),
                colour: None,
                mono: None,
            },
            &params,
        );
        assert_eq!(first_pixel(&pair.left), [0.1, 0.1, 0.9], "swapped");
    }

    #[test]
    fn two_files_reach_every_destination() {
        let left = flat(8, 4, [0.9, 0.1, 0.1]);
        let right = flat(8, 4, [0.1, 0.1, 0.9]);
        for layout in OutputLayout::ALL {
            let params = ConvertParams {
                layout,
                ..two_file_params()
            };
            let pair = process_frame(
                Sources {
                    primary: &left,
                    right_eye: Some(&right),
                    colour: None,
                    mono: None,
                },
                &params,
            );
            let out = compose_output(&pair, &params);
            assert_eq!(out.len(), layout.file_count(), "{layout:?}");
            assert_eq!(
                params.output_geometry((8, 4)),
                (out[0].width(), out[0].height()),
                "{layout:?} geometry"
            );
        }
    }

    #[test]
    fn two_files_can_be_muxed_into_an_anaglyph() {
        // The useful case: two per-eye files from a modern release, turned back
        // into something the old glasses can watch.
        let left = flat(4, 4, [0.9, 0.1, 0.1]);
        let right = flat(4, 4, [0.1, 0.1, 0.9]);
        let params = ConvertParams {
            layout: OutputLayout::Anaglyph,
            output_format: AnaglyphFormat::RedCyan,
            ..two_file_params()
        };
        let pair = process_frame(
            Sources {
                primary: &left,
                right_eye: Some(&right),
                colour: None,
                mono: None,
            },
            &params,
        );
        let out = compose_output(&pair, &params);
        assert_eq!(
            first_pixel(&out[0]),
            [0.9, 0.1, 0.9],
            "red from the left file, green and blue from the right"
        );
    }

    #[test]
    fn a_single_eye_can_be_pulled_from_two_files() {
        let left = flat(4, 4, [0.9, 0.1, 0.1]);
        let right = flat(4, 4, [0.1, 0.1, 0.9]);
        for (layout, want) in [
            (OutputLayout::LeftOnly, [0.9, 0.1, 0.1]),
            (OutputLayout::RightOnly, [0.1, 0.1, 0.9]),
        ] {
            let params = ConvertParams {
                layout,
                ..two_file_params()
            };
            let pair = process_frame(
                Sources {
                    primary: &left,
                    right_eye: Some(&right),
                    colour: None,
                    mono: None,
                },
                &params,
            );
            assert_eq!(
                first_pixel(&compose_output(&pair, &params)[0]),
                want,
                "{layout:?}"
            );
        }
    }

    #[test]
    fn every_layout_survives_an_oddly_sized_packed_source() {
        // The crash as reported: load a side-by-side file, choose anaglyph
        // output, and the app goes down. Preview frames are scaled to whatever
        // a pane offers, so a 601-wide frame is entirely ordinary, and an
        // uneven split left the two eyes a column apart.
        for (w, h) in [(601usize, 271usize), (1279, 577), (9, 5)] {
            for packing in [
                crate::packed::StereoPacking::SideBySide,
                crate::packed::StereoPacking::TopBottom,
            ] {
                for layout in OutputLayout::ALL {
                    let params = ConvertParams {
                        input: crate::params::InputMode::packed(packing, false),
                        layout,
                        ..Default::default()
                    };
                    let source = FrameF32::new_rgb(w, h);
                    let pair = process_frame(Sources::from_anaglyph(&source), &params);
                    let out = compose_output(&pair, &params);
                    assert_eq!(
                        out.len(),
                        layout.file_count(),
                        "{w}x{h} {packing:?} {layout:?}"
                    );
                    assert!(
                        out[0].width() > 0 && out[0].height() > 0,
                        "{w}x{h} {packing:?} {layout:?} produced nothing"
                    );
                }
            }
        }
    }

    #[test]
    fn the_written_anaglyph_uses_the_output_format_not_the_sources() {
        // Recovering a red/cyan transfer and writing green/magenta back out is
        // a reasonable thing to want, so the two settings are independent.
        // A packed source, because its two eyes are genuinely different. A
        // flat anaglyph recovers into two near-identical eyes, and then every
        // muxing arrangement of them coincides and the test proves nothing.
        let params = ConvertParams {
            output_format: AnaglyphFormat::GreenMagenta,
            layout: OutputLayout::Anaglyph,
            ..packed_params(false)
        };
        let pair = process_frame(Sources::from_anaglyph(&packed_source()), &params);
        let out = compose_output(&pair, &params);

        // Green/magenta puts the left eye in green and the right in red+blue,
        // so the result must differ from the red/cyan mux of the same pair.
        let as_red_cyan = ana_core_encode(&pair, AnaglyphFormat::RedCyan);
        assert_ne!(
            first_pixel(&out[0]),
            first_pixel(&as_red_cyan),
            "the output format was ignored"
        );
        let expected = ana_core_encode(&pair, AnaglyphFormat::GreenMagenta);
        assert_eq!(first_pixel(&out[0]), first_pixel(&expected));
    }

    fn ana_core_encode(pair: &StereoPair, format: AnaglyphFormat) -> FrameF32 {
        crate::extract::encode_anaglyph(&pair.left, &pair.right, format)
    }

    #[test]
    fn every_anaglyph_output_format_is_available() {
        let anaglyph = flat(4, 4, [0.8, 0.3, 0.3]);
        for format in [
            AnaglyphFormat::RedCyan,
            AnaglyphFormat::RedBlue,
            AnaglyphFormat::GreenMagenta,
        ] {
            let params = ConvertParams {
                output_format: format,
                layout: OutputLayout::Anaglyph,
                ..unblurred()
            };
            let pair = process_frame(Sources::from_anaglyph(&anaglyph), &params);
            let out = compose_output(&pair, &params);
            assert_eq!(out.len(), 1, "{format:?}");
            assert!(
                out[0].as_slice().iter().all(|s| s.is_finite()),
                "{format:?}"
            );
        }
    }

    #[test]
    fn a_packed_source_can_be_written_back_out_as_an_anaglyph() {
        // Feature and inverse in one: side-by-side in, red/cyan out.
        let params = ConvertParams {
            layout: OutputLayout::Anaglyph,
            ..packed_params(false)
        };
        let pair = process_frame(Sources::from_anaglyph(&packed_source()), &params);
        let out = compose_output(&pair, &params);
        assert_eq!(out.len(), 1);
        assert_eq!((out[0].width(), out[0].height()), (4, 4));
        assert_eq!(
            first_pixel(&out[0]),
            [0.9, 0.1, 0.9],
            "red from the left eye, green and blue from the right"
        );
    }

    #[test]
    fn a_single_eye_can_be_extracted_from_a_packed_source() {
        for (layout, want) in [
            (OutputLayout::LeftOnly, [0.9, 0.1, 0.1]),
            (OutputLayout::RightOnly, [0.1, 0.1, 0.9]),
        ] {
            let params = ConvertParams {
                layout,
                ..packed_params(false)
            };
            let pair = process_frame(Sources::from_anaglyph(&packed_source()), &params);
            let out = compose_output(&pair, &params);
            assert_eq!(out.len(), 1, "{layout:?} writes one file");
            assert_eq!(first_pixel(&out[0]), want, "{layout:?}");
            assert_eq!((out[0].width(), out[0].height()), (4, 4));
        }
    }

    #[test]
    fn a_recovered_anaglyph_can_be_written_straight_back_out_as_one() {
        // The round trip: recover, then re-mux. Not useful in itself, but it
        // proves the anaglyph output works from either kind of source.
        let anaglyph = flat(4, 4, [0.8, 0.3, 0.3]);
        let params = ConvertParams {
            layout: OutputLayout::Anaglyph,
            ..Default::default()
        };
        let pair = process_frame(Sources::from_anaglyph(&anaglyph), &params);
        let out = compose_output(&pair, &params);
        assert_eq!((out[0].width(), out[0].height()), (4, 4));
        assert!(out[0].as_slice().iter().all(|s| s.is_finite()));
    }

    #[test]
    fn output_geometry_agrees_with_what_is_actually_produced() {
        // The pipeline sizes encoders from this before decoding anything, so a
        // disagreement would shear every frame.
        let source = (8, 4);
        for input in [
            crate::params::InputMode::Anaglyph,
            crate::params::InputMode::packed(crate::packed::StereoPacking::SideBySide, false),
            crate::params::InputMode::packed(crate::packed::StereoPacking::SideBySide, true),
            crate::params::InputMode::packed(crate::packed::StereoPacking::TopBottom, false),
        ] {
            for layout in OutputLayout::ALL {
                let params = ConvertParams {
                    input,
                    layout,
                    ..Default::default()
                };
                let pair = process_frame(Sources::from_anaglyph(&packed_source()), &params);
                let out = compose_output(&pair, &params);
                assert_eq!(
                    params.output_geometry(source),
                    (out[0].width(), out[0].height()),
                    "{input:?} {layout:?}"
                );
                assert_eq!(out.len(), layout.file_count(), "{layout:?} file count");
            }
        }
    }

    #[test]
    fn a_full_default_run_produces_a_sane_side_by_side_frame() {
        let anaglyph = flat(32, 24, [0.7, 0.4, 0.4]);
        let pair = process_frame(Sources::from_anaglyph(&anaglyph), &ConvertParams::default());
        let out = compose_output(&pair, &ConvertParams::default());
        assert_eq!((out[0].width(), out[0].height()), (64, 24));
        assert!(
            out[0].as_slice().iter().all(|s| s.is_finite()),
            "no NaNs may reach the encoder"
        );
        assert!(
            luminance(out[0].plane(0)[0], out[0].plane(1)[0], out[0].plane(2)[0]) > 0.0,
            "the frame must not be black"
        );
    }
}
