// Ana-Convert — anaglyph 3D to full-colour stereo recovery
// SPDX-License-Identifier: GPL-3.0-or-later

//! Headless anaglyph conversion.
//!
//! This is the whole conversion without a window: enough to convert a film
//! overnight, and the thing the GUI will drive underneath. Presets written here
//! and presets written by the app are the same JSON, so a look tuned in one can
//! be rendered by the other.

use std::io::Write;
use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Instant;

use ana_core::compose::{EyeOrder, OutputLayout};
use ana_core::extract::AnaglyphFormat;
use ana_core::packed::StereoPacking;
use ana_core::params::{ConvertParams, InputMode, MonoEye};
use ana_core::timecode::parse_position;
use ana_media::encode::{EncodeSettings, VideoCodec};
use ana_pipeline::{render, Progress, RenderJob};
use clap::{Args, Parser, Subcommand, ValueEnum};

#[derive(Parser)]
#[command(
    name = "ana-convert",
    about = "Convert anaglyph 3D video into full-colour stereo",
    version
)]
struct Cli {
    /// Directory holding ffmpeg and ffprobe, if not the bundled or system ones.
    #[arg(long, global = true, value_name = "DIR")]
    ffmpeg_dir: Option<PathBuf>,

    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Convert a file.
    // Boxed only to keep the enum small; RenderArgs dwarfs the other variants.
    Render(Box<RenderArgs>),
    /// Report what a file contains.
    Probe {
        /// The file to inspect.
        input: PathBuf,
    },
    /// Write a preset of the default settings, as a starting point to edit.
    Preset {
        /// Where to write it.
        output: PathBuf,
    },
}

#[derive(Args)]
struct RenderArgs {
    /// The anaglyph video.
    #[arg(short, long)]
    input: PathBuf,

    /// Where to write the result. With `--layout separate` this is the stem
    /// that `-left` and `-right` files are derived from.
    #[arg(short, long)]
    out: PathBuf,

    /// The right eye, when --source two-files is used.
    #[arg(long, value_name = "FILE")]
    right_eye: Option<PathBuf>,

    /// A 2D release to take colour from. Defaults to the anaglyph itself,
    /// which works but leaves its colour cast in the result.
    #[arg(long, value_name = "FILE")]
    colour: Option<PathBuf>,

    /// A 2D release to use verbatim as one eye. Use --mono-start to line it up
    /// with the anaglyph.
    #[arg(long, value_name = "FILE")]
    mono: Option<PathBuf>,

    /// Which eye the 2D release supplies.
    #[arg(long, value_enum, default_value_t = MonoEyeArg::None)]
    mono_eye: MonoEyeArg,

    /// Where to start in the anaglyph. A time like 1:35, or a frame like 900f.
    #[arg(long, value_name = "POS")]
    start: Option<String>,

    /// Where to finish in the anaglyph, inclusive.
    #[arg(long, value_name = "POS")]
    end: Option<String>,

    /// Where the same moment begins in the colour source.
    #[arg(long, value_name = "POS")]
    colour_start: Option<String>,

    /// Where to finish in the colour source, inclusive.
    #[arg(long, value_name = "POS")]
    colour_end: Option<String>,

    /// Where the same moment begins in the 2D eye source. This is how two
    /// differently edited releases are brought onto the same frame.
    #[arg(long, value_name = "POS")]
    mono_start: Option<String>,

    /// Where to finish in the 2D eye source, inclusive.
    #[arg(long, value_name = "POS")]
    mono_end: Option<String>,

    /// Where to copy the audio track from. Defaults to the anaglyph.
    #[arg(long, value_name = "FILE")]
    audio: Option<PathBuf>,

    /// Load settings from a preset, before any flags below are applied.
    #[arg(long, value_name = "FILE")]
    preset: Option<PathBuf>,

    /// Write the settings actually used back out, so a good result can be
    /// reproduced or handed to the app.
    #[arg(long, value_name = "FILE")]
    save_preset: Option<PathBuf>,

    /// What the input file holds. `anaglyph` recovers a stereo pair from it;
    /// `sbs` or `tb` take apart a pair that is already packed into each frame.
    #[arg(long, value_enum, default_value_t = SourceArg::Anaglyph)]
    source: SourceArg,

