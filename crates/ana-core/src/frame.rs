// Ana-Convert — anaglyph 3D to full-colour stereo recovery
// SPDX-License-Identifier: GPL-3.0-or-later

//! Planar 32-bit float image buffers.
//!
//! Planar layout (all of plane 0, then all of plane 1, ...) keeps the separable
//! blur and the per-channel anaglyph extraction reading contiguous memory.
//! Sample values are nominally 0.0..=1.0 but are deliberately not clamped —
//! intermediate stages need headroom above 1.0 and below 0.0.

/// A planar float image: either single-channel grey or three-channel RGB.
#[derive(Clone, PartialEq)]
pub struct FrameF32 {
    width: usize,
    height: usize,
    channels: usize,
    data: Vec<f32>,
}

impl FrameF32 {
    /// Allocates a zeroed three-channel RGB frame.
    pub fn new_rgb(width: usize, height: usize) -> Self {
        Self::filled(width, height, 3, 0.0)
    }

    /// Allocates a zeroed single-channel grey frame.
    pub fn new_grey(width: usize, height: usize) -> Self {
        Self::filled(width, height, 1, 0.0)
    }

    /// Allocates a frame with every sample set to `value`.
    pub fn filled(width: usize, height: usize, channels: usize, value: f32) -> Self {
        assert!(
            channels == 1 || channels == 3,
            "frames are grey (1) or RGB (3), got {channels}"
        );
        Self {
            width,
            height,
            channels,
            data: vec![value; width * height * channels],
        }
    }

    /// Wraps existing planar samples.
    ///
    /// # Panics
    /// If `data.len()` is not `width * height * channels`.
    pub fn from_planar(width: usize, height: usize, channels: usize, data: Vec<f32>) -> Self {
        assert!(
            channels == 1 || channels == 3,
            "frames are grey (1) or RGB (3), got {channels}"
        );
        assert_eq!(
            data.len(),
            width * height * channels,
            "planar data length mismatch for {width}x{height}x{channels}"
        );
        Self {
            width,
            height,
            channels,
            data,
        }
    }

    pub fn width(&self) -> usize {
        self.width
    }

    pub fn height(&self) -> usize {
        self.height
    }

    pub fn channels(&self) -> usize {
        self.channels
    }

    /// Number of samples in one plane.
    pub fn plane_len(&self) -> usize {
        self.width * self.height
    }

    /// True when width or height is zero.
    pub fn is_empty(&self) -> bool {
        self.width == 0 || self.height == 0
    }

    /// Read-only view of one plane.
    pub fn plane(&self, index: usize) -> &[f32] {
        let len = self.plane_len();
        &self.data[index * len..(index + 1) * len]
    }

    /// Mutable view of one plane.
    pub fn plane_mut(&mut self, index: usize) -> &mut [f32] {
        let len = self.plane_len();
        &mut self.data[index * len..(index + 1) * len]
    }

    /// All planes as a flat planar slice.
    pub fn as_slice(&self) -> &[f32] {
        &self.data
    }

    /// All planes as a flat mutable planar slice.
    pub fn as_mut_slice(&mut self) -> &mut [f32] {
        &mut self.data
    }

    /// Whether the frame has the same geometry as `other`, ignoring channel count.
    pub fn same_size(&self, other: &Self) -> bool {
        self.width == other.width && self.height == other.height
    }

    /// Splits an RGB frame into its three planes.
    ///
    /// # Panics
    /// If the frame is not three-channel.
    pub fn rgb_planes(&self) -> (&[f32], &[f32], &[f32]) {
        assert_eq!(self.channels, 3, "rgb_planes requires an RGB frame");
        let len = self.plane_len();
        let (r, rest) = self.data.split_at(len);
        let (g, b) = rest.split_at(len);
        (r, g, b)
    }

