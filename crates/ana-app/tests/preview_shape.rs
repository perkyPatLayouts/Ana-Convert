// Ana-Convert — anaglyph 3D to full-colour stereo recovery
// SPDX-License-Identifier: GPL-3.0-or-later

//! What the preview pane actually draws.
//!
//! Three separate aspect-ratio faults reached the user because the sizing
//! arithmetic was right and the *drawing* was wrong — a centring layout that
//! stretches its child to fill discards whatever size it is given, and nothing
//! in a screenshot-free suite could see that.
//!
//! These tests render the real widget headlessly and measure the rectangle it
//! paints, so the shape on screen is checked rather than the shape intended.

use ana_app::app::{fitted_size, paint_preview};
use egui::vec2;
use egui_kittest::Harness;

/// Renders the preview into a fixed pane and reports the rectangle used.
fn painted_shape(pane: egui::Vec2, pixel_height: f32, display_aspect: f32) -> egui::Rect {
    let painted = std::sync::Arc::new(std::sync::Mutex::new(egui::Rect::ZERO));
    let recorder = painted.clone();

    let mut harness = Harness::builder().with_size(pane).build_ui(move |ui| {
        let rect = paint_preview(ui, pane, pixel_height, display_aspect, None);
        *recorder.lock().unwrap() = rect;
    });
    harness.run();
    let rect = *painted.lock().unwrap();
    rect
}

#[test]
fn a_side_by_side_source_is_painted_at_its_display_shape() {
    // The file that exposed this: 1280x576 with 8:5 pixels, so the pair is
    // meant to be seen at 32:9. Painted at its pixel count it comes out
    // 2.22:1 and everyone in it is too narrow.
    let rect = painted_shape(vec2(600.0, 400.0), 576.0, 32.0 / 9.0);
    let aspect = rect.width() / rect.height();
    eprintln!(
        "painted {:.1}x{:.1} = {aspect:.4}:1",
        rect.width(),
        rect.height()
    );
    assert!(
        (aspect - 32.0 / 9.0).abs() < 0.02,
        "expected 3.556:1 on screen, got {aspect:.4}:1"
    );
}

#[test]
fn a_square_pixel_source_is_painted_at_its_pixel_shape() {
    let rect = painted_shape(vec2(600.0, 400.0), 576.0, 16.0 / 9.0);
    let aspect = rect.width() / rect.height();
    assert!((aspect - 16.0 / 9.0).abs() < 0.02, "got {aspect:.4}:1");
}

#[test]
fn the_painted_area_stays_inside_the_pane() {
    for pane in [vec2(600.0, 400.0), vec2(200.0, 800.0), vec2(1200.0, 120.0)] {
        for aspect in [0.5, 1.0, 1.7778, 3.5556, 7.1111] {
            let rect = painted_shape(pane, 576.0, aspect);
            assert!(
                rect.width() <= pane.x + 1.0 && rect.height() <= pane.y + 1.0,
                "{:?} escaped a {pane:?} pane at {aspect}",
                rect.size()
            );
        }
    }
}

#[test]
fn the_painted_shape_matches_what_the_sizing_says() {
    // The arithmetic and the drawing must not be able to disagree, which is
    // exactly how the bug survived: the sizing was right all along.
    let pane = vec2(600.0, 400.0);
    for aspect in [1.0, 1.7778, 3.5556] {
        let want = fitted_size(pane, 576.0, aspect);
        let got = painted_shape(pane, 576.0, aspect).size();
        assert!(
            (got.x - want.x).abs() < 1.0 && (got.y - want.y).abs() < 1.0,
            "painted {got:?} but sizing said {want:?} at {aspect}"
        );
    }
}
