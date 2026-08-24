// Ana-Convert — anaglyph 3D to full-colour stereo recovery
// SPDX-License-Identifier: GPL-3.0-or-later

//! Anaglyph conversion with a live preview.
//!
//! The same conversion the command line runs, with the parameters attached to
//! sliders and the result on screen as you move them.

// The window is the product; a console behind it on Windows is not.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

/// Prints where the conversion tools were found. Returns a process exit code.
fn report_tools() -> i32 {
    match ana_media::locate(None) {
        Ok((tools, source)) => {
            println!("ffmpeg   {}", tools.ffmpeg.display());
            println!("ffprobe  {}", tools.ffprobe.display());
            println!("found as {source:?}");
            match std::process::Command::new(&tools.ffmpeg)
                .arg("-version")
                .output()
            {
                Ok(out) if out.status.success() => {
                    let first = String::from_utf8_lossy(&out.stdout);
                    println!("running  {}", first.lines().next().unwrap_or("?"));
                    0
                }
                Ok(out) => {
                    eprintln!(
                        "it will not run: {}",
                        String::from_utf8_lossy(&out.stderr).trim()
                    );
                    1
                }
                Err(e) => {
                    eprintln!("it will not run: {e}");
                    1
                }
            }
        }
        Err(e) => {
            eprintln!("{e}");
            1
        }
    }
}

fn main() -> eframe::Result<()> {
    let arg = std::env::args().nth(1);

    // `--check` reports which ffmpeg the app would use and exits. Worth having
    // for a bundled build, where "it does nothing" and "it cannot find its
    // tools" look identical from the outside.
    if arg.as_deref() == Some("--check") {
        std::process::exit(report_tools());
    }

    // A path on the command line opens straight away, which saves a trip
    // through the file dialog when returning to the same film.
    let open = arg.map(std::path::PathBuf::from);

    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("Stereoscopic Converter")
            // Ask for a tall window and let macOS clamp it to the display, so
            // the settings column arrives whole on a big screen and merely
            // scrolls on a small one.
            .with_inner_size([1380.0, 1200.0])
            .with_clamp_size_to_monitor_size(true)
            .with_min_inner_size([1000.0, 700.0]),
        ..Default::default()
    };
    eframe::run_native(
        "Stereoscopic Converter",
        options,
        Box::new(move |cc| Ok(Box::new(ana_app::app::AnaApp::new(cc, open)))),
    )
}
