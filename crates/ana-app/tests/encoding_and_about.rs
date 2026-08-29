// Ana-Convert — anaglyph 3D to full-colour stereo recovery
// SPDX-License-Identifier: GPL-3.0-or-later

//! The encoding dialog and the About window.
//!
//! The encoder settings moved off the panel and into a dialog, which means the
//! panel no longer proves they exist. What each setting does to the ffmpeg
//! command line is `ana_media`'s business and is tested there; this checks the
//! controls are reachable and that the version the app claims is the one it
//! was built as.

use ana_app::app::{AnaApp, VERSION};
use egui_kittest::kittest::Queryable;
use egui_kittest::Harness;

/// Whether a slider with this label is on screen.
///
/// By role, because egui gives a slider and its caption the same name and a
/// plain label query cannot tell them apart.
fn has_slider(harness: &Harness<'static, AnaApp>, label: &str) -> bool {
    harness
        .query_by_role_and_label(egui::accesskit::Role::Slider, label)
        .is_some()
}

fn app() -> Harness<'static, AnaApp> {
    let mut harness = Harness::builder()
        .with_size(egui::vec2(1380.0, 1440.0))
        .build_eframe(|cc| AnaApp::new(cc, None));
    harness.run();
    harness
}

#[test]
fn the_encoding_dialog_opens_from_the_destination_panel() {
    let mut harness = app();
    assert!(
        harness.query_by_label("Codec").is_none(),
        "the encoder controls should be behind the dialog, not on the panel"
    );

    harness.get_by_label("Encoding settings…").click();
    harness.run();

    for control in [
        "Codec",
        "Fixed bitrate",
        "Set keyframe interval",
        "Pass audio through",
    ] {
        assert!(
            harness.query_by_label(control).is_some(),
            "{control} is missing from the encoding dialog"
        );
    }
}

#[test]
fn choosing_a_fixed_bitrate_replaces_the_quality_slider() {
    // They are two ways of asking for the same thing, so showing both would
    // invite setting both and wondering which won.
    let mut harness = app();
    harness.get_by_label("Encoding settings…").click();
    harness.run();

    assert!(has_slider(&harness, "Quality"));
    assert!(!has_slider(&harness, "kbit/s"));

    harness.get_by_label("Fixed bitrate").click();
    harness.run();

    assert!(
        has_slider(&harness, "kbit/s"),
        "asking for a bitrate should offer somewhere to put it"
    );
    assert!(
        !has_slider(&harness, "Quality"),
        "the quality slider stayed alongside the bitrate"
    );
}

#[test]
fn the_keyframe_interval_is_opt_in() {
    let mut harness = app();
    harness.get_by_label("Encoding settings…").click();
    harness.run();

    assert!(
        !has_slider(&harness, "Frames"),
        "an interval is offered before it has been asked for, so the encoder's \
         own choice is no longer the default"
    );

    harness.get_by_label("Set keyframe interval").click();
    harness.run();
    assert!(has_slider(&harness, "Frames"));
}

#[test]
fn about_states_the_version_the_app_was_built_as() {
    let mut harness = app();
    harness.get_by_label("About").click();
    harness.run();

    let expected = format!("Version {VERSION} Beta");
    assert!(
        harness.query_by_label(&expected).is_some(),
        "About should say {expected:?}"
    );
    assert!(
        harness.query_by_label("Stereoscopic Converter").is_some(),
        "About should name the app"
    );
}

#[test]
fn the_version_is_the_one_in_the_manifest() {
    // About, the disk image name and the Homebrew cask all read from here, so
    // a hand-written version string in the window would be the one that drifts.
    assert_eq!(VERSION, env!("CARGO_PKG_VERSION"));
    assert!(
        VERSION.starts_with("0.10."),
        "expected the 0.10 series, got {VERSION}"
    );
}
