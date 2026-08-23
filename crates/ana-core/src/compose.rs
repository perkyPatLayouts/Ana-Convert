// Ana-Convert — anaglyph 3D to full-colour stereo recovery
// SPDX-License-Identifier: GPL-3.0-or-later

//! Assembling the two recovered eyes into a deliverable, and resampling.

use crate::frame::FrameF32;

/// Lanczos `a` parameter. Three lobes is the usual quality/ringing compromise.
const LANCZOS_A: f32 = 3.0;

/// How the two eyes are packed into the output.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
#[derive(Default)]
pub enum OutputLayout {
    /// One frame, twice as wide.
    #[default]
    SideBySide,
    /// One frame, twice as tall.
    TopBottom,
    /// Two independent streams.
    Separate,
}

/// Which eye is placed first — left or top.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
#[derive(Default)]
pub enum EyeOrder {
    #[default]
    LeftFirst,
    RightFirst,
}

/// Places `a` to the left of `b`.
pub fn stack_horizontal(a: &FrameF32, b: &FrameF32) -> FrameF32 {
    assert_eq!(a.height(), b.height(), "side-by-side needs the same height");
    assert_eq!(a.channels(), b.channels(), "channel counts must match");

    let (w, h) = (a.width() + b.width(), a.height());
    let mut out = FrameF32::filled(w, h, a.channels(), 0.0);
    for c in 0..a.channels() {
        let (src_a, src_b) = (a.plane(c), b.plane(c));
        let dst = out.plane_mut(c);
        for y in 0..h {
            let row = &mut dst[y * w..(y + 1) * w];
            row[..a.width()].copy_from_slice(&src_a[y * a.width()..(y + 1) * a.width()]);
            row[a.width()..].copy_from_slice(&src_b[y * b.width()..(y + 1) * b.width()]);
        }
    }
    out
}

/// Places `a` above `b`.
pub fn stack_vertical(a: &FrameF32, b: &FrameF32) -> FrameF32 {
    assert_eq!(a.width(), b.width(), "top-bottom needs the same width");
    assert_eq!(a.channels(), b.channels(), "channel counts must match");

    let (w, h) = (a.width(), a.height() + b.height());
    let mut out = FrameF32::filled(w, h, a.channels(), 0.0);
    for c in 0..a.channels() {
        let split = a.plane_len();
        let dst = out.plane_mut(c);
        dst[..split].copy_from_slice(a.plane(c));
        dst[split..].copy_from_slice(b.plane(c));
    }
    out
}

/// Resamples to exact dimensions with a separable Lanczos-3 filter.
///
/// The filter widens when downscaling, so shrinking averages the pixels that
/// are being discarded rather than aliasing them.
pub fn resize(frame: &FrameF32, width: usize, height: usize) -> FrameF32 {
    if frame.width() == width && frame.height() == height {
        return frame.clone();
    }
    assert!(width > 0 && height > 0, "resize target must be non-empty");

    let channels = frame.channels();
    let (sw, sh) = (frame.width(), frame.height());
    let taps_x = axis_taps(sw, width);
    let taps_y = axis_taps(sh, height);

    let mut out = FrameF32::filled(width, height, channels, 0.0);
    let mut horizontal = vec![0.0f32; width * sh];

    for c in 0..channels {
        let src = frame.plane(c);
        for y in 0..sh {
            for (x, tap) in taps_x.iter().enumerate() {
                horizontal[y * width + x] = tap.apply(&src[y * sw..(y + 1) * sw], 1);
            }
        }
        let dst = out.plane_mut(c);
        for (y, tap) in taps_y.iter().enumerate() {
            for x in 0..width {
                dst[y * width + x] = tap.apply(&horizontal[x..], width);
            }
        }
    }
    out
}

/// A precomputed set of source samples and weights for one output position.
struct Tap {
    start: usize,
    weights: Vec<f32>,
}

