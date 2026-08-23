// Ana-Convert — anaglyph 3D to full-colour stereo recovery
// SPDX-License-Identifier: GPL-3.0-or-later

//! Separable Gaussian blur.
//!
//! Two stages need blur, for different reasons:
//!
//! * The colour reference is blurred hard horizontally and softly vertically.
//!   Anaglyph disparity is a horizontal shift, so colour sampled at a given
//!   pixel belongs somewhere nearby on the x axis; smearing it there is what
//!   makes colour restoration work at all.
//! * Either eye's luma can be blurred horizontally to suppress the white
//!   fringes that excessive edge peaking leaves on DVD-era transfers.
//!
//! The original script blurred by resizing down and back up, which is what
//! AviSynth made cheap. A real Gaussian avoids the ringing and aliasing that
//! bicubic decimation introduces.

use crate::frame::FrameF32;
use rayon::prelude::*;

/// Converts the original script's `decimate` percentage into a Gaussian sigma.
///
/// The percentage describes how far the colour reference was shrunk before
/// being scaled back up: 100 means no shrink, 5 means a twentieth. Existing
/// per-movie parameters keep roughly their old meaning under this mapping, and
/// 100% maps to exactly zero blur.
pub fn sigma_from_decimate(decimate_percent: f32) -> f32 {
    sigma_from_shrink(100.0 / decimate_percent.clamp(0.1, 100.0))
}

/// Converts a shrink factor into the sigma of a comparable Gaussian.
///
/// Both of the original's blur controls were expressed as "shrink by this much
/// and scale back up" — `decimate` as a percentage, `blurLeft`/`blurRight` as a
/// direct divisor — so both land here. A factor of 1 means no blur at all.
pub fn sigma_from_shrink(shrink_factor: f32) -> f32 {
    ((shrink_factor.max(1.0)) - 1.0) / 2.0
}

/// Blurs every plane with independent horizontal and vertical sigmas.
///
/// Edges are handled by clamping to the outermost pixel, so a flat field stays
/// flat right up to the border instead of darkening towards it.
pub fn gaussian_blur(frame: &FrameF32, sigma_x: f32, sigma_y: f32) -> FrameF32 {
    let (w, h) = (frame.width(), frame.height());
    let mut out = frame.clone();
    if w == 0 || h == 0 {
        return out;
    }

    let plan_x = BlurPlan::for_sigma(sigma_x);
    let plan_y = BlurPlan::for_sigma(sigma_y);
    let channels = frame.channels();
    let plane_len = w * h;

    out.as_mut_slice()
        .par_chunks_mut(plane_len)
        .take(channels)
        .for_each(|plane| {
            let mut scratch = vec![0.0f32; plane_len];
            plan_x.apply_rows(plane, &mut scratch, w, h);
            plan_y.apply_columns(plane, &mut scratch, w, h);
        });
    out
}

/// Above this sigma the exact convolution is abandoned for repeated box
/// passes. Below it the kernel is small enough that exactness is nearly free,
/// and a three-tap box is a poor stand-in for a narrow Gaussian.
const BOX_BLUR_THRESHOLD: f32 = 4.0;

/// How one axis will be blurred.
enum BlurPlan {
    /// Nothing to do.
    None,
    /// Direct convolution with an explicit kernel.
    Exact(Vec<f32>),
    /// Three box passes of the given radii, which converge on a Gaussian and
    /// cost the same per pixel no matter how wide they are. That flat cost is
    /// what keeps the preview responsive while a blur slider is being dragged.
    Boxes([usize; 3]),
}

impl BlurPlan {
    fn for_sigma(sigma: f32) -> Self {
        if sigma.is_nan() || sigma <= 1e-3 {
            Self::None
        } else if sigma <= BOX_BLUR_THRESHOLD {
            Self::Exact(gaussian_kernel(sigma))
        } else {
            Self::Boxes(box_radii_for(sigma))
        }
    }

