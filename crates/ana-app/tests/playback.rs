// Ana-Convert — anaglyph 3D to full-colour stereo recovery
// SPDX-License-Identifier: GPL-3.0-or-later

//! Playing the preview, and the row the transport lives on.
//!
//! Playback reuses the preview's own decode path, which spends an ffmpeg launch
//! on every frame — around 80 ms at 1080p. So this plays as fast as it decodes
//! rather than at the film's rate, and these tests check that it advances and
//! stops, not that it hits any particular speed.

use ana_app::app::AnaApp;
use ana_media::testing::make_test_clip;
use egui_kittest::kittest::Queryable;
use egui_kittest::Harness;
use std::time::Duration;

const WINDOW: egui::Vec2 = egui::vec2(1380.0, 1440.0);

/// A harness with a short clip already open.
fn app_with_clip(dir: &std::path::Path) -> Harness<'static, AnaApp> {
    app_with_frames(dir, 30)
}

/// The same, with a chosen length.
fn app_with_frames(dir: &std::path::Path, frames: usize) -> Harness<'static, AnaApp> {
    let (tools, _) = ana_media::locate(None).expect("ffmpeg must be installed");
    let clip = dir.join("clip.mkv");
    make_test_clip(&tools, &clip, 64, 48, frames, 10.0);

    let mut harness = Harness::builder()
        .with_size(WINDOW)
        .build_eframe(move |cc| AnaApp::new(cc, Some(clip.clone())));
    harness.run();
    harness.run();
    harness
}

/// The frame the scrubber is showing.
fn current_frame(harness: &Harness<'static, AnaApp>) -> u64 {
    use egui_kittest::kittest::NodeT as _;
    harness
        .get_by_role_and_label(egui::accesskit::Role::Slider, "Frame")
        .accesskit_node()
        .numeric_value()
        .expect("the scrubber should report its frame") as u64
}

/// The fixture runs at 10 fps, so a frame is due every 100 ms.
const A_FRAME: Duration = Duration::from_millis(110);

/// Steps the harness as a running app would, letting real time pass so the
/// playback pacing has something to measure against. `run` is no use here:
/// playing asks for a repaint every pass, on purpose, and never settles.
fn play_for(harness: &mut Harness<'static, AnaApp>, frames: u32) {
    for _ in 0..frames {
        std::thread::sleep(A_FRAME);
        harness.step();
    }
}

#[test]
fn playing_advances_the_frame() {
    let dir = tempfile::tempdir().expect("temp dir");
    let mut harness = app_with_clip(dir.path());
    assert_eq!(
        current_frame(&harness),
        0,
        "should start at the first frame"
    );

    harness.get_by_label("Play").click();
    play_for(&mut harness, 4);

    // Several, not one: the point is that it keeps going, and a single step
    // would pass a `> 0` check while playback was actually stuck.
    assert!(
        current_frame(&harness) >= 3,
        "four frames' worth of playback moved the preview only to frame {}",
        current_frame(&harness)
    );
}

#[test]
fn pausing_leaves_the_frame_where_it_is() {
    let dir = tempfile::tempdir().expect("temp dir");
    let mut harness = app_with_clip(dir.path());

    harness.get_by_label("Play").click();
    play_for(&mut harness, 2);

    // The same control, now offering the opposite.
    harness.get_by_label("Pause").click();
    harness.step();
    let stopped_at = current_frame(&harness);

    play_for(&mut harness, 3);
    assert_eq!(
        current_frame(&harness),
        stopped_at,
        "the preview kept advancing after Pause"
    );
}

#[test]
fn playback_does_not_outrun_the_source() {
    // Decoding is the real limit at any sensible resolution, but a 64x48
    // fixture converts far faster than its own 10 fps. Without a ceiling the
    // preview would tear through the film as fast as the machine allows.
    let dir = tempfile::tempdir().expect("temp dir");
    let mut harness = app_with_clip(dir.path());

    harness.get_by_label("Play").click();
    for _ in 0..30 {
        harness.step();
    }

    // Generous, so a slow machine taking real time over those passes does not
    // fail it: without the ceiling this would be at frame 30.
    assert!(
        current_frame(&harness) <= 5,
        "30 passes in barely any time advanced to frame {} — playback is \
         running at the machine's rate, not the film's",
        current_frame(&harness)
    );
}

#[test]
fn playback_stops_at_the_end_rather_than_looping() {
    // This is for judging a shot, not watching the film, so running off the end
    // should leave the last frame up rather than snap back to the first.
    let dir = tempfile::tempdir().expect("temp dir");
    let mut harness = app_with_frames(dir.path(), 4);

    harness.get_by_label("Play").click();
    play_for(&mut harness, 8);

    assert_eq!(
        current_frame(&harness),
        3,
        "playback should have come to rest on the last frame"
    );
    assert!(
        harness.query_by_label("Play").is_some(),
        "the button still offers Pause, so playback thinks it is running"
    );
}

#[test]
fn the_scrubber_shares_the_play_button_s_row_and_fills_it() {
    let dir = tempfile::tempdir().expect("temp dir");
    let harness = app_with_clip(dir.path());

    let button = harness.get_by_label("Play").rect();
    let scrubber = harness
        .get_by_role_and_label(egui::accesskit::Role::Slider, "Frame")
        .rect();

    assert!(
        scrubber.y_range().contains(button.center().y),
        "the Play button at y={:.0} is not on the scrubber's row ({:.0}..{:.0})",
        button.center().y,
        scrubber.min.y,
        scrubber.max.y
    );
    assert!(
        button.max.x <= scrubber.min.x,
        "the Play button should come before the scrubber, not sit on top of it"
    );
    // The transport panel is the window less the settings column, so a scrubber
    // that fills its row is most of the window wide. A default-width egui
    // slider is 100pt and would fail this comfortably.
    assert!(
        scrubber.width() > WINDOW.x * 0.5,
        "the scrubber is only {:.0}pt wide in a {:.0}pt window — it is not \
         filling the rest of the row",
        scrubber.width(),
        WINDOW.x
    );
}
