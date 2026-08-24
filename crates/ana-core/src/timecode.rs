// Ana-Convert — anaglyph 3D to full-colour stereo recovery
// SPDX-License-Identifier: GPL-3.0-or-later

//! Moving between frame numbers and the times a person reads off a player.
//!
//! Alignment is a frame-accurate job, so frames are the unit everything is
//! stored and computed in. But nobody finds a cut by frame number — they find
//! it at "about nine minutes twelve" — so both have to be sayable.

/// Renders a frame number as `M:SS.hh`, or `H:MM:SS.hh` past an hour.
pub fn format_timecode(frame: u64, fps: f64) -> String {
    if !fps.is_finite() || fps <= 0.0 {
        // Some containers report no frame rate at all. Say so rather than
        // dividing by it and printing NaN at the user.
        return "?".to_string();
    }
    let hundredths = (frame as f64 / fps * 100.0).round() as u64;
    let (h, m, s, cs) = (
        hundredths / 360_000,
        hundredths / 6_000 % 60,
        hundredths / 100 % 60,
        hundredths % 100,
    );
    if h > 0 {
        format!("{h}:{m:02}:{s:02}.{cs:02}")
    } else {
        format!("{m}:{s:02}.{cs:02}")
    }
}

/// Parses `SS`, `M:SS` or `H:MM:SS`, each allowing a fractional seconds part,
/// into a frame number.
pub fn parse_timecode(text: &str, fps: f64) -> Result<u64, String> {
    if !fps.is_finite() || fps <= 0.0 {
        return Err("this file reports no frame rate, so times cannot be used".into());
    }
    let text = text.trim();
    if text.is_empty() {
        return Err("expected a time like 1:35 or 1:02:05".into());
    }
    if text.starts_with('-') {
        return Err(format!("{text:?} is before the start of the file"));
    }

    let parts: Vec<&str> = text.split(':').collect();
    let bad = || format!("{text:?} is not a time like 1:35 or 1:02:05");
    // Only the seconds field may carry a fraction; hours and minutes are whole.
    let whole = |p: &str| p.parse::<u64>().map_err(|_| bad());
    let secs = |p: &str| p.parse::<f64>().map_err(|_| bad());

    let seconds = match parts.as_slice() {
        [s] => secs(s)?,
        [m, s] => whole(m)? as f64 * 60.0 + secs(s)?,
        [h, m, s] => whole(h)? as f64 * 3600.0 + whole(m)? as f64 * 60.0 + secs(s)?,
        _ => return Err(bad()),
    };
    if !seconds.is_finite() || seconds < 0.0 {
        return Err(bad());
    }
    Ok((seconds * fps).round() as u64)
}

/// Parses a position given either as a time or as an explicit frame number.
///
/// A bare number is ambiguous — `120` could mean two minutes or frame 120 — so
/// frames are marked with a trailing `f`: `1:35` is a time, `900f` is a frame.
pub fn parse_position(text: &str, fps: f64) -> Result<u64, String> {
    let text = text.trim();
    match text.strip_suffix(['f', 'F']) {
        Some(frames) => frames
            .trim()
            .parse::<u64>()
            .map_err(|_| format!("{text:?} is not a frame number like 900f")),
        None => parse_timecode(text, fps),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn seconds_and_hundredths_are_shown() {
        assert_eq!(format_timecode(0, 24.0), "0:00.00");
        assert_eq!(format_timecode(12, 24.0), "0:00.50");
        assert_eq!(format_timecode(24, 24.0), "0:01.00");
    }

    #[test]
    fn minutes_appear_without_a_leading_hour() {
        assert_eq!(format_timecode(24 * 95, 24.0), "1:35.00");
    }

    #[test]
    fn hours_appear_once_there_are_any() {
        assert_eq!(format_timecode(24 * 3725, 24.0), "1:02:05.00");
    }

    #[test]
    fn a_broken_frame_rate_does_not_produce_nonsense() {
        // A container that reports no frame rate must not make the display
        // read "NaN" or panic.
        let shown = format_timecode(100, 0.0);
        assert!(
            !shown.contains("NaN") && !shown.contains("inf"),
            "got {shown}"
        );
    }

    #[test]
    fn bare_seconds_are_accepted() {
        assert_eq!(parse_timecode("5", 24.0), Ok(120));
        assert_eq!(parse_timecode("5.5", 24.0), Ok(132));
    }

    #[test]
    fn minutes_and_seconds_are_accepted() {
        assert_eq!(parse_timecode("1:35", 24.0), Ok(24 * 95));
    }

    #[test]
    fn hours_minutes_and_seconds_are_accepted() {
        assert_eq!(parse_timecode("1:02:05", 24.0), Ok(24 * 3725));
    }

    #[test]
    fn surrounding_space_is_ignored() {
        assert_eq!(parse_timecode("  1:35  ", 24.0), Ok(24 * 95));
    }

    #[test]
    fn a_fractional_frame_rate_round_trips() {
        // 23.976 and 29.97 are the two that actually turn up on discs.
        for fps in [23.976, 29.97] {
            for frame in [0u64, 1, 500, 18_164] {
                let text = format_timecode(frame, fps);
                let back = parse_timecode(&text, fps).expect("should parse what we printed");
                assert!(
                    back.abs_diff(frame) <= 1,
                    "{fps} fps frame {frame} printed {text} and came back {back}"
                );
            }
        }
    }

    #[test]
    fn nonsense_is_rejected_with_something_readable() {
        for bad in ["", "abc", "1:2:3:4", "1:xx"] {
            let err = parse_timecode(bad, 24.0).expect_err("{bad} should be rejected");
            assert!(!err.is_empty(), "{bad} produced an empty message");
        }
    }

    #[test]
    fn a_trailing_f_means_an_exact_frame() {
        assert_eq!(parse_position("900f", 24.0), Ok(900));
        assert_eq!(parse_position("0f", 24.0), Ok(0));
    }

    #[test]
    fn anything_else_is_read_as_a_time() {
        assert_eq!(parse_position("1:35", 24.0), Ok(24 * 95));
        assert_eq!(
            parse_position("5", 24.0),
            Ok(120),
            "a bare number is seconds"
        );
    }

    #[test]
    fn a_frame_position_ignores_the_frame_rate() {
        // The point of the suffix: frame 900 is frame 900 whatever the rate.
        assert_eq!(parse_position("900f", 24.0), parse_position("900f", 29.97));
    }

    #[test]
    fn a_malformed_frame_is_rejected() {
        assert!(parse_position("abcf", 24.0).is_err());
        assert!(parse_position("-4f", 24.0).is_err());
    }

    #[test]
    fn a_negative_time_is_rejected_rather_than_wrapping() {
        assert!(parse_timecode("-5", 24.0).is_err());
    }
}
