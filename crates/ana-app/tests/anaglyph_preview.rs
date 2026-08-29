// Ana-Convert — anaglyph 3D to full-colour stereo recovery
// SPDX-License-Identifier: GPL-3.0-or-later

//! Previewing any of the four anaglyph encodings.
//!
//! The anaglyph view used to be drawn with whatever the *source* was set to,
//! so the only way to see a green/magenta encoding was to tell the app its
//! source was green/magenta — which changes the conversion. The picker added
//! beside the view buttons is a preview control and nothing else.
//!
//! That `compose_view` honours each of the four is `ana_core`'s business and
//! is tested there; what matters here is that the control exists where it
//! should and is not wired to the conversion.

use ana_app::app::AnaApp;
use ana_media::testing::make_test_clip;
use egui_kittest::kittest::Queryable;
use egui_kittest::Harness;

/// The preview picker's label.
const PICKER: &str = "Anaglyph mode";
/// The Source section's own colour-mode label, which must stay separate.
const SOURCE_MODE: &str = "Colour mode";

fn app_with_clip(dir: &std::path::Path) -> Harness<'static, AnaApp> {
    let (tools, _) = ana_media::locate(None).expect("ffmpeg must be installed");
    let clip = dir.join("clip.mkv");
    make_test_clip(&tools, &clip, 64, 48, 10, 10.0);

    let mut harness = Harness::builder()
        .with_size(egui::vec2(1380.0, 1440.0))
        .build_eframe(move |cc| AnaApp::new(cc, Some(clip.clone())));
    harness.run();
    harness.run();
    harness
}

#[test]
fn the_format_picker_belongs_to_the_anaglyph_view() {
    let dir = tempfile::tempdir().expect("temp dir");
    let mut harness = app_with_clip(dir.path());

    assert!(
        harness.query_by_label(PICKER).is_none(),
        "the picker is showing while a plain eye view is selected, where it \
         would mean nothing"
    );

    harness.get_by_label("Anaglyph").click();
    harness.run();
    assert!(
        harness.query_by_label(PICKER).is_some(),
        "choosing the Anaglyph view should offer the four encodings"
    );

    harness.get_by_label("Difference").click();
    harness.run();
    assert!(
        harness.query_by_label(PICKER).is_none(),
        "the picker outstayed the view it belongs to"
    );
}

#[test]
fn the_preview_picker_is_a_separate_control_from_the_source_setting() {
    // Both on screen at once is the point: what you are looking at and what
    // the file actually is are different questions, and answering one must
    // not answer the other.
    let dir = tempfile::tempdir().expect("temp dir");
    let mut harness = app_with_clip(dir.path());
    harness.get_by_label("Anaglyph").click();
    harness.run();

    assert!(
        harness.query_by_label(SOURCE_MODE).is_some(),
        "the source's own colour mode should still be there"
    );
    assert!(
        harness.query_by_label(PICKER).is_some(),
        "and the preview's should be beside it, not instead of it"
    );
}

#[test]
fn switching_to_the_anaglyph_view_keeps_the_preview_on_screen() {
    let dir = tempfile::tempdir().expect("temp dir");
    let mut harness = app_with_clip(dir.path());

    harness.get_by_label("Anaglyph").click();
    harness.run();

    assert!(
        harness
            .query_by_label("Open an anaglyph film to begin.")
            .is_none(),
        "the anaglyph view drew nothing"
    );
}
