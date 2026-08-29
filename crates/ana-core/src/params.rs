// Ana-Convert — anaglyph 3D to full-colour stereo recovery
// SPDX-License-Identifier: GPL-3.0-or-later

//! The per-movie conversion settings.
//!
//! These replace the hand-edited `XX-DeAna.avs` parameter file. Names and
//! units follow the original wherever a user would recognise them — decimate
//! percentages, leak percentages, `1.0`-means-off de-fringe — so notes kept
//! against the AviSynth version still mean something here.

use serde::{Deserialize, Serialize};

use crate::blur::{sigma_from_decimate, sigma_from_shrink};
use crate::compose::{EyeOrder, OutputLayout};
use crate::extract::AnaglyphFormat;
use crate::grade::Grade;
use crate::packed::StereoPacking;
use crate::restore::ColourRestore;
use crate::transfer::TransferFunction;

/// The largest frame edge the pipeline will accept, in pixels.
///
/// Comfortably past anything real — a 16K stereo pair packed side by side is
/// 30720 wide — but small enough that a frame's sample count cannot come near
/// overflowing a `usize`. Without a bound somewhere, dimensions taken from a
/// file's own metadata or from a preset go straight into buffer arithmetic.
pub const MAX_DIMENSION: usize = 32768;

/// Which eye, if any, is supplied by a separate 2D release instead of being
/// recovered from the anaglyph.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[derive(Default)]
pub enum MonoEye {
    /// Recover both eyes from the anaglyph.
    #[default]
    None,
    Left,
    Right,
}

/// Which part of one source file to use.
///
/// A 2D release and the anaglyph it accompanies rarely start on the same
/// frame — different distributors, different logos, different credit rolls —
/// and may not run the same length either. Giving every source its own range
/// says "these two frames are the same moment" without assuming anything about
/// what came before.
///
/// `end` is inclusive, because it names a frame someone looked at and marked.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct SourceTrim {
    /// First frame to read from this source.
    pub start: u64,
    /// Last frame to read, inclusive. `None` runs to the end of the file.
    pub end: Option<u64>,
}

impl SourceTrim {
    /// A trim that uses the whole file.
    pub fn whole() -> Self {
        Self::default()
    }

    /// True when this uses the whole file, so the UI can stay quiet about it.
    pub fn is_whole(&self) -> bool {
        *self == Self::default()
    }

    /// How many frames this trim yields from a source of `total` frames.
    ///
    /// Zero for a range that starts past the end or finishes before it starts,
    /// rather than an error: a half-set alignment should stop the render, not
    /// crash it.
    pub fn length(&self, total: u64) -> u64 {
        let last = match self.end {
            Some(end) => end.min(total.saturating_sub(1)),
            None => total.saturating_sub(1),
        };
        if total == 0 || self.start > last {
            return 0;
        }
        last - self.start + 1
    }

    /// The absolute frame `offset` frames into this trim.
    pub fn frame_at(&self, offset: u64) -> u64 {
        self.start.saturating_add(offset)
    }

    /// How far into this trim an absolute frame sits, if it is inside at all.
    pub fn offset_of(&self, frame: u64) -> Option<u64> {
        if frame < self.start {
            return None;
        }
        match self.end {
            Some(end) if frame > end => None,
            _ => Some(frame - self.start),
        }
    }
}

/// What the input file holds, and therefore what has to be done to it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case", tag = "kind")]
pub enum InputMode {
    /// A red/cyan, green/magenta or red/blue anaglyph, to be recovered.
    #[default]
    Anaglyph,
    /// A stereo pair already packed into one frame. Nothing needs recovering —
    /// the two eyes only have to be taken apart.
    Packed {
        packing: StereoPacking,
        /// Which eye is stored first.
        order: EyeOrder,
        /// Each eye is squeezed to half size and must be stretched back.
        anamorphic: bool,
    },
    /// Two files, one per eye. Nothing needs recovering or splitting; the pair
    /// is simply read from two places at once.
    TwoFiles,
}

impl InputMode {
    /// A packed source with the usual arrangement.
    pub fn packed(packing: StereoPacking, anamorphic: bool) -> Self {
        Self::Packed {
            packing,
            order: EyeOrder::LeftFirst,
            anamorphic,
        }
    }

    pub fn is_anaglyph(self) -> bool {
        matches!(self, Self::Anaglyph)
    }

