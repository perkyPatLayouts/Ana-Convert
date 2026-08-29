// Ana-Convert — anaglyph 3D to full-colour stereo recovery
// SPDX-License-Identifier: GPL-3.0-or-later

//! The preview must survive a change of view.
//!
//! Switching view drops the texture, because the texture is built from the
//! view as well as from the converted pair. Nothing rebuilt it: the refresh
//! is guarded by a cache that only knows about the frame and the conversion
//! settings, and a view is neither. So the pane fell back to its empty state
//! and told someone with a film open to open a film, until a scrub happened
//! to force a decode and the picture came back.

use ana_app::app::AnaApp;
use ana_media::testing::make_test_clip;
use egui_kittest::kittest::Queryable;
use egui_kittest::Harness;

/// What the pane says when it has nothing to draw.
const EMPTY_STATE: &str = "Open an anaglyph film to begin.";

#[test]
fn the_preview_stays_on_screen_when_the_view_changes() {
    let (tools, _) = ana_media::locate(None).expect("ffmpeg must be installed");
    let dir = tempfile::tempdir().expect("temp dir");
    let clip = dir.path().join("clip.mkv");
    make_test_clip(&tools, &clip, 64, 48, 10, 10.0);

    let mut harness = Harness::builder()
        .with_size(egui::vec2(1380.0, 1440.0))
        .build_eframe(move |cc| AnaApp::new(cc, Some(clip.clone())));
    // Decode, convert, upload.
    harness.run();
    harness.run();

    assert!(
        harness.query_by_label(EMPTY_STATE).is_none(),
        "the film should already be previewing before the view is touched"
    );

    // Two switches, and one of them back off a mode again: the texture has to
    // be rebuilt every time the view moves, not just the first time.
    for view in ["Difference", "Anaglyph", "Difference"] {
        harness.get_by_label(view).click();
        harness.run();

        assert!(
            harness.query_by_label(EMPTY_STATE).is_none(),
            "the preview went blank on switching to {view}, and told someone \
             with a film open to open a film"
        );
    }
}