    /// The packed source squeezes each eye to half size, so stretch it back.
    /// Usual for broadcast and disc stereo; wrong for full-resolution packing.
    #[arg(long)]
    anamorphic: bool,

    /// Which anaglyph encoding the source uses.
    #[arg(long, value_enum)]
    format: Option<FormatArg>,

    /// The encoding written when --layout anaglyph is chosen. Independent of
    /// the source's, so a red/cyan transfer can be written as green/magenta.
    #[arg(long, value_enum)]
    output_format: Option<FormatArg>,

    /// How the two eyes are packed.
    #[arg(long, value_enum)]
    layout: Option<LayoutArg>,

    /// Which eye comes first in a stacked layout.
    #[arg(long, value_enum)]
    eye_order: Option<EyeOrderArg>,

    /// Swap the eyes before layout.
    #[arg(long)]
    swap_eyes: bool,

    /// Horizontal colour blur, as the original's shrink percentage. Lower
    /// blurs harder.
    #[arg(long, value_name = "PERCENT")]
    decimate_horiz: Option<f32>,

    /// Vertical colour blur.
    #[arg(long, value_name = "PERCENT")]
    decimate_vert: Option<f32>,

    /// Percentage of the right eye to remove from the left.
    #[arg(long, value_name = "PERCENT", allow_negative_numbers = true)]
    leak_left: Option<f32>,

    /// Percentage of the left eye to remove from the right.
    #[arg(long, value_name = "PERCENT", allow_negative_numbers = true)]
    leak_right: Option<f32>,

    /// Video encoder.
    #[arg(long, value_enum, default_value_t = CodecArg::H264Hw)]
    codec: CodecArg,

    /// Perceptual quality, 0 to 100.
    #[arg(long, default_value_t = 75)]
    quality: u8,

    /// Resize the finished frame, as WxH.
    #[arg(long, value_name = "WxH")]
    size: Option<String>,
}

#[derive(Copy, Clone, PartialEq, Eq, ValueEnum)]
enum FormatArg {
    /// Red left, cyan right. The most common release format.
    RedCyan,
    /// Green left, magenta right.
    GreenMagenta,
    /// Red left, blue right.
    RedBlue,
}

#[derive(Copy, Clone, PartialEq, Eq, ValueEnum)]
enum LayoutArg {
    /// One frame, twice as wide.
    Sbs,
    /// One frame, twice as tall.
    Tb,
    /// Two files, one per eye.
    Separate,
    /// Muxed into an anaglyph, for the old glasses.
    Anaglyph,
    /// The left eye alone, as a flat 2D file.
    Left,
    /// The right eye alone.
    Right,
}

#[derive(Copy, Clone, PartialEq, Eq, ValueEnum)]
enum SourceArg {
    /// A red/cyan, green/magenta or red/blue anaglyph.
    Anaglyph,
    /// A stereo pair packed side by side in each frame.
    Sbs,
    /// A stereo pair packed top and bottom.
    Tb,
    /// This file is one eye; --right-eye holds the other.
    TwoFiles,
}

#[derive(Copy, Clone, PartialEq, Eq, ValueEnum)]
enum EyeOrderArg {
    LeftFirst,
    RightFirst,
}

#[derive(Copy, Clone, PartialEq, Eq, ValueEnum)]
enum MonoEyeArg {
    None,
    Left,
    Right,
}

#[derive(Copy, Clone, PartialEq, Eq, ValueEnum)]
enum CodecArg {
    /// Hardware H.264. Fast on Apple Silicon.
    H264Hw,
    /// Hardware HEVC.
    HevcHw,
    /// Software H.264. Slower, identical on every platform.
    H264,
    /// Software HEVC.
    Hevc,
    /// ProRes HQ, for keeping a master.
    Prores,
}

impl From<FormatArg> for AnaglyphFormat {
    fn from(a: FormatArg) -> Self {
        match a {
            FormatArg::RedCyan => Self::RedCyan,
            FormatArg::GreenMagenta => Self::GreenMagenta,
            FormatArg::RedBlue => Self::RedBlue,
        }
    }
}

impl From<LayoutArg> for OutputLayout {
    fn from(a: LayoutArg) -> Self {
        match a {
            LayoutArg::Sbs => Self::SideBySide,
            LayoutArg::Tb => Self::TopBottom,
            LayoutArg::Separate => Self::Separate,
            LayoutArg::Anaglyph => Self::Anaglyph,
            LayoutArg::Left => Self::LeftOnly,
            LayoutArg::Right => Self::RightOnly,
        }
    }
}