impl Tap {
    /// Weighted sum over `src`, walking `stride` samples at a time so the same
    /// tap serves both row-wise and column-wise passes.
    fn apply(&self, src: &[f32], stride: usize) -> f32 {
        self.weights
            .iter()
            .enumerate()
            .map(|(i, &w)| w * src[(self.start + i) * stride])
            .sum()
    }
}

/// Builds the Lanczos taps mapping `src_len` samples onto `dst_len`.
fn axis_taps(src_len: usize, dst_len: usize) -> Vec<Tap> {
    let scale = dst_len as f32 / src_len as f32;
    // Widening the filter when shrinking is what turns discarded detail into
    // an average instead of an alias.
    let filter_scale = if scale < 1.0 { 1.0 / scale } else { 1.0 };
    let support = LANCZOS_A * filter_scale;

    (0..dst_len)
        .map(|i| {
            let centre = (i as f32 + 0.5) / scale - 0.5;
            let first = (centre - support).ceil() as isize;
            let last = (centre + support).floor() as isize;

            let mut weights = Vec::with_capacity((last - first + 1).max(1) as usize);
            let mut sum = 0.0;
            for s in first..=last {
                let w = lanczos((s as f32 - centre) / filter_scale);
                weights.push(w);
                sum += w;
            }
            if sum != 0.0 {
                for w in &mut weights {
                    *w /= sum;
                }
            }

            // Clamp to the edge by folding out-of-range taps onto the border
            // sample, keeping the weights summing to one.
            let start = first.clamp(0, src_len as isize - 1) as usize;
            let mut clamped = vec![0.0f32; (last.max(0) as usize).min(src_len - 1) - start + 1];
            for (i, &w) in weights.iter().enumerate() {
                let s = (first + i as isize).clamp(0, src_len as isize - 1) as usize;
                clamped[s - start] += w;
            }
            Tap {
                start,
                weights: clamped,
            }
        })
        .collect()
}