    /// True when a second video file supplies the other eye.
    pub fn needs_second_file(self) -> bool {
        matches!(self, Self::TwoFiles)
    }

    /// The size of one eye, given the source frame size.
    pub fn eye_size(self, source: (usize, usize)) -> (usize, usize) {
        match self {
            // Recovery works at full frame size.
            Self::Anaglyph => source,
            Self::Packed {
                packing,
                anamorphic,
                ..
            } => crate::packed::eye_size(source, packing, anamorphic),
            // Each file is already one whole eye.
            Self::TwoFiles => source,
        }
    }
}

/// Everything needed to convert one movie.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct ConvertParams {
    /// What the source file holds.
    pub input: InputMode,
    /// Which anaglyph encoding the source uses.
    pub input_format: AnaglyphFormat,
    /// The encoding written when the output layout is [`OutputLayout::Anaglyph`].
    ///
    /// Deliberately separate from the source's. Recovering a red/cyan transfer
    /// and writing it back as green/magenta is a reasonable thing to want —
    /// green/magenta glasses hold colour better — and tying the two together
    /// would make it impossible.
    pub output_format: AnaglyphFormat,
    /// The transfer function the decoded samples carry.
    pub transfer: TransferFunction,
    /// Convert to linear light before extraction, cross-talk and blur.
    pub work_in_linear_light: bool,

    /// Horizontal colour blur, as the original's shrink percentage. Small
    /// numbers blur harder; anaglyph disparity is horizontal, so this is
    /// normally much stronger than the vertical figure.
    pub decimate_horiz: f32,
    /// Vertical colour blur. Raise it for films whose cameras were misaligned.
    pub decimate_vert: f32,

    /// Percentage of the right eye to remove from the left (-100..=100).
    pub leak_correct_left: f32,
    /// Percentage of the left eye to remove from the right (-100..=100).
    pub leak_correct_right: f32,

    /// Horizontal de-fringe for the left eye. Exactly 1.0 disables it.
    pub defringe_left: f32,
    /// Horizontal de-fringe for the right eye. Exactly 1.0 disables it.
    pub defringe_right: f32,

    /// How eye brightness and reference colour are recombined.
    pub restore: ColourRestore,
    pub grade_left: Grade,
    pub grade_right: Grade,

    /// Which eye comes from a 2D release rather than the anaglyph.
    pub mono_eye: MonoEye,

    /// The part of the anaglyph to convert.
    pub anaglyph_trim: SourceTrim,
    /// The part of the colour source that lines up with it.
    pub colour_trim: SourceTrim,
    /// The part of the 2D eye source that lines up with it.
    pub mono_trim: SourceTrim,

    /// Horizontal convergence, as a percentage of frame width.
    ///
    /// Positive moves the eyes apart and the scene behind the screen; negative
    /// brings them together and the scene forward, placing the plane of zero
    /// parallax — the ground plane — where the viewer wants it. High-disparity
    /// shots are what make this worth having: the depth that reads as thrown
    /// at the camera is also what makes an audience's eyes ache.
    ///
    /// The output keeps only what both eyes cover, so it narrows by exactly
    /// this percentage.
    pub convergence: f32,

    /// Exchange the two eyes before layout.
    pub swap_eyes: bool,
    pub layout: OutputLayout,
    pub eye_order: EyeOrder,
    /// Final output size. `None` keeps the stacked size.
    pub output_size: Option<(usize, usize)>,
}

impl Default for ConvertParams {
    fn default() -> Self {
        Self {
            input: InputMode::Anaglyph,
            input_format: AnaglyphFormat::RedCyan,
            output_format: AnaglyphFormat::RedCyan,
            transfer: TransferFunction::Srgb,
            work_in_linear_light: true,
            // The values the original post recommends as a starting point.
            decimate_horiz: 5.0,
            decimate_vert: 20.0,
            // Corrections start off; they are per-movie by nature.
            leak_correct_left: 0.0,
            leak_correct_right: 0.0,
            defringe_left: 1.0,
            defringe_right: 1.0,
            restore: ColourRestore::default(),
            grade_left: Grade::default(),
            grade_right: Grade::default(),
            mono_eye: MonoEye::None,
            anaglyph_trim: SourceTrim::whole(),
            colour_trim: SourceTrim::whole(),
            mono_trim: SourceTrim::whole(),
            convergence: 0.0,
            swap_eyes: false,
            layout: OutputLayout::SideBySide,
            eye_order: EyeOrder::LeftFirst,
            output_size: None,
        }
    }
}