impl From<EyeOrderArg> for EyeOrder {
    fn from(a: EyeOrderArg) -> Self {
        match a {
            EyeOrderArg::LeftFirst => Self::LeftFirst,
            EyeOrderArg::RightFirst => Self::RightFirst,
        }
    }
}

impl From<MonoEyeArg> for MonoEye {
    fn from(a: MonoEyeArg) -> Self {
        match a {
            MonoEyeArg::None => Self::None,
            MonoEyeArg::Left => Self::Left,
            MonoEyeArg::Right => Self::Right,
        }
    }
}

impl From<CodecArg> for VideoCodec {
    fn from(a: CodecArg) -> Self {
        match a {
            CodecArg::H264Hw => Self::H264VideoToolbox,
            CodecArg::HevcHw => Self::HevcVideoToolbox,
            CodecArg::H264 => Self::H264,
            CodecArg::Hevc => Self::Hevc,
            CodecArg::Prores => Self::ProRes,
        }
    }
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    match run(&cli) {
        Ok(code) => code,
        Err(message) => {
            eprintln!("error: {message}");
            ExitCode::FAILURE
        }
    }
}

fn run(cli: &Cli) -> Result<ExitCode, String> {
    let (tools, source) =
        ana_media::locate(cli.ffmpeg_dir.as_deref()).map_err(|e| e.to_string())?;

    match &cli.command {
        Command::Probe { input } => {
            let info = ana_media::probe(&tools, input).map_err(|e| e.to_string())?;
            println!("{}", input.display());
            println!("  size        {}x{}", info.width, info.height);
            println!("  frame rate  {:.3} fps", info.fps);
            println!(
                "  frames      {}",
                info.estimated_frame_count()
                    .map_or_else(|| "unknown".into(), |n| n.to_string())
            );
            println!("  pixels      {} ({}-bit)", info.pix_fmt, info.bit_depth);
            println!(
                "  pixel shape {:.5} ({})",
                info.sample_aspect,
                if (info.sample_aspect - 1.0).abs() < 1e-6 {
                    "square"
                } else {
                    "not square"
                }
            );
            println!(
                "  displays at {:.4}:1  ({}x{} of square pixels)",
                info.display_aspect(),
                (info.width as f64 * info.sample_aspect).round() as i64,
                info.height
            );
            // What a conversion would write, which is the number that matters
            // when a result looks the wrong shape.
            for layout in [
                OutputLayout::SideBySide,
                OutputLayout::TopBottom,
                OutputLayout::LeftOnly,
            ] {
                let p = ConvertParams {
                    layout,
                    ..Default::default()
                };
                let (w, h) = p.output_geometry((info.width, info.height));
                println!(
                    "    as {:<14} {w}x{h} displaying {:.4}:1",
                    layout.label(),
                    p.output_display_aspect(info.display_aspect())
                );
            }
            println!(
                "  audio       {}",
                if info.has_audio { "yes" } else { "no" }
            );
            if let Some(frames) = info.estimated_frame_count() {
                // Handy when picking trim points: --start takes this format.
                println!(
                    "  length      {}",
                    ana_core::timecode::format_timecode(frames, info.fps)
                );
            }
            Ok(ExitCode::SUCCESS)
        }

        Command::Preset { output } => {
            let json = serde_json::to_string_pretty(&ConvertParams::default())
                .map_err(|e| e.to_string())?;
            std::fs::write(output, json + "\n").map_err(|e| e.to_string())?;
            println!("wrote default preset to {}", output.display());
            Ok(ExitCode::SUCCESS)
        }

        Command::Render(args) => {
            eprintln!("using ffmpeg from {} ({source:?})", tools.ffmpeg.display());
            do_render(&tools, args)
        }
    }
}

