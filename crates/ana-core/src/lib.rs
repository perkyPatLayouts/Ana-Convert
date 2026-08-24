// Ana-Convert — anaglyph 3D to full-colour stereo recovery
// SPDX-License-Identifier: GPL-3.0-or-later

//! Pure image maths for recovering full-colour left and right views from
//! red/cyan, green/magenta and red/blue anaglyph frames.
//!
//! This crate performs no I/O and spawns no processes: every stage is a
//! function from frames and parameters to frames, so it can be tested without
//! ffmpeg or any media files present.
//!
//! The algorithm follows the AviSynth `AnaExtract.avs` scripts published at
//! <https://vrtifacts.com/dump-those-silly-colored-3d-glassess/>, reworked to
//! run in 32-bit float linear light with true Gaussian blur.

pub mod frame;
pub mod transfer;

pub use frame::FrameF32;
pub use transfer::TransferFunction;
pub mod blur;
pub mod compose;
pub mod extract;
pub mod grade;
pub mod leak;
pub mod packed;
pub mod params;
pub mod pipeline;
pub mod restore;
pub mod timecode;
