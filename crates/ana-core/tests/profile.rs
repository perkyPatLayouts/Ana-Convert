// Ana-Convert — anaglyph 3D to full-colour stereo recovery
// SPDX-License-Identifier: GPL-3.0-or-later

//! Where the time goes at 1080p.
//!
//! Ignored by default; run with:
//! `cargo test --release -p ana-core --test profile -- --nocapture --ignored`
//!
//! Always measure in release. A debug build is roughly ten times slower here
//! and will send you optimising the wrong thing.

use std::time::Instant;

use ana_core::blur::gaussian_blur;
use ana_core::params::ConvertParams;
use ana_core::pipeline::{process_frame, Sources};
use ana_core::FrameF32;

fn time<T>(label: &str, f: impl Fn() -> T) {
    let runs = 5;
    let start = Instant::now();
    for _ in 0..runs {
        std::hint::black_box(f());
    }
    let per = start.elapsed().as_secs_f64() / f64::from(runs);
    println!(
        "{label:38} {:7.1} ms  ({:5.1} fps)",
        per * 1000.0,
        1.0 / per
    );
}

#[test]
#[ignore = "performance measurement; run with --ignored"]
fn profile_stages() {
    let (w, h) = (1920, 1080);
    let data: Vec<f32> = (0..w * h * 3).map(|i| ((i % 251) as f32) / 251.0).collect();
    let frame = FrameF32::from_planar(w, h, 3, data);
    let defaults = ConvertParams::default();

    println!(
        "colour sigmas: x={:.1} y={:.1}",
        defaults.colour_sigma_x(),
        defaults.colour_sigma_y()
    );

    time("full process_frame (defaults)", || {
        process_frame(Sources::from_anaglyph(&frame), &defaults)
    });
    time("full process_frame (no colour blur)", || {
        process_frame(
            Sources::from_anaglyph(&frame),
            &ConvertParams {
                decimate_horiz: 100.0,
                decimate_vert: 100.0,
                ..defaults.clone()
            },
        )
    });
    time("colour blur alone", || {
        gaussian_blur(&frame, defaults.colour_sigma_x(), defaults.colour_sigma_y())
    });
    time("  horizontal only", || {
        gaussian_blur(&frame, defaults.colour_sigma_x(), 0.0)
    });
    time("  vertical only", || {
        gaussian_blur(&frame, 0.0, defaults.colour_sigma_y())
    });
}