fn do_render(tools: &ana_media::FfmpegTools, args: &RenderArgs) -> Result<ExitCode, String> {
    // Match the source frame rate rather than guessing, or the audio drifts —
    // and positions given as times need it to become frame numbers.
    let info = ana_media::probe(tools, &args.input).map_err(|e| e.to_string())?;
    let fps = if info.fps > 0.0 { info.fps } else { 24.0 };
    let params = build_params(args, fps)?;

    if let Some(path) = &args.save_preset {
        let json = serde_json::to_string_pretty(&params).map_err(|e| e.to_string())?;
        std::fs::write(path, json + "\n").map_err(|e| e.to_string())?;
        eprintln!("saved preset to {}", path.display());
    }

    // Default the audio to the anaglyph, which is what almost everyone wants —
    // but only when it actually has a track. Plenty of anaglyph rips are silent,
    // and defaulting blindly would make every one of them fail.
    let audio = match &args.audio {
        Some(path) => Some(path.clone()),
        None if info.has_audio => Some(args.input.clone()),
        None => None,
    };

    // A 2D release that is one of the eyes is also the best colour reference
    // for the other, so naming it once is enough.
    let colour = args.colour.clone().or_else(|| args.mono.clone());
    let job = RenderJob {
        anaglyph: args.input.clone(),
        right_eye: args.right_eye.clone(),
        colour,
        mono: args.mono.clone(),
        audio,
        output: args.out.clone(),
        params,
        encode: EncodeSettings {
            codec: args.codec.into(),
            quality: args.quality,
            fps: if info.fps > 0.0 { info.fps } else { 24.0 },
            audio_from: None,
            // The pipeline recomputes this from the source; set here only so
            // the struct is complete.
            display_aspect: None,
        },
    };

    let cancel = Arc::new(AtomicBool::new(false));
    {
        let cancel = Arc::clone(&cancel);
        // Second interrupt aborts outright, in case finishing the current
        // frame is somehow not prompt enough.
        let _ = ctrlc::set_handler(move || {
            if cancel.swap(true, Ordering::SeqCst) {
                eprintln!("\ninterrupted twice, exiting now");
                std::process::exit(130);
            }
            eprintln!("\nstopping after this frame...");
        });
    }

    let started = Instant::now();
    let mut reporter = ProgressReporter::new(started);
    let summary =
        render(tools, &job, &mut |p| reporter.report(p), &cancel).map_err(|e| e.to_string())?;

    let elapsed = started.elapsed().as_secs_f64();
    eprintln!(
        "\n{} {} frames in {:.1}s ({:.1} fps)",
        if summary.cancelled {
            "stopped after"
        } else {
            "converted"
        },
        summary.frames,
        elapsed,
        summary.frames as f64 / elapsed.max(1e-9)
    );
    for path in &summary.outputs {
        eprintln!("  {}", path.display());
    }

    // A cancelled run leaves a partial file, so it must not look like success
    // to a script that only checks the exit code.
    Ok(if summary.cancelled {
        ExitCode::from(130)
    } else {
        ExitCode::SUCCESS
    })
}

/// Preset first, then explicit flags on top.
fn build_params(args: &RenderArgs, fps: f64) -> Result<ConvertParams, String> {
    let mut params = match &args.preset {
        Some(path) => {
            let text = std::fs::read_to_string(path)
                .map_err(|e| format!("could not read {}: {e}", path.display()))?;
            serde_json::from_str(&text)
                .map_err(|e| format!("could not parse {}: {e}", path.display()))?
        }
        None => ConvertParams::default(),
    };

    params.input = match args.source {
        SourceArg::Anaglyph => InputMode::Anaglyph,
        SourceArg::Sbs => InputMode::packed(StereoPacking::SideBySide, args.anamorphic),
        SourceArg::Tb => InputMode::packed(StereoPacking::TopBottom, args.anamorphic),
        SourceArg::TwoFiles => InputMode::TwoFiles,
    };
    if let Some(v) = args.format {
        params.input_format = v.into();
    }
    if let Some(v) = args.output_format {
        params.output_format = v.into();
    }
    if let Some(v) = args.layout {
        params.layout = v.into();
    }
    if let Some(v) = args.eye_order {
        params.eye_order = v.into();
    }
    if args.swap_eyes {
        params.swap_eyes = true;
    }
    if let Some(v) = args.decimate_horiz {
        params.decimate_horiz = v;
    }
    if let Some(v) = args.decimate_vert {
        params.decimate_vert = v;
    }
    if let Some(v) = args.leak_left {
        params.leak_correct_left = v;
    }
    if let Some(v) = args.leak_right {
        params.leak_correct_right = v;
    }
    if args.mono_eye != MonoEyeArg::None {
        params.mono_eye = args.mono_eye.into();
    }

    for (trim, start, end, which) in [
        (&mut params.anaglyph_trim, &args.start, &args.end, "--start"),
        (
            &mut params.colour_trim,
            &args.colour_start,
            &args.colour_end,
            "--colour-start",
        ),
        (
            &mut params.mono_trim,
            &args.mono_start,
            &args.mono_end,
            "--mono-start",
        ),
    ] {
        if let Some(text) = start {
            trim.start = parse_position(text, fps).map_err(|e| format!("{which}: {e}"))?;
        }
        if let Some(text) = end {
            trim.end = Some(parse_position(text, fps).map_err(|e| format!("{which}: {e}"))?);
        }
    }
    if let Some(size) = &args.size {
        params.output_size = Some(parse_size(size)?);
    }

    params.validate().map_err(|e| e.to_string())?;
    Ok(params)
}

