// Ana-Convert — anaglyph 3D to full-colour stereo recovery
// SPDX-License-Identifier: GPL-3.0-or-later

//! Anaglyph conversion with a live preview.
//!
//! Exposed as a library as well as a binary so the parts that decide what the
//! preview shows can be tested without opening a window.

pub mod app;
pub mod preview;
pub mod render;
pub mod view;