/// A parameter that would produce nonsense downstream.
#[derive(Debug, thiserror::Error, PartialEq)]
pub enum ParamsError {
    #[error("{name} must be between {min} and {max}, got {value}")]
    OutOfRange {
        name: &'static str,
        value: f32,
        min: f32,
        max: f32,
    },
    #[error("output size must be non-zero in both dimensions, got {0}x{1}")]
    EmptyOutputSize(usize, usize),
    #[error("output size must be at most {MAX_DIMENSION} in each dimension, got {0}x{1}")]
    OutputSizeTooLarge(usize, usize),
}

impl ConvertParams {
    /// Cross-talk fraction for the left eye, as the maths wants it.
    pub fn leak_left_fraction(&self) -> f32 {
        self.leak_correct_left / 100.0
    }

    /// Cross-talk fraction for the right eye.
    pub fn leak_right_fraction(&self) -> f32 {
        self.leak_correct_right / 100.0
    }

    /// Horizontal sigma for the colour reference blur.
    pub fn colour_sigma_x(&self) -> f32 {
        sigma_from_decimate(self.decimate_horiz)
    }

    /// Vertical sigma for the colour reference blur.
    pub fn colour_sigma_y(&self) -> f32 {
        sigma_from_decimate(self.decimate_vert)
    }

    /// Horizontal de-fringe sigma for the left eye.
    pub fn defringe_sigma_left(&self) -> f32 {
        sigma_from_shrink(self.defringe_left)
    }

    /// Horizontal de-fringe sigma for the right eye.
    pub fn defringe_sigma_right(&self) -> f32 {
        sigma_from_shrink(self.defringe_right)
    }

    /// The frame to read from a secondary source when the anaglyph is at
    /// `frame`, so that both land on the same moment.
    pub fn aligned_frame(&self, other: &SourceTrim, frame: u64) -> u64 {
        other.frame_at(frame.saturating_sub(self.anaglyph_trim.start))
    }

    /// The size of each recovered eye, given the source frame size.
    pub fn eye_size(&self, source: (usize, usize)) -> (usize, usize) {
        self.input.eye_size(source)
    }

    /// The size of the finished frame the encoder will be handed.
    pub fn output_geometry(&self, source: (usize, usize)) -> (usize, usize) {
        if let Some(size) = self.output_size {
            return size;
        }
        let (w, h) = self.eye_size(source);
        let w = crate::compose::converged_width(w, self.convergence);
        match self.layout {
            OutputLayout::SideBySide => (w * 2, h),
            OutputLayout::TopBottom => (w, h * 2),
            OutputLayout::Separate
            | OutputLayout::Anaglyph
            | OutputLayout::LeftOnly
            | OutputLayout::RightOnly => (w, h),
        }
    }

    /// The display aspect ratio of one eye, given the source frame's shape.
    ///
    /// `source_display_aspect` is the shape the whole source frame is meant to
    /// be seen at, pixel shape already accounted for.
    pub fn eye_display_aspect(&self, source_display_aspect: f64, source: (usize, usize)) -> f64 {
        let packed = self.packed_eye_display_aspect(source_display_aspect);
        // Cropping narrows the picture without touching pixel shape, so the
        // shape must narrow with it. Taking the ratio from the pixel widths
        // rather than from the percentage keeps this in step with
        // output_geometry even when the shift rounds.
        let (w, _) = self.eye_size(source);
        let kept = crate::compose::converged_width(w, self.convergence);
        packed * kept as f64 / w as f64
    }

    /// The eye's shape from packing alone, before convergence crops it.
    fn packed_eye_display_aspect(&self, source_display_aspect: f64) -> f64 {
        match self.input {
            // The eye is the whole frame.
            InputMode::Anaglyph | InputMode::TwoFiles => source_display_aspect,
            // A squeezed eye is meant to fill the frame's own shape.
            InputMode::Packed {
                anamorphic: true, ..
            } => source_display_aspect,
            // A full-resolution pair holds two frames' worth, so each eye is
            // half as wide (or twice as tall) as the packed frame.
            InputMode::Packed {
                packing: StereoPacking::SideBySide,
                ..
            } => source_display_aspect / 2.0,
            InputMode::Packed {
                packing: StereoPacking::TopBottom,
                ..
            } => source_display_aspect * 2.0,
        }
    }

