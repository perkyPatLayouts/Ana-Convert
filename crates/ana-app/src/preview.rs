// Ana-Convert — anaglyph 3D to full-colour stereo recovery
// SPDX-License-Identifier: GPL-3.0-or-later

//! Keeping the preview both fast and honest.
//!
//! Two problems, neither of them about drawing. Decoding a frame costs an
//! ffmpeg launch, so it must not happen every time a slider moves; and a
//! preview shown at reduced resolution must display the *same* conversion the
//! full render will produce, or the tuning it enables is worthless.

use ana_core::params::ConvertParams;
#[cfg(test)]
use ana_core::params::SourceTrim;

/// What has to be recomputed before the preview is correct again.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PreviewWork {
    /// The cached image is current.
    Nothing,
    /// The decoded frame still applies; only the conversion must be redone.
    Reprocess,
    /// A different frame is wanted, so it must be decoded first.
    DecodeAndProcess,
}

/// Tracks what the preview already holds, so a slider drag never triggers a
/// decode and a scrub never skips one.
#[derive(Debug, Default)]
pub struct PreviewCache {
    decoded_frame: Option<u64>,
    processed: Option<(u64, ConvertParams)>,
}

impl PreviewCache {
    pub fn new() -> Self {
        Self::default()
    }

    /// What must happen to show `frame` under `params`.
    pub fn work_for(&self, frame: u64, params: &ConvertParams) -> PreviewWork {
        if self.decoded_frame != Some(frame) {
            return PreviewWork::DecodeAndProcess;
        }
        match &self.processed {
            Some((done_frame, done_params)) if *done_frame == frame && done_params == params => {
                PreviewWork::Nothing
            }
            _ => PreviewWork::Reprocess,
        }
    }

    /// Records that `frame` has been decoded and is in hand.
    pub fn record_decode(&mut self, frame: u64) {
        self.decoded_frame = Some(frame);
        // A new frame invalidates whatever was converted from the old one.
        self.processed = None;
    }

    /// Records that `frame` has been converted under `params`.
    pub fn record_process(&mut self, frame: u64, params: &ConvertParams) {
        self.processed = Some((frame, params.clone()));
    }

    /// Drops everything, for when the source files change underneath us.
    pub fn invalidate(&mut self) {
        self.decoded_frame = None;
        self.processed = None;
    }
}

/// Rewrites the blur settings so a preview rendered at `scale` shows the same
/// blur *relative to the frame* as the full-size render.
///
/// Both blur controls are pixel radii, not fractions of frame width. Previewing
/// a half-size frame with the settings meant for full size would show half the
/// blur, and every value dialled in against it would be wrong. `scale` is the
/// preview's width divided by the source's.
pub fn scale_params_for_preview(params: &ConvertParams, scale: f32) -> ConvertParams {
    let scale = scale.clamp(0.01, 1.0);
    ConvertParams {
        decimate_horiz: scale_decimate(params.decimate_horiz, scale),
        decimate_vert: scale_decimate(params.decimate_vert, scale),
        defringe_left: scale_shrink(params.defringe_left, scale),
        defringe_right: scale_shrink(params.defringe_right, scale),
        ..params.clone()
    }
}

/// A decimate percentage whose sigma is `scale` times the original's.
///
/// `sigma = (100/d - 1) / 2`, so holding `sigma * scale` and solving for the
/// percentage gives `100 / (1 + (100/d - 1) * scale)`.
fn scale_decimate(decimate: f32, scale: f32) -> f32 {
    let shrink = 100.0 / decimate.clamp(0.1, 100.0);
    (100.0 / (1.0 + (shrink - 1.0) * scale)).clamp(0.1, 100.0)
}