    /// Builds an RGB frame from three equally sized planes.
    pub fn from_rgb_planes(width: usize, height: usize, r: &[f32], g: &[f32], b: &[f32]) -> Self {
        let len = width * height;
        assert!(
            r.len() == len && g.len() == len && b.len() == len,
            "plane lengths must all equal {len}"
        );
        let mut data = Vec::with_capacity(len * 3);
        data.extend_from_slice(r);
        data.extend_from_slice(g);
        data.extend_from_slice(b);
        Self::from_planar(width, height, 3, data)
    }

    /// Replicates a grey frame across all three RGB planes.
    pub fn grey_to_rgb(&self) -> Self {
        assert_eq!(self.channels, 1, "grey_to_rgb requires a grey frame");
        Self::from_rgb_planes(self.width, self.height, &self.data, &self.data, &self.data)
    }

    /// Decodes interleaved 8-bit RGB into planar floats scaled to 0.0..=1.0.
    pub fn from_rgb8(width: usize, height: usize, interleaved: &[u8]) -> Self {
        let len = width * height;
        assert_eq!(
            interleaved.len(),
            len * 3,
            "expected {} interleaved bytes for {width}x{height}",
            len * 3
        );
        let mut data = vec![0.0f32; len * 3];
        let (r, rest) = data.split_at_mut(len);
        let (g, b) = rest.split_at_mut(len);
        for (i, px) in interleaved.chunks_exact(3).enumerate() {
            r[i] = f32::from(px[0]) / 255.0;
            g[i] = f32::from(px[1]) / 255.0;
            b[i] = f32::from(px[2]) / 255.0;
        }
        Self::from_planar(width, height, 3, data)
    }

    /// Encodes to interleaved 8-bit RGB, clamping and rounding to nearest.
    pub fn to_rgb8(&self) -> Vec<u8> {
        assert_eq!(self.channels, 3, "to_rgb8 requires an RGB frame");
        let (r, g, b) = self.rgb_planes();
        let mut out = vec![0u8; self.plane_len() * 3];
        for i in 0..self.plane_len() {
            out[i * 3] = quantise(r[i], 255.0) as u8;
            out[i * 3 + 1] = quantise(g[i], 255.0) as u8;
            out[i * 3 + 2] = quantise(b[i], 255.0) as u8;
        }
        out
    }

    /// Decodes interleaved 16-bit RGB into planar floats scaled to 0.0..=1.0.
    pub fn from_rgb16(width: usize, height: usize, interleaved: &[u16]) -> Self {
        let len = width * height;
        assert_eq!(
            interleaved.len(),
            len * 3,
            "expected {} interleaved samples for {width}x{height}",
            len * 3
        );
        let mut data = vec![0.0f32; len * 3];
        let (r, rest) = data.split_at_mut(len);
        let (g, b) = rest.split_at_mut(len);
        for (i, px) in interleaved.chunks_exact(3).enumerate() {
            r[i] = f32::from(px[0]) / 65535.0;
            g[i] = f32::from(px[1]) / 65535.0;
            b[i] = f32::from(px[2]) / 65535.0;
        }
        Self::from_planar(width, height, 3, data)
    }

    /// Encodes to interleaved 16-bit RGB, clamping and rounding to nearest.
    pub fn to_rgb16(&self) -> Vec<u16> {
        assert_eq!(self.channels, 3, "to_rgb16 requires an RGB frame");
        let (r, g, b) = self.rgb_planes();
        let mut out = vec![0u16; self.plane_len() * 3];
        for i in 0..self.plane_len() {
            out[i * 3] = quantise(r[i], 65535.0) as u16;
            out[i * 3 + 1] = quantise(g[i], 65535.0) as u16;
            out[i * 3 + 2] = quantise(b[i], 65535.0) as u16;
        }
        out
    }
}

fn quantise(value: f32, scale: f32) -> u32 {
    // NaN maps to 0 via the clamp ordering below.
    let scaled = (value * scale).round();
    if scaled.is_nan() || scaled < 0.0 {
        0
    } else if scaled > scale {
        scale as u32
    } else {
        scaled as u32
    }
}

