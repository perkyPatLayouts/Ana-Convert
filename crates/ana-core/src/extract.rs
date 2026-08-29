// Ana-Convert — anaglyph 3D to full-colour stereo recovery
// SPDX-License-Identifier: GPL-3.0-or-later

//! Recovering a per-eye brightness signal from the anaglyph's colour channels.

use crate::frame::FrameF32;
use crate::transfer::{LUMA_B, LUMA_G, LUMA_R};
use rayon::prelude::*;

/// Which anaglyph encoding the source uses.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
#[derive(Default)]
pub enum AnaglyphFormat {
    /// Red left, cyan right. The most common release format.
    #[default]
    RedCyan,
    /// Green left, magenta right.
    GreenMagenta,
    /// Red left, blue right.
    RedBlue,
    /// ColorCode 3-D: amber left, dark blue right.
    ///
    /// The amber filter passes red *and* green, so the left eye keeps almost
    /// all the luminance and most of the colour. The right eye gets blue alone,
    /// which carries barely seven percent of white's luminance — so it is dim,
    /// and in most transfers the blue channel is the noisiest and most heavily
    /// compressed of the three. The depth survives; the right eye's picture
    /// does not survive well.
    ///
    /// Spelled the American way because it is a trade name, not a colour.
    ColorCode,
}

/// The linear combination of RGB that one eye's filter lets through.
///
/// This is the single most important type in the crate. It describes what the
/// anaglyph actually preserves about an eye, and both halves of the recovery
/// must agree on it: extraction reads this combination out of the anaglyph, and
/// restoration scales the colour reference until *its* value of the same
/// combination matches. Use different projections in the two places and every
/// saturated colour comes back at the wrong brightness.
///
/// Weights always sum to 1, so a neutral grey of value `v` projects to `v`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct EyeProjection {
    pub r: f32,
    pub g: f32,
    pub b: f32,
}

impl EyeProjection {
    /// The value this filter passes for one RGB triple.
    pub fn apply(&self, r: f32, g: f32, b: f32) -> f32 {
        self.r * r + self.g * g + self.b * b
    }

    /// Sum of the weights. Always 1 for a well-formed projection.
    pub fn weight_sum(&self) -> f32 {
        self.r + self.g + self.b
    }
}

/// Renormalises the red+blue pair so a neutral magenta of value `v` projects
/// to `v` rather than to the fraction of white's luminance it carries.
const MAGENTA_NORM: f32 = 1.0 / (LUMA_R + LUMA_B);

/// The same for amber, which is red plus green.
const AMBER_NORM: f32 = 1.0 / (LUMA_R + LUMA_G);

/// The `(left, right)` projections for an anaglyph encoding.
pub fn projections(format: AnaglyphFormat) -> (EyeProjection, EyeProjection) {
    const RED: EyeProjection = EyeProjection {
        r: 1.0,
        g: 0.0,
        b: 0.0,
    };
    const GREEN: EyeProjection = EyeProjection {
        r: 0.0,
        g: 1.0,
        b: 0.0,
    };
    const BLUE: EyeProjection = EyeProjection {
        r: 0.0,
        g: 0.0,
        b: 1.0,
    };
    // Cyan is green plus blue, but the original found blue too noisy to use
    // and green alone works better. Magenta has no such choice to make.
    const MAGENTA: EyeProjection = EyeProjection {
        r: LUMA_R * MAGENTA_NORM,
        g: 0.0,
        b: LUMA_B * MAGENTA_NORM,
    };

    // Amber passes red and green, which between them carry 93% of white's
    // luminance — which is the whole idea of ColorCode: one eye gets a nearly
    // complete picture and the other supplies little more than parallax.
    const AMBER: EyeProjection = EyeProjection {
        r: LUMA_R * AMBER_NORM,
        g: LUMA_G * AMBER_NORM,
        b: 0.0,
    };

    match format {
        AnaglyphFormat::RedCyan => (RED, GREEN),
        AnaglyphFormat::GreenMagenta => (GREEN, MAGENTA),
        AnaglyphFormat::RedBlue => (RED, BLUE),
        AnaglyphFormat::ColorCode => (AMBER, BLUE),
    }
}