/// The same for a de-fringe shrink factor, where `sigma = (f - 1) / 2`.
fn scale_shrink(shrink: f32, scale: f32) -> f32 {
    (1.0 + (shrink.max(1.0) - 1.0) * scale).clamp(1.0, 32.0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use ana_core::blur::{sigma_from_decimate, sigma_from_shrink};

    fn other_params() -> ConvertParams {
        ConvertParams {
            leak_correct_left: 12.0,
            ..Default::default()
        }
    }

    // --- cache ---

    #[test]
    fn a_fresh_cache_must_decode() {
        let cache = PreviewCache::new();
        assert_eq!(
            cache.work_for(0, &ConvertParams::default()),
            PreviewWork::DecodeAndProcess
        );
    }

    #[test]
    fn nothing_is_needed_once_a_frame_is_decoded_and_processed() {
        let params = ConvertParams::default();
        let mut cache = PreviewCache::new();
        cache.record_decode(7);
        cache.record_process(7, &params);
        assert_eq!(cache.work_for(7, &params), PreviewWork::Nothing);
    }

    #[test]
    fn changing_a_parameter_reprocesses_without_decoding_again() {
        // The whole point: dragging a slider must not relaunch ffmpeg.
        let mut cache = PreviewCache::new();
        cache.record_decode(7);
        cache.record_process(7, &ConvertParams::default());
        assert_eq!(cache.work_for(7, &other_params()), PreviewWork::Reprocess);
    }

    #[test]
    fn scrubbing_to_another_frame_decodes() {
        let params = ConvertParams::default();
        let mut cache = PreviewCache::new();
        cache.record_decode(7);
        cache.record_process(7, &params);
        assert_eq!(cache.work_for(8, &params), PreviewWork::DecodeAndProcess);
    }

    #[test]
    fn a_decoded_but_unprocessed_frame_still_needs_processing() {
        let mut cache = PreviewCache::new();
        cache.record_decode(3);
        assert_eq!(
            cache.work_for(3, &ConvertParams::default()),
            PreviewWork::Reprocess
        );
    }

    #[test]
    fn invalidating_forces_a_decode() {
        let params = ConvertParams::default();
        let mut cache = PreviewCache::new();
        cache.record_decode(2);
        cache.record_process(2, &params);
        cache.invalidate();
        assert_eq!(cache.work_for(2, &params), PreviewWork::DecodeAndProcess);
    }

    // --- preview scaling ---

    #[test]
    fn full_scale_changes_nothing() {
        let params = other_params();
        assert_eq!(scale_params_for_preview(&params, 1.0), params);
    }

    #[test]
    fn half_scale_halves_the_effective_colour_blur() {
        // Measured as sigma, because that is what the blur actually applies.
        let params = ConvertParams::default();
        let scaled = scale_params_for_preview(&params, 0.5);
        for (full, small) in [
            (params.decimate_horiz, scaled.decimate_horiz),
            (params.decimate_vert, scaled.decimate_vert),
        ] {
            let want = sigma_from_decimate(full) * 0.5;
            let got = sigma_from_decimate(small);
            assert!(
                (got - want).abs() < 1e-3,
                "decimate {full} -> {small}: sigma {got}, wanted {want}"
            );
        }
    }

    #[test]
    fn a_quarter_scale_quarters_the_effective_colour_blur() {
        let params = ConvertParams::default();
        let scaled = scale_params_for_preview(&params, 0.25);
        let want = sigma_from_decimate(params.decimate_horiz) * 0.25;
        let got = sigma_from_decimate(scaled.decimate_horiz);
        assert!((got - want).abs() < 1e-3, "sigma {got}, wanted {want}");
    }

    #[test]
    fn de_fringe_scales_the_same_way() {
        let params = ConvertParams {
            defringe_left: 3.0,
            defringe_right: 2.0,
            ..Default::default()
        };
        let scaled = scale_params_for_preview(&params, 0.5);
        for (full, small) in [
            (params.defringe_left, scaled.defringe_left),
            (params.defringe_right, scaled.defringe_right),
        ] {
            let want = sigma_from_shrink(full) * 0.5;
            let got = sigma_from_shrink(small);
            assert!((got - want).abs() < 1e-3, "defringe {full} -> {small}");
        }
    }

    #[test]
    fn de_fringe_that_is_off_stays_off() {
        // Exactly 1.0 means disabled, and scaling must not switch it on.
        let scaled = scale_params_for_preview(&ConvertParams::default(), 0.3);
        assert_eq!(scaled.defringe_left, 1.0);
        assert_eq!(scaled.defringe_right, 1.0);
    }

    #[test]
    fn settings_unrelated_to_blur_are_left_alone() {
        let params = ConvertParams {
            leak_correct_left: 12.0,
            leak_correct_right: -5.0,
            swap_eyes: true,
            mono_trim: SourceTrim {
                start: 40,
                end: Some(900),
            },
            ..Default::default()
        };
        let scaled = scale_params_for_preview(&params, 0.5);
        assert_eq!(scaled.leak_correct_left, 12.0);
        assert_eq!(scaled.leak_correct_right, -5.0);
        assert!(scaled.swap_eyes);
        assert_eq!(
            scaled.mono_trim, params.mono_trim,
            "alignment is not a blur setting"
        );
        assert_eq!(scaled.input_format, params.input_format);
        assert_eq!(scaled.restore, params.restore);
    }

    #[test]
    fn scaled_settings_are_still_valid() {
        // The conversion must accept whatever this produces, at any scale and
        // for the extremes of both controls.
        for scale in [0.05, 0.1, 0.5, 0.99, 1.0] {
            for decimate in [0.1, 1.0, 5.0, 50.0, 100.0] {
                let params = ConvertParams {
                    decimate_horiz: decimate,
                    decimate_vert: decimate,
                    defringe_left: 4.0,
                    ..Default::default()
                };
                let scaled = scale_params_for_preview(&params, scale);
                assert!(
                    scaled.validate().is_ok(),
                    "scale {scale}, decimate {decimate} produced {:?}: {:?}",
                    scaled.decimate_horiz,
                    scaled.validate()
                );
            }
        }
    }
}