impl std::fmt::Debug for FrameF32 {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FrameF32")
            .field("width", &self.width)
            .field("height", &self.height)
            .field("channels", &self.channels)
            .finish_non_exhaustive()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_rgb_allocates_three_zeroed_planes() {
        let frame = FrameF32::new_rgb(4, 3);
        assert_eq!(frame.width(), 4);
        assert_eq!(frame.height(), 3);
        assert_eq!(frame.channels(), 3);
        assert_eq!(frame.plane_len(), 12);
        assert_eq!(frame.as_slice().len(), 36);
        assert!(frame.as_slice().iter().all(|&s| s == 0.0));
    }

    #[test]
    fn planes_are_contiguous_and_independently_addressable() {
        let mut frame = FrameF32::new_rgb(2, 2);
        frame.plane_mut(1)[3] = 0.5;
        assert_eq!(frame.plane(0), &[0.0, 0.0, 0.0, 0.0]);
        assert_eq!(frame.plane(1), &[0.0, 0.0, 0.0, 0.5]);
        assert_eq!(frame.plane(2), &[0.0, 0.0, 0.0, 0.0]);
    }

    #[test]
    fn rgb_planes_splits_without_copying_order() {
        let frame = FrameF32::from_rgb_planes(2, 1, &[1.0, 2.0], &[3.0, 4.0], &[5.0, 6.0]);
        let (r, g, b) = frame.rgb_planes();
        assert_eq!(r, &[1.0, 2.0]);
        assert_eq!(g, &[3.0, 4.0]);
        assert_eq!(b, &[5.0, 6.0]);
    }

    #[test]
    fn grey_to_rgb_replicates_the_single_plane() {
        let grey = FrameF32::from_planar(2, 1, 1, vec![0.25, 0.75]);
        let rgb = grey.grey_to_rgb();
        assert_eq!(rgb.channels(), 3);
        let (r, g, b) = rgb.rgb_planes();
        assert_eq!(r, &[0.25, 0.75]);
        assert_eq!(g, &[0.25, 0.75]);
        assert_eq!(b, &[0.25, 0.75]);
    }

    #[test]
    fn rgb8_round_trips_every_byte_value_exactly() {
        let interleaved: Vec<u8> = (0..=255u8).flat_map(|v| [v, v, v]).collect();
        let frame = FrameF32::from_rgb8(256, 1, &interleaved);
        assert_eq!(frame.to_rgb8(), interleaved);
    }

    #[test]
    fn from_rgb8_deinterleaves_channels() {
        let frame = FrameF32::from_rgb8(2, 1, &[255, 0, 0, 0, 255, 0]);
        let (r, g, b) = frame.rgb_planes();
        assert_eq!(r, &[1.0, 0.0]);
        assert_eq!(g, &[0.0, 1.0]);
        assert_eq!(b, &[0.0, 0.0]);
    }

    #[test]
    fn rgb16_round_trips_endpoint_values() {
        let interleaved: Vec<u16> = vec![0, 32768, 65535, 65535, 0, 12345];
        let frame = FrameF32::from_rgb16(2, 1, &interleaved);
        assert_eq!(frame.to_rgb16(), interleaved);
    }

    #[test]
    fn quantisation_clamps_out_of_range_samples() {
        let frame = FrameF32::from_rgb_planes(3, 1, &[-1.0, 0.5, 2.0], &[0.0; 3], &[0.0; 3]);
        let bytes = frame.to_rgb8();
        assert_eq!(bytes[0], 0, "negative clamps to 0");
        assert_eq!(bytes[3], 128, "0.5 rounds to nearest");
        assert_eq!(bytes[6], 255, "above 1.0 clamps to 255");
    }

    #[test]
    fn quantisation_maps_nan_to_zero() {
        let frame = FrameF32::from_rgb_planes(1, 1, &[f32::NAN], &[f32::NAN], &[f32::NAN]);
        assert_eq!(frame.to_rgb8(), vec![0, 0, 0]);
    }

    #[test]
    #[should_panic(expected = "planar data length mismatch")]
    fn from_planar_rejects_wrong_length() {
        FrameF32::from_planar(2, 2, 3, vec![0.0; 11]);
    }
}