    fn apply_rows(&self, plane: &mut [f32], scratch: &mut [f32], w: usize, h: usize) {
        match self {
            Self::None => {}
            Self::Exact(kernel) => convolve_rows(plane, scratch, w, h, kernel),
            Self::Boxes(radii) => {
                for &radius in radii {
                    box_rows(plane, scratch, w, h, radius);
                }
            }
        }
    }

    fn apply_columns(&self, plane: &mut [f32], scratch: &mut [f32], w: usize, h: usize) {
        match self {
            Self::None => {}
            Self::Exact(kernel) => convolve_columns(plane, scratch, w, h, kernel),
            Self::Boxes(radii) => {
                for &radius in radii {
                    box_columns(plane, scratch, w, h, radius);
                }
            }
        }
    }
}

/// Radii for three box passes whose combined variance matches `sigma`.
///
/// Three passes is the usual stopping point: the sum of three uniform
/// distributions is already close enough to a Gaussian that the difference does
/// not survive being looked at.
fn box_radii_for(sigma: f32) -> [usize; 3] {
    const PASSES: f32 = 3.0;
    // Total width whose variance over three passes equals sigma squared.
    let ideal = (12.0 * sigma * sigma / PASSES + 1.0).sqrt();
    let mut lower = ideal.floor() as i32;
    if lower % 2 == 0 {
        lower -= 1;
    }
    let lower = lower.max(1);
    let upper = lower + 2;

    // How many passes should use the narrower width to land closest to sigma.
    let l = lower as f32;
    let ideal_count = (12.0 * sigma * sigma - PASSES * l * l - 4.0 * PASSES * l - 3.0 * PASSES)
        / (-4.0 * l - 4.0);
    let narrow = ideal_count.round().clamp(0.0, PASSES) as usize;

    let mut radii = [0usize; 3];
    for (i, radius) in radii.iter_mut().enumerate() {
        let width = if i < narrow { lower } else { upper };
        *radius = ((width - 1) / 2).max(0) as usize;
    }
    radii
}

/// Builds a normalised 1-D Gaussian.
fn gaussian_kernel(sigma: f32) -> Vec<f32> {
    let radius = (sigma * 3.0).ceil() as usize;
    let two_sigma_sq = 2.0 * sigma * sigma;
    let mut kernel: Vec<f32> = (0..=2 * radius)
        .map(|i| {
            let x = i as f32 - radius as f32;
            (-x * x / two_sigma_sq).exp()
        })
        .collect();
    let sum: f32 = kernel.iter().sum();
    for w in &mut kernel {
        *w /= sum;
    }
    kernel
}

fn convolve_rows(plane: &mut [f32], scratch: &mut [f32], w: usize, h: usize, kernel: &[f32]) {
    scratch.copy_from_slice(plane);
    let radius = (kernel.len() / 2) as isize;
    for y in 0..h {
        let row = &scratch[y * w..(y + 1) * w];
        for x in 0..w {
            let mut acc = 0.0;
            for (i, &weight) in kernel.iter().enumerate() {
                let sx = clamp_index(x as isize + i as isize - radius, w);
                acc += weight * row[sx];
            }
            plane[y * w + x] = acc;
        }
    }
}

fn convolve_columns(plane: &mut [f32], scratch: &mut [f32], w: usize, h: usize, kernel: &[f32]) {
    scratch.copy_from_slice(plane);
    let radius = (kernel.len() / 2) as isize;
    for y in 0..h {
        for x in 0..w {
            let mut acc = 0.0;
            for (i, &weight) in kernel.iter().enumerate() {
                let sy = clamp_index(y as isize + i as isize - radius, h);
                acc += weight * scratch[sy * w + x];
            }
            plane[y * w + x] = acc;
        }
    }
}

/// One box pass across each row, using a running sum so the cost per pixel does
/// not grow with the radius.
fn box_rows(plane: &mut [f32], scratch: &mut [f32], w: usize, h: usize, radius: usize) {
    if radius == 0 {
        return;
    }
    scratch.copy_from_slice(plane);
    let r = radius as isize;
    let width = (2 * radius + 1) as f32;

    for y in 0..h {
        let row = &scratch[y * w..(y + 1) * w];
        let mut acc: f32 = (-r..=r).map(|k| row[clamp_index(k, w)]).sum();
        plane[y * w] = acc / width;
        for x in 1..w as isize {
            acc += row[clamp_index(x + r, w)] - row[clamp_index(x - r - 1, w)];
            plane[y * w + x as usize] = acc / width;
        }
    }
}