fn parse_size(text: &str) -> Result<(usize, usize), String> {
    let (w, h) = text
        .split_once(['x', 'X'])
        .ok_or_else(|| format!("--size wants WxH, got {text:?}"))?;
    let parse = |v: &str, which| {
        v.trim()
            .parse::<usize>()
            .map_err(|_| format!("--size {which} is not a number: {v:?}"))
    };
    Ok((parse(w, "width")?, parse(h, "height")?))
}

/// Draws a single progress line, rewritten in place.
struct ProgressReporter {
    started: Instant,
    last_drawn: Instant,
}

impl ProgressReporter {
    fn new(started: Instant) -> Self {
        Self {
            started,
            last_drawn: started,
        }
    }

    fn report(&mut self, progress: Progress) {
        match progress {
            Progress::Started {
                total_frames,
                width,
                height,
            } => {
                eprintln!(
                    "{width}x{height}, {}",
                    total_frames
                        .map_or_else(|| "length unknown".to_string(), |n| format!("{n} frames"))
                );
            }
            Progress::Frame { done, total } => {
                // Redrawing every frame would spend more time on the terminal
                // than on the conversion.
                if self.last_drawn.elapsed().as_millis() < 100 {
                    return;
                }
                self.last_drawn = Instant::now();
                let rate = done as f64 / self.started.elapsed().as_secs_f64().max(1e-9);
                match total {
                    Some(total) if total > 0 => {
                        let remaining = total.saturating_sub(done) as f64 / rate.max(1e-9);
                        eprint!(
                            "\r  {done}/{total} ({:.0}%) {rate:.1} fps, {} left    ",
                            done as f64 / total as f64 * 100.0,
                            format_duration(remaining)
                        );
                    }
                    _ => eprint!("\r  {done} frames, {rate:.1} fps    "),
                }
                let _ = std::io::stderr().flush();
            }
            Progress::Note(message) => eprintln!("note: {message}"),
            Progress::Finished { .. } => {}
        }
    }
}

fn format_duration(seconds: f64) -> String {
    if !seconds.is_finite() || seconds < 0.0 {
        return "?".into();
    }
    let total = seconds.round() as u64;
    let (h, m, s) = (total / 3600, (total % 3600) / 60, total % 60);
    if h > 0 {
        format!("{h}h{m:02}m")
    } else if m > 0 {
        format!("{m}m{s:02}s")
    } else {
        format!("{s}s")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn size_is_parsed_from_either_separator() {
        assert_eq!(parse_size("1920x1080"), Ok((1920, 1080)));
        assert_eq!(parse_size("640X480"), Ok((640, 480)));
    }

    #[test]
    fn a_malformed_size_explains_itself() {
        assert!(parse_size("1920").unwrap_err().contains("WxH"));
        assert!(parse_size("axb").unwrap_err().contains("not a number"));
    }

    #[test]
    fn durations_read_naturally_at_every_scale() {
        assert_eq!(format_duration(45.0), "45s");
        assert_eq!(format_duration(125.0), "2m05s");
        assert_eq!(format_duration(7325.0), "2h02m");
        assert_eq!(format_duration(f64::INFINITY), "?");
    }

    #[test]
    fn the_command_line_is_internally_consistent() {
        // Catches conflicting flags, bad defaults and duplicate short options,
        // which clap only reports at runtime.
        use clap::CommandFactory;
        Cli::command().debug_assert();
    }
}