impl AnaglyphFormat {
    /// Every encoding, in the order a menu should list them.
    pub const ALL: [AnaglyphFormat; 4] = [
        Self::RedCyan,
        Self::GreenMagenta,
        Self::RedBlue,
        Self::ColorCode,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Self::RedCyan => "Red / cyan",
            Self::GreenMagenta => "Green / magenta",
            Self::RedBlue => "Red / blue",
            Self::ColorCode => "ColorCode (amber / blue)",
        }
    }
}

/// Extracts each eye's surviving signal from an anaglyph frame.
///
/// Returns `(left, right)` as single-channel frames holding the value of each
/// eye's [`EyeProjection`]. Input must be linear-light RGB.
pub fn extract_eyes(anaglyph: &FrameF32, format: AnaglyphFormat) -> (FrameF32, FrameF32) {
    let (w, h) = (anaglyph.width(), anaglyph.height());
    let (r, g, b) = anaglyph.rgb_planes();
    let (left, right) = projections(format);

    let project = |p: EyeProjection| {
        let data: Vec<f32> = (0..w * h)
            .into_par_iter()
            .map(|i| p.apply(r[i], g[i], b[i]))
            .collect();
        FrameF32::from_planar(w, h, 1, data)
    };
    (project(left), project(right))
}

/// Muxes a full-colour stereo pair back into an anaglyph.
///
/// The inverse of [`extract_eyes`], and exactly what a mastering house did in
/// the first place: a straight channel copy. Two uses — building test material
/// whose true answer is known, and letting the app show a recovered pair back
/// through the glasses as a check.
pub fn encode_anaglyph(left: &FrameF32, right: &FrameF32, format: AnaglyphFormat) -> FrameF32 {
    assert!(
        left.same_size(right),
        "the two eyes must be the same size to mux"
    );
    let (w, h) = (left.width(), left.height());
    let (lr, lg, _) = left.rgb_planes();
    let (rr, rg, rb) = right.rgb_planes();

    match format {
        // Red passes the left eye; cyan — green and blue — passes the right.
        AnaglyphFormat::RedCyan => FrameF32::from_rgb_planes(w, h, lr, rg, rb),
        // Green passes the left eye; magenta — red and blue — passes the right.
        AnaglyphFormat::GreenMagenta => FrameF32::from_rgb_planes(w, h, rr, lg, rb),
        // Neither filter passes green cleanly, so it follows the red eye.
        AnaglyphFormat::RedBlue => FrameF32::from_rgb_planes(w, h, lr, lg, rb),
        // Amber is red plus green, both from the left eye; blue is the right.
        AnaglyphFormat::ColorCode => FrameF32::from_rgb_planes(w, h, lr, lg, rb),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn single_pixel(r: f32, g: f32, b: f32) -> FrameF32 {
        FrameF32::from_rgb_planes(1, 1, &[r], &[g], &[b])
    }

    fn eyes(r: f32, g: f32, b: f32, format: AnaglyphFormat) -> (f32, f32) {
        let (l, right) = extract_eyes(&single_pixel(r, g, b), format);
        (l.plane(0)[0], right.plane(0)[0])
    }

    #[test]
    fn red_cyan_takes_red_for_left_and_green_for_right() {
        let (l, r) = eyes(0.8, 0.3, 0.9, AnaglyphFormat::RedCyan);
        assert_eq!(l, 0.8, "left eye is the red channel");
        assert_eq!(r, 0.3, "right eye is the green channel");
    }

    #[test]
    fn red_cyan_ignores_the_blue_channel() {
        // The original AviSynth script found blue too noisy to use, even though
        // cyan is green plus blue. Changing blue must not move either eye.
        let quiet = eyes(0.8, 0.3, 0.0, AnaglyphFormat::RedCyan);
        let noisy = eyes(0.8, 0.3, 1.0, AnaglyphFormat::RedCyan);
        assert_eq!(quiet, noisy, "blue must not influence red/cyan extraction");
    }

    #[test]
    fn green_magenta_takes_green_for_left() {
        let (l, _) = eyes(0.8, 0.3, 0.9, AnaglyphFormat::GreenMagenta);
        assert_eq!(l, 0.3, "left eye is the green channel");
    }

    #[test]
    fn green_magenta_right_is_a_luminance_weighted_red_blue_mix() {
        // Only red and blue survive in the magenta pair, so the right eye's
        // luminance is estimated from those two, renormalised so that a neutral
        // magenta of value v yields v.
        let (_, r) = eyes(0.6, 0.0, 0.2, AnaglyphFormat::GreenMagenta);
        let expected = (0.2126 * 0.6 + 0.0722 * 0.2) / (0.2126 + 0.0722);
        assert!((r - expected).abs() < 1e-6, "expected {expected}, got {r}");
    }

    #[test]
    fn green_magenta_right_preserves_a_neutral_level() {
        // A magenta pair both at 0.5 must come back as 0.5, not something dimmer.
        let (_, r) = eyes(0.5, 0.0, 0.5, AnaglyphFormat::GreenMagenta);
        assert!(
            (r - 0.5).abs() < 1e-6,
            "neutral magenta must round-trip, got {r}"
        );
    }

    #[test]
    fn green_magenta_weights_red_more_heavily_than_blue() {
        let (_, red_only) = eyes(1.0, 0.0, 0.0, AnaglyphFormat::GreenMagenta);
        let (_, blue_only) = eyes(0.0, 0.0, 1.0, AnaglyphFormat::GreenMagenta);
        assert!(
            red_only > blue_only,
            "red carries more luminance than blue: {red_only} vs {blue_only}"
        );
    }

    #[test]
    fn red_blue_takes_red_for_left_and_blue_for_right() {
        let (l, r) = eyes(0.8, 0.3, 0.9, AnaglyphFormat::RedBlue);
        assert_eq!(l, 0.8);
        assert_eq!(r, 0.9);
    }

    #[test]
    fn output_frames_are_grey_and_keep_source_geometry() {
        let anaglyph = FrameF32::new_rgb(7, 5);
        let (l, r) = extract_eyes(&anaglyph, AnaglyphFormat::RedCyan);
        for (name, frame) in [("left", &l), ("right", &r)] {
            assert_eq!(frame.channels(), 1, "{name} must be single-channel");
            assert_eq!(frame.width(), 7, "{name} width");
            assert_eq!(frame.height(), 5, "{name} height");
        }
    }

    #[test]
    fn extraction_is_per_pixel_across_a_whole_plane() {
        let anaglyph =
            FrameF32::from_rgb_planes(3, 1, &[0.1, 0.2, 0.3], &[0.4, 0.5, 0.6], &[0.7, 0.8, 0.9]);
        let (l, r) = extract_eyes(&anaglyph, AnaglyphFormat::RedCyan);
        assert_eq!(l.plane(0), &[0.1, 0.2, 0.3]);
        assert_eq!(r.plane(0), &[0.4, 0.5, 0.6]);
    }

    fn pair() -> (FrameF32, FrameF32) {
        let left = FrameF32::from_rgb_planes(1, 1, &[0.9], &[0.5], &[0.1]);
        let right = FrameF32::from_rgb_planes(1, 1, &[0.2], &[0.7], &[0.4]);
        (left, right)
    }

    fn encoded(format: AnaglyphFormat) -> [f32; 3] {
        let (l, r) = pair();
        let a = encode_anaglyph(&l, &r, format);
        let (ar, ag, ab) = a.rgb_planes();
        [ar[0], ag[0], ab[0]]
    }

    #[test]
    fn red_cyan_takes_red_from_the_left_and_cyan_from_the_right() {
        assert_eq!(encoded(AnaglyphFormat::RedCyan), [0.9, 0.7, 0.4]);
    }

    #[test]
    fn green_magenta_takes_green_from_the_left_and_magenta_from_the_right() {
        assert_eq!(encoded(AnaglyphFormat::GreenMagenta), [0.2, 0.5, 0.4]);
    }

    #[test]
    fn red_blue_takes_blue_from_the_right() {
        assert_eq!(encoded(AnaglyphFormat::RedBlue), [0.9, 0.5, 0.4]);
    }

    #[test]
    fn encoding_then_extracting_returns_each_eye_projected_signal() {
        // The round trip that ties the two halves together: whatever an eye's
        // filter would have passed must survive being muxed and pulled apart.
        for format in AnaglyphFormat::ALL {
            let (l, r) = pair();
            let (lp, rp) = projections(format);
            let (got_l, got_r) = extract_eyes(&encode_anaglyph(&l, &r, format), format);

            let want_l = lp.apply(l.plane(0)[0], l.plane(1)[0], l.plane(2)[0]);
            let want_r = rp.apply(r.plane(0)[0], r.plane(1)[0], r.plane(2)[0]);
            assert!(
                (got_l.plane(0)[0] - want_l).abs() < 1e-6,
                "{format:?} left: {} vs {want_l}",
                got_l.plane(0)[0]
            );
            assert!(
                (got_r.plane(0)[0] - want_r).abs() < 1e-6,
                "{format:?} right: {} vs {want_r}",
                got_r.plane(0)[0]
            );
        }
    }

    #[test]
    fn encoding_preserves_geometry() {
        let a = encode_anaglyph(
            &FrameF32::new_rgb(6, 4),
            &FrameF32::new_rgb(6, 4),
            AnaglyphFormat::RedCyan,
        );
        assert_eq!((a.width(), a.height(), a.channels()), (6, 4, 3));
    }

    #[test]
    #[should_panic(expected = "same size")]
    fn encoding_rejects_mismatched_eyes() {
        encode_anaglyph(
            &FrameF32::new_rgb(6, 4),
            &FrameF32::new_rgb(4, 6),
            AnaglyphFormat::RedCyan,
        );
    }

    #[test]
    fn colorcode_left_is_an_amber_mix_of_red_and_green() {
        // Amber passes both, weighted by what each contributes to brightness
        // and renormalised so a neutral amber of value v yields v.
        let (l, _) = eyes(0.6, 0.4, 0.9, AnaglyphFormat::ColorCode);
        let expected = (0.2126 * 0.6 + 0.7152 * 0.4) / (0.2126 + 0.7152);
        assert!((l - expected).abs() < 1e-6, "expected {expected}, got {l}");
    }

    #[test]
    fn colorcode_left_ignores_blue() {
        let quiet = eyes(0.6, 0.4, 0.0, AnaglyphFormat::ColorCode);
        let noisy = eyes(0.6, 0.4, 1.0, AnaglyphFormat::ColorCode);
        assert_eq!(quiet.0, noisy.0, "blue must not reach the amber eye");
    }

    #[test]
    fn colorcode_right_is_the_blue_channel_alone() {
        let (_, r) = eyes(0.6, 0.4, 0.9, AnaglyphFormat::ColorCode);
        assert_eq!(r, 0.9);
    }

    #[test]
    fn colorcode_amber_weights_green_far_above_red() {
        // Which is why the amber eye keeps nearly all the luminance: green
        // carries most of it.
        let (green_only, _) = eyes(0.0, 1.0, 0.0, AnaglyphFormat::ColorCode);
        let (red_only, _) = eyes(1.0, 0.0, 0.0, AnaglyphFormat::ColorCode);
        assert!(
            green_only > red_only * 3.0,
            "green {green_only} should dominate red {red_only}"
        );
    }

    #[test]
    fn colorcode_preserves_a_neutral_level() {
        let (l, _) = eyes(0.5, 0.5, 0.0, AnaglyphFormat::ColorCode);
        assert!(
            (l - 0.5).abs() < 1e-6,
            "neutral amber must round-trip, got {l}"
        );
    }

    #[test]
    fn colorcode_differs_from_red_blue() {
        // The distinction that makes it a separate format: red/blue gives the
        // left eye red alone, ColorCode gives it red and green together.
        let cc = eyes(0.6, 0.4, 0.9, AnaglyphFormat::ColorCode).0;
        let rb = eyes(0.6, 0.4, 0.9, AnaglyphFormat::RedBlue).0;
        assert!((cc - rb).abs() > 0.05, "ColorCode {cc} vs red/blue {rb}");
    }

    #[test]
    fn colorcode_encodes_amber_from_the_left_and_blue_from_the_right() {
        assert_eq!(encoded(AnaglyphFormat::ColorCode), [0.9, 0.5, 0.4]);
    }

    #[test]
    fn every_format_is_offered_and_named() {
        for format in [
            AnaglyphFormat::RedCyan,
            AnaglyphFormat::GreenMagenta,
            AnaglyphFormat::RedBlue,
            AnaglyphFormat::ColorCode,
        ] {
            assert!(
                AnaglyphFormat::ALL.contains(&format),
                "{format:?} exists but is not offered"
            );
            assert!(!format.label().is_empty(), "{format:?} needs a label");
        }
    }

    #[test]
    fn red_blue_and_colorcode_write_the_same_pixels() {
        // Surprising enough to be worth stating outright, because it looks like
        // a bug from the outside: choose either in the preview and the picture
        // does not change.
        //
        // Both put the left eye's red and green in the red and green channels
        // and the right eye's blue in blue. Red/blue reads only the red channel
        // back, so its green is a free channel that happens to be filled the
        // same way; ColorCode reads red and green together through amber. The
        // encodings coincide, the projections do not — which is why the two
        // formats recover differently from the same file.
        //
        // If the ColorCode encoding is ever changed to something truer to the
        // format, this test should fail and be deleted rather than adjusted.
        let (left, right) = pair();
        let red_blue = encode_anaglyph(&left, &right, AnaglyphFormat::RedBlue);
        let colorcode = encode_anaglyph(&left, &right, AnaglyphFormat::ColorCode);
        assert_eq!(red_blue.as_slice(), colorcode.as_slice());

        // And the half that does differ: what each one reads back out.
        let (rb_left, _) = projections(AnaglyphFormat::RedBlue);
        let (cc_left, _) = projections(AnaglyphFormat::ColorCode);
        assert_ne!(
            (rb_left.r, rb_left.g),
            (cc_left.r, cc_left.g),
            "the two formats must at least disagree about how to read the file"
        );
    }

    #[test]
    fn every_projection_weights_sum_to_one() {
        // Restoration divides by a projected reference value, and both
        // reconstructions only stay exact if a neutral grey projects to itself.
        for format in AnaglyphFormat::ALL {
            let (l, r) = projections(format);
            assert!(
                (l.weight_sum() - 1.0).abs() < 1e-6,
                "{format:?} left: {l:?}"
            );
            assert!(
                (r.weight_sum() - 1.0).abs() < 1e-6,
                "{format:?} right: {r:?}"
            );
        }
    }

    #[test]
    fn extraction_agrees_with_the_declared_projection() {
        // The invariant the whole recovery rests on.
        let px = [0.6, 0.35, 0.2];
        for format in AnaglyphFormat::ALL {
            let (lp, rp) = projections(format);
            let (l, r) = eyes(px[0], px[1], px[2], format);
            assert!(
                (l - lp.apply(px[0], px[1], px[2])).abs() < 1e-6,
                "{format:?} left"
            );
            assert!(
                (r - rp.apply(px[0], px[1], px[2])).abs() < 1e-6,
                "{format:?} right"
            );
        }
    }

    #[test]
    fn negative_headroom_passes_through_extraction() {
        let (l, _) = eyes(-0.25, 0.0, 0.0, AnaglyphFormat::RedCyan);
        assert_eq!(
            l, -0.25,
            "extraction must not clamp; leak correction does that"
        );
    }
}