    /// The display aspect ratio the finished frame should be seen at.
    ///
    /// Stacking two eyes doubles the width or the height, so the shape of the
    /// output is not the shape of an eye. Without this the encoder assumes
    /// square pixels and every non-square source comes out stretched.
    pub fn output_display_aspect(&self, source_display_aspect: f64, source: (usize, usize)) -> f64 {
        let eye = self.eye_display_aspect(source_display_aspect, source);
        match self.layout {
            OutputLayout::SideBySide => eye * 2.0,
            OutputLayout::TopBottom => eye / 2.0,
            OutputLayout::Separate
            | OutputLayout::Anaglyph
            | OutputLayout::LeftOnly
            | OutputLayout::RightOnly => eye,
        }
    }

    /// Rejects settings that cannot produce a sensible result.
    pub fn validate(&self) -> Result<(), ParamsError> {
        fn range(name: &'static str, value: f32, min: f32, max: f32) -> Result<(), ParamsError> {
            if value.is_finite() && (min..=max).contains(&value) {
                Ok(())
            } else {
                Err(ParamsError::OutOfRange {
                    name,
                    value,
                    min,
                    max,
                })
            }
        }

        range("decimate_horiz", self.decimate_horiz, 0.1, 100.0)?;
        range("decimate_vert", self.decimate_vert, 0.1, 100.0)?;
        range("leak_correct_left", self.leak_correct_left, -100.0, 100.0)?;
        range("leak_correct_right", self.leak_correct_right, -100.0, 100.0)?;
        range("convergence", self.convergence, -10.0, 10.0)?;
        range("defringe_left", self.defringe_left, 1.0, 32.0)?;
        range("defringe_right", self.defringe_right, 1.0, 32.0)?;

        if let Some((w, h)) = self.output_size {
            if w == 0 || h == 0 {
                return Err(ParamsError::EmptyOutputSize(w, h));
            }
            if w > MAX_DIMENSION || h > MAX_DIMENSION {
                return Err(ParamsError::OutputSizeTooLarge(w, h));
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The real transfer that exposed this: 708x276 stored with 8:9 pixels,
    /// so it is meant to be seen at 2.28:1 rather than the 2.57:1 its stored
    /// dimensions suggest.
    const REAL_DAR: f64 = 472.0 / 207.0;

    /// Aspect is independent of frame size unless convergence crops it, so
    /// tests that are not about convergence can pass any size.
    const ANY_SIZE: (usize, usize) = (1920, 1080);

    #[test]
    fn convergence_narrows_the_output() {
        let params = ConvertParams {
            convergence: 4.0,
            layout: OutputLayout::LeftOnly,
            ..Default::default()
        };
        assert_eq!(params.output_geometry((100, 50)), (96, 50));
    }

    #[test]
    fn convergence_narrows_the_display_aspect_by_exactly_what_it_crops() {
        // The failure this guards against is the one that reached the user
        // three times: geometry and aspect computed by separate routes, then
        // disagreeing, so every frame comes out stretched. Both must fall out
        // of the same crop.
        let params = ConvertParams {
            convergence: 6.0,
            layout: OutputLayout::LeftOnly,
            ..Default::default()
        };
        let source = (1000, 500);
        let (out_w, _) = params.output_geometry(source);
        let plain = ConvertParams {
            layout: OutputLayout::LeftOnly,
            ..Default::default()
        };
        let pixel_ratio = out_w as f64 / plain.output_geometry(source).0 as f64;
        let aspect_ratio = params.output_display_aspect(REAL_DAR, source)
            / plain.output_display_aspect(REAL_DAR, source);
        assert!(
            (pixel_ratio - aspect_ratio).abs() < 1e-9,
            "cropped {pixel_ratio}x the pixels but {aspect_ratio}x the shape"
        );
    }

    #[test]
    fn convergence_leaves_a_side_by_side_pair_stackable() {
        // Both eyes lose the same width, so the stacked frame stays exactly
        // twice one eye. 3% of 800 is 24px of overlap given up in total —
        // 12 from each eye — leaving 776.
        let params = ConvertParams {
            convergence: -3.0,
            layout: OutputLayout::SideBySide,
            ..Default::default()
        };
        let (w, h) = params.output_geometry((800, 400));
        assert_eq!((w, h), (2 * 776, 400));
    }

    #[test]
    fn zero_convergence_changes_no_geometry() {
        let params = ConvertParams {
            layout: OutputLayout::LeftOnly,
            ..Default::default()
        };
        assert_eq!(params.output_geometry((640, 480)), (640, 480));
        assert_eq!(params.output_display_aspect(REAL_DAR, (640, 480)), REAL_DAR);
    }

    #[test]
    fn convergence_past_the_limit_is_rejected() {
        let params = ConvertParams {
            convergence: 25.0,
            ..Default::default()
        };
        assert!(params.validate().is_err(), "25% should be out of range");
    }

    #[test]
    fn an_explicit_output_size_still_shows_the_right_shape() {
        // Resizing changes how many pixels are stored, not what the picture
        // looks like. Asking for 1920x1080 from a 2.28:1 source gives a
        // 1920x1080 file that still displays at 2.28:1, rather than a
        // stretched one — a resize should never distort.
        let p = ConvertParams {
            output_size: Some((1920, 1080)),
            layout: OutputLayout::LeftOnly,
            ..Default::default()
        };
        assert_eq!(p.output_geometry((708, 276)), (1920, 1080));
        assert!(
            (p.output_display_aspect(REAL_DAR, ANY_SIZE) - REAL_DAR).abs() < 1e-9,
            "the shape must survive the resize"
        );
    }

    #[test]
    fn an_anaglyph_eye_keeps_the_whole_frames_shape() {
        let p = ConvertParams::default();
        assert!((p.eye_display_aspect(REAL_DAR, ANY_SIZE) - REAL_DAR).abs() < 1e-9);
    }

    #[test]
    fn stacking_two_eyes_side_by_side_doubles_the_shape() {
        let p = ConvertParams {
            layout: OutputLayout::SideBySide,
            ..Default::default()
        };
        assert!((p.output_display_aspect(REAL_DAR, ANY_SIZE) - REAL_DAR * 2.0).abs() < 1e-9);
    }

    #[test]
    fn stacking_top_and_bottom_halves_the_shape() {
        let p = ConvertParams {
            layout: OutputLayout::TopBottom,
            ..Default::default()
        };
        assert!((p.output_display_aspect(REAL_DAR, ANY_SIZE) - REAL_DAR / 2.0).abs() < 1e-9);
    }

    #[test]
    fn a_single_eye_output_keeps_the_eyes_shape() {
        for layout in [
            OutputLayout::Anaglyph,
            OutputLayout::LeftOnly,
            OutputLayout::RightOnly,
            OutputLayout::Separate,
        ] {
            let p = ConvertParams {
                layout,
                ..Default::default()
            };
            assert!(
                (p.output_display_aspect(REAL_DAR, ANY_SIZE) - REAL_DAR).abs() < 1e-9,
                "{layout:?}"
            );
        }
    }

    #[test]
    fn a_full_resolution_packed_eye_is_half_the_packed_shape() {
        // A 3840x1080 frame at 32:9 holds two 16:9 eyes.
        let p = ConvertParams {
            input: InputMode::packed(StereoPacking::SideBySide, false),
            ..Default::default()
        };
        assert!((p.eye_display_aspect(32.0 / 9.0, ANY_SIZE) - 16.0 / 9.0).abs() < 1e-9);
    }

    #[test]
    fn a_squeezed_packed_eye_fills_the_packed_shape() {
        // A 1920x1080 frame at 16:9 holds two squeezed eyes, each of which is
        // meant to be seen at 16:9 once stretched back.
        let p = ConvertParams {
            input: InputMode::packed(StereoPacking::SideBySide, true),
            ..Default::default()
        };
        assert!((p.eye_display_aspect(16.0 / 9.0, ANY_SIZE) - 16.0 / 9.0).abs() < 1e-9);
    }

    #[test]
    fn a_full_resolution_top_bottom_eye_is_twice_the_packed_shape() {
        // A 1920x2160 frame at 8:9 holds two 16:9 eyes.
        let p = ConvertParams {
            input: InputMode::packed(StereoPacking::TopBottom, false),
            ..Default::default()
        };
        assert!((p.eye_display_aspect(8.0 / 9.0, ANY_SIZE) - 16.0 / 9.0).abs() < 1e-9);
    }

    #[test]
    fn unpacking_and_repacking_returns_the_original_shape() {
        // Take a full side-by-side pair apart and stack it again: the finished
        // frame must be the shape it started as.
        let p = ConvertParams {
            input: InputMode::packed(StereoPacking::SideBySide, false),
            layout: OutputLayout::SideBySide,
            ..Default::default()
        };
        assert!((p.output_display_aspect(32.0 / 9.0, ANY_SIZE) - 32.0 / 9.0).abs() < 1e-9);
    }

    #[test]
    fn an_untrimmed_source_yields_every_frame() {
        let trim = SourceTrim::whole();
        assert!(trim.is_whole());
        assert_eq!(trim.length(500), 500);
        assert_eq!(trim.frame_at(0), 0);
    }

    #[test]
    fn a_start_point_drops_the_frames_before_it() {
        let trim = SourceTrim {
            start: 100,
            end: None,
        };
        assert_eq!(trim.length(500), 400);
        assert_eq!(
            trim.frame_at(0),
            100,
            "the first converted frame is the start"
        );
        assert_eq!(trim.frame_at(7), 107);
    }

    #[test]
    fn an_end_point_is_inclusive() {
        // It names a frame someone looked at and marked, so it is part of the
        // range rather than one past it.
        let trim = SourceTrim {
            start: 10,
            end: Some(19),
        };
        assert_eq!(trim.length(500), 10);
    }

    #[test]
    fn an_end_beyond_the_file_is_clamped_to_it() {
        let trim = SourceTrim {
            start: 0,
            end: Some(9_999),
        };
        assert_eq!(trim.length(100), 100);
    }

    #[test]
    fn a_range_that_starts_past_the_end_yields_nothing() {
        // A half-finished alignment should stop the render, not crash it.
        assert_eq!(
            SourceTrim {
                start: 900,
                end: None
            }
            .length(100),
            0
        );
        assert_eq!(
            SourceTrim {
                start: 50,
                end: Some(20)
            }
            .length(100),
            0
        );
        assert_eq!(SourceTrim::whole().length(0), 0);
    }

    #[test]
    fn a_frame_maps_into_the_trim_and_back() {
        let trim = SourceTrim {
            start: 60,
            end: Some(120),
        };
        assert_eq!(trim.offset_of(60), Some(0));
        assert_eq!(trim.offset_of(75), Some(15));
        assert_eq!(trim.offset_of(59), None, "before the start");
        assert_eq!(trim.offset_of(121), None, "past the end");
    }

    #[test]
    fn two_sources_starting_at_different_points_align() {
        // The whole purpose: the anaglyph begins at 100, the 2D copy shows the
        // same moment at 340, so converting anaglyph frame 105 must read 345.
        let params = ConvertParams {
            anaglyph_trim: SourceTrim {
                start: 100,
                end: None,
            },
            mono_trim: SourceTrim {
                start: 340,
                end: None,
            },
            ..Default::default()
        };
        assert_eq!(params.aligned_frame(&params.mono_trim, 100), 340);
        assert_eq!(params.aligned_frame(&params.mono_trim, 105), 345);
    }

    #[test]
    fn a_source_that_needs_no_shift_maps_one_to_one() {
        let params = ConvertParams::default();
        assert_eq!(params.aligned_frame(&params.colour_trim, 42), 42);
    }

    #[test]
    fn trims_survive_a_preset_round_trip() {
        let params = ConvertParams {
            anaglyph_trim: SourceTrim {
                start: 120,
                end: Some(9000),
            },
            mono_trim: SourceTrim {
                start: 355,
                end: None,
            },
            ..Default::default()
        };
        let json = serde_json::to_string(&params).expect("serialise");
        let back: ConvertParams = serde_json::from_str(&json).expect("deserialise");
        assert_eq!(back, params);
    }

    #[test]
    fn a_preset_without_trims_still_loads() {
        // Presets written before alignment existed must keep working.
        let back: ConvertParams =
            serde_json::from_str(r#"{"decimate_horiz": 4.0}"#).expect("deserialise");
        assert!(back.anaglyph_trim.is_whole());
        assert!(back.mono_trim.is_whole());
    }

    #[test]
    fn defaults_are_valid_and_do_nothing_but_recover() {
        let p = ConvertParams::default();
        assert_eq!(p.validate(), Ok(()));
        assert_eq!(p.leak_left_fraction(), 0.0, "no cross-talk correction");
        assert_eq!(p.defringe_sigma_left(), 0.0, "no de-fringe");
        assert!(p.grade_left.is_identity(), "no grading");
        assert!(!p.swap_eyes);
    }

    #[test]
    fn defaults_blur_colour_harder_horizontally_than_vertically() {
        // The whole reason the two axes are separate controls.
        let p = ConvertParams::default();
        assert!(
            p.colour_sigma_x() > p.colour_sigma_y(),
            "{} vs {}",
            p.colour_sigma_x(),
            p.colour_sigma_y()
        );
    }

    #[test]
    fn leak_percentages_become_fractions() {
        let p = ConvertParams {
            leak_correct_left: 10.0,
            leak_correct_right: -25.0,
            ..Default::default()
        };
        assert!((p.leak_left_fraction() - 0.1).abs() < 1e-6);
        assert!((p.leak_right_fraction() + 0.25).abs() < 1e-6);
    }

    #[test]
    fn defringe_of_exactly_one_means_no_blur() {
        let p = ConvertParams {
            defringe_left: 1.0,
            defringe_right: 2.0,
            ..Default::default()
        };
        assert_eq!(p.defringe_sigma_left(), 0.0);
        assert!(p.defringe_sigma_right() > 0.0);
    }

    #[test]
    fn out_of_range_decimate_is_rejected() {
        let p = ConvertParams {
            decimate_horiz: 150.0,
            ..Default::default()
        };
        assert!(matches!(
            p.validate(),
            Err(ParamsError::OutOfRange {
                name: "decimate_horiz",
                ..
            })
        ));
    }

    #[test]
    fn out_of_range_leak_is_rejected() {
        let p = ConvertParams {
            leak_correct_right: 250.0,
            ..Default::default()
        };
        assert!(p.validate().is_err());
    }

    #[test]
    fn a_non_finite_parameter_is_rejected() {
        let p = ConvertParams {
            decimate_vert: f32::NAN,
            ..Default::default()
        };
        assert!(p.validate().is_err(), "NaN must not slip through");
    }

    #[test]
    fn a_zero_output_dimension_is_rejected() {
        let p = ConvertParams {
            output_size: Some((1920, 0)),
            ..Default::default()
        };
        assert_eq!(p.validate(), Err(ParamsError::EmptyOutputSize(1920, 0)));
    }

    #[test]
    fn an_absurd_output_dimension_is_rejected() {
        // A size this large is not a frame anyone wants; it is a number that
        // reaches buffer arithmetic. Refusing it here is the whole defence,
        // since a preset is just a file and can say anything.
        let p = ConvertParams {
            output_size: Some((usize::MAX, 1080)),
            ..Default::default()
        };
        assert_eq!(
            p.validate(),
            Err(ParamsError::OutputSizeTooLarge(usize::MAX, 1080))
        );
    }

    #[test]
    fn the_largest_allowed_output_size_is_accepted() {
        // The bound has to sit above anything real, or it becomes the bug.
        let p = ConvertParams {
            output_size: Some((MAX_DIMENSION, MAX_DIMENSION)),
            ..Default::default()
        };
        assert_eq!(p.validate(), Ok(()));
    }

    #[test]
    fn params_round_trip_through_json() {
        let p = ConvertParams {
            input_format: AnaglyphFormat::GreenMagenta,
            leak_correct_right: 12.5,
            mono_eye: MonoEye::Left,
            mono_trim: SourceTrim {
                start: 12,
                end: Some(400),
            },
            layout: OutputLayout::TopBottom,
            output_size: Some((1920, 1080)),
            ..Default::default()
        };
        let json = serde_json::to_string_pretty(&p).expect("serialise");
        let back: ConvertParams = serde_json::from_str(&json).expect("deserialise");
        assert_eq!(p, back);
    }

    #[test]
    fn a_preset_missing_fields_falls_back_to_defaults() {
        // Presets saved by an older build must keep loading.
        let back: ConvertParams =
            serde_json::from_str(r#"{"leak_correct_right": 8.0}"#).expect("deserialise");
        assert_eq!(back.leak_correct_right, 8.0);
        assert_eq!(back.decimate_horiz, ConvertParams::default().decimate_horiz);
        assert_eq!(back.input_format, AnaglyphFormat::RedCyan);
    }

    #[test]
    fn enums_serialise_as_readable_snake_case() {
        let json = serde_json::to_string(&ConvertParams {
            input_format: AnaglyphFormat::GreenMagenta,
            layout: OutputLayout::TopBottom,
            ..Default::default()
        })
        .expect("serialise");
        assert!(json.contains("green_magenta"), "got {json}");
        assert!(json.contains("top_bottom"), "got {json}");
    }
}