fn lanczos(x: f32) -> f32 {
    if x.abs() < 1e-6 {
        return 1.0;
    }
    if x.abs() >= LANCZOS_A {
        return 0.0;
    }
    let px = std::f32::consts::PI * x;
    LANCZOS_A * px.sin() * (px / LANCZOS_A).sin() / (px * px)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ramp(w: usize, h: usize, offset: f32) -> FrameF32 {
        let data: Vec<f32> = (0..w * h * 3).map(|i| i as f32 / 100.0 + offset).collect();
        FrameF32::from_planar(w, h, 3, data)
    }

    #[test]
    fn horizontal_stack_doubles_the_width() {
        let out = stack_horizontal(&FrameF32::new_rgb(4, 3), &FrameF32::new_rgb(4, 3));
        assert_eq!((out.width(), out.height(), out.channels()), (8, 3, 3));
    }

    #[test]
    fn vertical_stack_doubles_the_height() {
        let out = stack_vertical(&FrameF32::new_rgb(4, 3), &FrameF32::new_rgb(4, 3));
        assert_eq!((out.width(), out.height(), out.channels()), (4, 6, 3));
    }

    #[test]
    fn horizontal_stack_interleaves_rows_not_whole_images() {
        // Each output row must be a's row followed by b's row. Concatenating
        // the buffers instead would put all of a above all of b.
        let a = FrameF32::from_rgb_planes(2, 2, &[1.0, 2.0, 3.0, 4.0], &[0.0; 4], &[0.0; 4]);
        let b = FrameF32::from_rgb_planes(2, 2, &[5.0, 6.0, 7.0, 8.0], &[0.0; 4], &[0.0; 4]);
        let out = stack_horizontal(&a, &b);
        assert_eq!(out.plane(0), &[1.0, 2.0, 5.0, 6.0, 3.0, 4.0, 7.0, 8.0]);
    }

    #[test]
    fn vertical_stack_puts_the_first_frame_on_top() {
        let a = FrameF32::from_rgb_planes(2, 1, &[1.0, 2.0], &[0.0; 2], &[0.0; 2]);
        let b = FrameF32::from_rgb_planes(2, 1, &[3.0, 4.0], &[0.0; 2], &[0.0; 2]);
        let out = stack_vertical(&a, &b);
        assert_eq!(out.plane(0), &[1.0, 2.0, 3.0, 4.0]);
    }

    #[test]
    fn stacking_keeps_every_plane_separate() {
        let a = ramp(2, 2, 0.0);
        let b = ramp(2, 2, 10.0);
        let out = stack_horizontal(&a, &b);
        assert_eq!(out.plane_len(), 8);
        assert_eq!(
            out.plane(1)[0],
            a.plane(1)[0],
            "green plane must stay green"
        );
        assert_eq!(out.plane(2)[2], b.plane(2)[0], "blue plane must stay blue");
    }

    #[test]
    #[should_panic(expected = "same height")]
    fn horizontal_stack_rejects_mismatched_heights() {
        stack_horizontal(&FrameF32::new_rgb(4, 3), &FrameF32::new_rgb(4, 5));
    }

    #[test]
    #[should_panic(expected = "same width")]
    fn vertical_stack_rejects_mismatched_widths() {
        stack_vertical(&FrameF32::new_rgb(4, 3), &FrameF32::new_rgb(6, 3));
    }

    #[test]
    fn resizing_to_the_same_size_changes_nothing() {
        let src = ramp(5, 4, 0.0);
        let out = resize(&src, 5, 4);
        for (i, (&a, &b)) in out.as_slice().iter().zip(src.as_slice()).enumerate() {
            assert!((a - b).abs() < 1e-4, "sample {i}: {a} vs {b}");
        }
    }

    #[test]
    fn resizing_produces_the_requested_dimensions() {
        let out = resize(&FrameF32::new_rgb(16, 9), 40, 30);
        assert_eq!((out.width(), out.height(), out.channels()), (40, 30, 3));
    }

    #[test]
    fn a_flat_field_survives_both_directions() {
        // Weights that do not sum to one would show up here as a brightness
        // shift or as dark edges.
        let src = FrameF32::filled(20, 20, 3, 0.6);
        for (w, h) in [(7, 7), (60, 45), (20, 3)] {
            let out = resize(&src, w, h);
            for (i, &s) in out.as_slice().iter().enumerate() {
                assert!((s - 0.6).abs() < 1e-4, "{w}x{h} sample {i} drifted to {s}");
            }
        }
    }

    #[test]
    fn downscaling_averages_rather_than_dropping_pixels() {
        // A one-pixel-wide alternating pattern must average towards mid-grey,
        // not alias to whichever pixel happened to be sampled.
        let values: Vec<f32> = (0..32)
            .map(|i| if i % 2 == 0 { 0.0 } else { 1.0 })
            .collect();
        let mut data = values.clone();
        data.extend_from_slice(&values);
        data.extend_from_slice(&values);
        let src = FrameF32::from_planar(32, 1, 3, data);

        let out = resize(&src, 4, 1);
        for (i, &s) in out.plane(0).iter().enumerate() {
            assert!(
                (s - 0.5).abs() < 0.15,
                "sample {i} aliased to {s} instead of averaging to 0.5"
            );
        }
    }

    #[test]
    fn upscaling_interpolates_between_neighbours() {
        let src = FrameF32::from_planar(2, 1, 3, vec![0.0, 1.0, 0.0, 1.0, 0.0, 1.0]);
        let out = resize(&src, 8, 1);
        let r = out.plane(0);
        assert!(r[0] < r[3] && r[3] < r[7], "must ramp across: {r:?}");
    }

    #[test]
    fn resizing_a_grey_frame_keeps_it_grey() {
        let src = FrameF32::from_planar(4, 4, 1, vec![0.5; 16]);
        let out = resize(&src, 8, 8);
        assert_eq!(out.channels(), 1);
        assert_eq!(out.plane_len(), 64);
    }
}
