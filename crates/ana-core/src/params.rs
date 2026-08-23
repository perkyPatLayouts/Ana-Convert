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
use crate::restore::ColourRestore;
use crate::transfer::TransferFunction;

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

/// Everything needed to convert one movie.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct ConvertParams {
    /// Which anaglyph encoding the source uses.
    pub input_format: AnaglyphFormat,
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
    /// Frames to shift the 2D source by, to bring it into sync.
    pub mono_frame_offset: i32,

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
            input_format: AnaglyphFormat::RedCyan,
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
            restore: ColourRestore::Scale,
            grade_left: Grade::default(),
            grade_right: Grade::default(),
            mono_eye: MonoEye::None,
            mono_frame_offset: 0,
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
        range("defringe_left", self.defringe_left, 1.0, 32.0)?;
        range("defringe_right", self.defringe_right, 1.0, 32.0)?;

        if let Some((w, h)) = self.output_size {
            if w == 0 || h == 0 {
                return Err(ParamsError::EmptyOutputSize(w, h));
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
    fn params_round_trip_through_json() {
        let p = ConvertParams {
            input_format: AnaglyphFormat::GreenMagenta,
            leak_correct_right: 12.5,
            mono_eye: MonoEye::Left,
            mono_frame_offset: -3,
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
