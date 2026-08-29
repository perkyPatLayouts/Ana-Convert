// Ana-Convert — anaglyph 3D to full-colour stereo recovery
// SPDX-License-Identifier: GPL-3.0-or-later

//! Whether the settings column arrives whole.
//!
//! The window asked for a height that was simply less than the column needs, so
//! the Destination section — codec, quality, the output file — opened below the
//! bottom edge. It scrolled, so nothing was unreachable, but the only hint that
//! there was anything down there was a floating scrollbar two pixels wide that
//! fades out when the pointer is elsewhere. On a 4K screen with a thousand
//! points to spare, the app looked like it had no Destination section.
//!
//! Clamping to the monitor only ever shrinks the window, so asking for too
//! little cannot be corrected later.

use ana_app::app::{AnaApp, MINIMUM_WINDOW, STARTUP_WINDOW};
use egui_kittest::kittest::Queryable;
use egui_kittest::Harness;

/// The last control in the settings column, at the bottom of Destination.
const LAST_CONTROL: &str = "Reset all settings";

fn app_at(size: egui::Vec2) -> Harness<'static, AnaApp> {
    let mut harness = Harness::builder()
        .with_size(size)
        .build_eframe(|cc| AnaApp::new(cc, None));
    // Twice: the first pass lays the column out, the second settles the scroll
    // state now that the content height is known.
    harness.run();
    harness.run();
    harness
}

#[test]
fn every_setting_is_visible_in_the_window_the_app_opens_at() {
    let harness = app_at(STARTUP_WINDOW);

    let rect = harness
        .query_by_label(LAST_CONTROL)
        .expect("the settings column should end with the reset button")
        .rect();

    assert!(
        rect.max.y <= STARTUP_WINDOW.y,
        "the column ends at y={:.0} but the window opens {:.0} tall, so the \
         Destination section starts below the bottom edge. The startup height \
         has to cover the column, not the other way round.",
        rect.max.y,
        STARTUP_WINDOW.y
    );
}

#[test]
fn the_column_still_scrolls_when_the_window_is_too_short_for_it() {
    // Every screen the window gets clamped to is a screen the column may not
    // fit on, so being cut off must always mean scrollable, never truncated.
    let harness = app_at(MINIMUM_WINDOW);

    assert!(
        harness.query_by_label(LAST_CONTROL).is_some(),
        "the last setting vanished from the layout on a short window rather \
         than sitting below the fold where scrolling can reach it"
    );
}