/// The same running sum down each column.
fn box_columns(plane: &mut [f32], scratch: &mut [f32], w: usize, h: usize, radius: usize) {
    if radius == 0 {
        return;
    }
    scratch.copy_from_slice(plane);
    let r = radius as isize;
    let height = (2 * radius + 1) as f32;

    for x in 0..w {
        let at = |y: isize| scratch[clamp_index(y, h) * w + x];
        let mut acc: f32 = (-r..=r).map(at).sum();
        plane[x] = acc / height;
        for y in 1..h as isize {
            acc += at(y + r) - at(y - r - 1);
            plane[y as usize * w + x] = acc / height;
        }
    }
}

/// Clamp-to-edge addressing: samples outside the frame repeat the border pixel.
fn clamp_index(index: isize, limit: usize) -> usize {
    index.clamp(0, limit as isize - 1) as usize
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(values: &[f32]) -> FrameF32 {
        FrameF32::from_planar(values.len(), 1, 1, values.to_vec())
    }

    fn column(values: &[f32]) -> FrameF32 {
        FrameF32::from_planar(1, values.len(), 1, values.to_vec())
    }

    /// Normalised f32 kernels sum to 1.0 only to within rounding, so identity
    /// cases land a few ulps away rather than exactly on the input.
    fn assert_planes_close(actual: &[f32], expected: &[f32]) {
        assert_eq!(actual.len(), expected.len(), "plane length");
        for (i, (&a, &e)) in actual.iter().zip(expected).enumerate() {
            assert!((a - e).abs() < 1e-5, "sample {i}: expected {e}, got {a}");
        }
    }

    #[test]
    fn full_decimate_means_no_blur() {
        assert_eq!(sigma_from_decimate(100.0), 0.0);
    }

    #[test]
    fn smaller_decimate_gives_more_blur() {
        let heavy = sigma_from_decimate(5.0);
        let light = sigma_from_decimate(50.0);
        assert!(
            heavy > light,
            "5% ({heavy}) must blur more than 50% ({light})"
        );
        assert!(light > 0.0, "50% must still blur somewhat");
    }

    #[test]
    fn decimate_sigma_matches_the_shrink_factor() {
        // A shrink to 1/20th resolution should behave like a radius of about
        // ten pixels, matching what the original script's users are used to.
        assert!(
            (sigma_from_decimate(5.0) - 9.5).abs() < 0.01,
            "got {}",
            sigma_from_decimate(5.0)
        );
    }

    #[test]
    fn zero_sigma_is_the_identity() {
        let src = row(&[0.0, 1.0, 0.0, 0.5, 0.25]);
        let out = gaussian_blur(&src, 0.0, 0.0);
        assert_eq!(out.plane(0), src.plane(0));
    }

    #[test]
    fn a_flat_field_stays_flat_including_at_the_edges() {
        // Weights summing to one plus edge clamping means no vignetting.
        let src = FrameF32::filled(8, 6, 1, 0.7);
        let out = gaussian_blur(&src, 4.0, 4.0);
        for (i, &s) in out.plane(0).iter().enumerate() {
            assert!((s - 0.7).abs() < 1e-5, "sample {i} drifted to {s}");
        }
    }

    #[test]
    fn a_delta_spreads_symmetrically_around_its_peak() {
        let src = row(&[0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0]);
        let out = gaussian_blur(&src, 1.0, 0.0);
        let p = out.plane(0);
        assert!(
            p[3] > p[2] && p[2] > p[1],
            "must decay away from the peak: {p:?}"
        );
        assert!((p[2] - p[4]).abs() < 1e-6, "symmetry: {p:?}");
        assert!((p[1] - p[5]).abs() < 1e-6, "symmetry: {p:?}");
    }

    #[test]
    fn horizontal_blur_leaves_vertical_structure_alone() {
        // A pattern that varies only down the column has nothing for a
        // horizontal blur to smear.
        let src = column(&[0.0, 1.0, 0.0, 1.0]);
        let out = gaussian_blur(&src, 5.0, 0.0);
        assert_planes_close(out.plane(0), src.plane(0));
    }

    #[test]
    fn vertical_blur_leaves_horizontal_structure_alone() {
        let src = row(&[0.0, 1.0, 0.0, 1.0]);
        let out = gaussian_blur(&src, 0.0, 5.0);
        assert_planes_close(out.plane(0), src.plane(0));
    }

    #[test]
    fn horizontal_blur_smooths_a_horizontal_pattern() {
        // Sampled away from the borders, where clamp-to-edge would otherwise
        // hold the outermost values in place.
        let src = row(&[0.0, 1.0].repeat(16));
        let out = gaussian_blur(&src, 3.0, 0.0);
        let interior = &out.plane(0)[10..22];
        let spread = interior.iter().cloned().fold(f32::MIN, f32::max)
            - interior.iter().cloned().fold(f32::MAX, f32::min);
        assert!(
            spread < 0.01,
            "heavy blur must flatten the pattern, spread {spread}"
        );
        assert!(
            (interior[0] - 0.5).abs() < 0.01,
            "and settle on the mean, got {}",
            interior[0]
        );
    }

    /// A deliberately naive, obviously correct Gaussian, used to hold the fast
    /// path honest. Too slow for production, exact enough to judge it by.
    fn reference_blur(src: &FrameF32, sigma: f32) -> FrameF32 {
        let (w, h) = (src.width(), src.height());
        let radius = (sigma * 4.0).ceil() as isize;
        let weights: Vec<f32> = (-radius..=radius)
            .map(|k| (-(k * k) as f32 / (2.0 * sigma * sigma)).exp())
            .collect();
        let total: f32 = weights.iter().sum();

        let mut out = src.clone();
        for c in 0..src.channels() {
            let plane = src.plane(c);
            let dst = out.plane_mut(c);
            for y in 0..h {
                for x in 0..w {
                    let mut acc = 0.0;
                    for (i, weight) in weights.iter().enumerate() {
                        let sx = (x as isize + i as isize - radius).clamp(0, w as isize - 1);
                        acc += weight * plane[y * w + sx as usize];
                    }
                    dst[y * w + x] = acc / total;
                }
            }
        }
        out
    }

    /// Effective sigma of a blur, measured from how far it spreads a delta.
    fn measured_sigma(sigma: f32) -> f32 {
        let n = 401usize;
        let mut values = vec![0.0f32; n];
        values[n / 2] = 1.0;
        let out = gaussian_blur(&FrameF32::from_planar(n, 1, 1, values), sigma, 0.0);
        let plane = out.plane(0);
        let mass: f32 = plane.iter().sum();
        let variance: f32 = plane
            .iter()
            .enumerate()
            .map(|(i, &w)| {
                let d = i as f32 - (n / 2) as f32;
                w * d * d
            })
            .sum::<f32>()
            / mass;
        variance.sqrt()
    }

    #[test]
    fn a_heavy_blur_spreads_by_the_sigma_it_was_asked_for() {
        // What matters for colour restoration is how far colour is smeared, so
        // that is what gets pinned. Above BOX_BLUR_THRESHOLD this exercises the
        // box approximation, whose width quantises to odd integers — accurate
        // to well under a percent by the time sigma reaches useful sizes.
        for target in [4.0f32, 6.0, 9.5, 20.0, 49.5] {
            let measured = measured_sigma(target);
            let error = (measured - target).abs() / target;
            assert!(
                error < 0.05,
                "sigma {target}: spread measured {measured:.2}, {:.1}% off",
                error * 100.0
            );
        }
    }

    #[test]
    fn blurring_never_changes_total_energy() {
        // A box pass that mishandled its running sum would show up here long
        // before it showed up as a visible artefact.
        let n = 401usize;
        let mut values = vec![0.0f32; n];
        values[n / 2] = 1.0;
        let src = FrameF32::from_planar(n, 1, 1, values);
        for sigma in [1.0f32, 4.0, 9.5, 30.0] {
            let mass: f32 = gaussian_blur(&src, sigma, 0.0).plane(0).iter().sum();
            assert!((mass - 1.0).abs() < 1e-3, "sigma {sigma} lost mass: {mass}");
        }
    }

    #[test]
    fn a_heavy_blur_stays_close_to_a_true_gaussian() {
        let values: Vec<f32> = (0..128)
            .map(|i| if (i / 7) % 2 == 0 { 0.15 } else { 0.85 })
            .collect();
        let src = FrameF32::from_planar(128, 1, 1, values);

        for sigma in [9.5, 20.0] {
            let fast = gaussian_blur(&src, sigma, 0.0);
            let reference = reference_blur(&src, sigma);
            // Compared away from the borders. Three clamped box passes
            // propagate edge values differently from one clamped convolution,
            // so they legitimately disagree within about a sigma of each edge
            // (~0.08 there, against ~0.005 across the interior). Both are valid
            // edge handling; the property that matters at a border is that a
            // flat field stays flat, which is asserted separately.
            let margin = (sigma * 1.5) as usize;
            let worst = fast.plane(0)[margin..128 - margin]
                .iter()
                .zip(&reference.plane(0)[margin..128 - margin])
                .map(|(a, b)| (a - b).abs())
                .fold(0.0f32, f32::max);
            assert!(
                worst < 0.02,
                "sigma {sigma}: worst interior deviation from a true Gaussian was {worst}"
            );
        }
    }

    #[test]
    fn a_heavy_blur_stays_symmetric_around_a_delta() {
        let mut values = vec![0.0f32; 81];
        values[40] = 1.0;
        let out = gaussian_blur(&FrameF32::from_planar(81, 1, 1, values), 8.0, 0.0);
        let p = out.plane(0);
        for offset in 1..=20 {
            let (lo, hi) = (p[40 - offset], p[40 + offset]);
            assert!(
                (lo - hi).abs() < 1e-6,
                "asymmetry at offset {offset}: {lo} vs {hi}"
            );
        }
        assert!(p[40] > p[45] && p[45] > p[55], "must decay outwards: {p:?}");
    }

    #[test]
    fn larger_sigma_blurs_more() {
        let src = row(&[0.0, 0.0, 1.0, 0.0, 0.0]);
        let light = gaussian_blur(&src, 0.6, 0.0);
        let heavy = gaussian_blur(&src, 2.5, 0.0);
        assert!(
            heavy.plane(0)[2] < light.plane(0)[2],
            "heavier blur must lower the peak: {} vs {}",
            heavy.plane(0)[2],
            light.plane(0)[2]
        );
    }

    #[test]
    fn rgb_planes_blur_independently() {
        let src =
            FrameF32::from_rgb_planes(3, 1, &[1.0, 0.0, 0.0], &[0.0, 0.0, 0.0], &[0.0, 0.0, 1.0]);
        let out = gaussian_blur(&src, 1.0, 0.0);
        let (r, g, b) = out.rgb_planes();
        assert!(r[0] > r[2], "red mass stays on the left: {r:?}");
        assert!(b[2] > b[0], "blue mass stays on the right: {b:?}");
        assert!(
            g.iter().all(|&s| s == 0.0),
            "empty plane stays empty: {g:?}"
        );
    }

    #[test]
    fn geometry_and_channel_count_are_preserved() {
        let src = FrameF32::new_rgb(9, 4);
        let out = gaussian_blur(&src, 2.0, 3.0);
        assert_eq!((out.width(), out.height(), out.channels()), (9, 4, 3));
    }

    #[test]
    fn blur_survives_a_single_pixel_frame() {
        let src = FrameF32::from_planar(1, 1, 1, vec![0.42]);
        let out = gaussian_blur(&src, 10.0, 10.0);
        assert!(
            (out.plane(0)[0] - 0.42).abs() < 1e-6,
            "got {}",
            out.plane(0)[0]
        );
    }
}
