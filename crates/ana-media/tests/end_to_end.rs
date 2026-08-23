// Ana-Convert — anaglyph 3D to full-colour stereo recovery
// SPDX-License-Identifier: GPL-3.0-or-later

//! A real file in, a real file out.
//!
//! Everything else in the suite tests one layer. This runs an actual anaglyph
//! clip through decode, conversion and encode, and reads the result back.

use std::path::Path;
use std::process::Command;
use std::time::Instant;

use ana_core::params::ConvertParams;
use ana_core::pipeline::{compose_output, process_frame, Sources};
use ana_media::{
    encode::{EncodeSettings, VideoCodec},
    locate, probe, Decoder, Encoder, FfmpegTools,
};

/// Builds a clip that is a genuine red/cyan anaglyph: a red-tinted left view
/// and a cyan-tinted right view of the same moving pattern, offset horizontally
/// to give it disparity.
fn make_anaglyph_clip(
    tools: &FfmpegTools,
    path: &Path,
    width: usize,
    height: usize,
    frames: usize,
) {
    let duration = frames as f64 / 24.0;
    let filter = format!(
        "[0:v]split=2[a][b];\
         [a]crop={w}:{h}:0:0,lutrgb=g=0:b=0[left];\
         [b]crop={w}:{h}:8:0,lutrgb=r=0[right];\
         [left][right]blend=all_mode=addition[out]",
        w = width,
        h = height
    );
    let status = Command::new(&tools.ffmpeg)
        .args([
            "-y",
            "-v",
            "error",
            "-f",
            "lavfi",
            "-i",
            &format!(
                "testsrc2=size={}x{}:rate=24:duration={duration}",
                width + 16,
                height
            ),
            "-filter_complex",
            &filter,
            "-map",
            "[out]",
            "-c:v",
            "libx264",
            "-crf",
            "12",
            "-pix_fmt",
            "yuv420p",
        ])
        .arg(path)
        .status()
        .expect("run ffmpeg");
    assert!(status.success(), "failed to build the anaglyph fixture");
}

#[test]
fn an_anaglyph_clip_converts_to_a_side_by_side_file() {
    let (tools, source) = locate(None).expect("ffmpeg must be installed");
    eprintln!("using ffmpeg from {source:?}: {}", tools.ffmpeg.display());

    let dir = tempfile::tempdir().expect("temp dir");
    let input = dir.path().join("anaglyph.mp4");
    let output = dir.path().join("sbs.mp4");
    make_anaglyph_clip(&tools, &input, 320, 240, 24);

    let info = probe(&tools, &input).expect("probe input");
    assert_eq!((info.width, info.height), (320, 240));

    let params = ConvertParams::default();
    let mut decoder = Decoder::open(&tools, &input, &info).expect("open decoder");
    let mut encoder = Encoder::create(
        &tools,
        &output,
        info.width * 2, // side-by-side
        info.height,
        &EncodeSettings {
            codec: VideoCodec::H264,
            fps: info.fps,
            audio_from: None,
            ..Default::default()
        },
    )
    .expect("create encoder");

    let started = Instant::now();
    let mut count = 0u64;
    while let Some(frame) = decoder.next_frame().expect("decode") {
        let pair = process_frame(Sources::from_anaglyph(&frame), &params);
        for composed in compose_output(&pair, &params) {
            encoder.write_frame(&composed).expect("encode");
        }
        count += 1;
    }
    encoder.finish().expect("finish encoding");

    let elapsed = started.elapsed();
    eprintln!(
        "converted {count} frames of {}x{} in {:.2}s ({:.1} fps)",
        info.width,
        info.height,
        elapsed.as_secs_f64(),
        count as f64 / elapsed.as_secs_f64()
    );

    assert_eq!(count, 24, "every input frame must be converted");

    let out_info = probe(&tools, &output).expect("probe output");
    assert_eq!(
        (out_info.width, out_info.height),
        (640, 240),
        "side-by-side output should be twice as wide"
    );
    assert_eq!(out_info.estimated_frame_count(), Some(24));
}

#[test]
fn the_recovered_eyes_differ_from_each_other() {
    // The point of the exercise: if both halves came back identical there
    // would be no stereo, and a bug that collapsed them would otherwise
    // produce a perfectly valid-looking file.
    let (tools, _) = locate(None).expect("ffmpeg must be installed");
    let dir = tempfile::tempdir().expect("temp dir");
    let input = dir.path().join("anaglyph.mp4");
    make_anaglyph_clip(&tools, &input, 160, 120, 4);

    let info = probe(&tools, &input).expect("probe");
    let mut decoder = Decoder::open(&tools, &input, &info).expect("open");
    let frame = decoder.next_frame().expect("decode").expect("a frame");

    let pair = process_frame(Sources::from_anaglyph(&frame), &ConvertParams::default());
    let difference: f32 = pair
        .left
        .as_slice()
        .iter()
        .zip(pair.right.as_slice())
        .map(|(a, b)| (a - b).abs())
        .sum::<f32>()
        / pair.left.as_slice().len() as f32;

    eprintln!("mean absolute difference between the eyes: {difference:.4}");
    assert!(
        difference > 0.01,
        "the two eyes should carry different views, got {difference:.4}"
    );
}

#[test]
#[ignore = "performance measurement; run with --ignored"]
fn measures_conversion_throughput_at_1080p() {
    let (tools, _) = locate(None).expect("ffmpeg must be installed");
    let dir = tempfile::tempdir().expect("temp dir");
    let input = dir.path().join("hd.mp4");
    make_anaglyph_clip(&tools, &input, 1920, 1080, 24);

    let info = probe(&tools, &input).expect("probe");
    let params = ConvertParams::default();
    let mut decoder = Decoder::open(&tools, &input, &info).expect("open");

    // Decode everything first, so the figure measures conversion rather than
    // how fast ffmpeg can feed us.
    let mut frames = Vec::new();
    while let Some(f) = decoder.next_frame().expect("decode") {
        frames.push(f);
    }

    let started = Instant::now();
    for frame in &frames {
        let pair = process_frame(Sources::from_anaglyph(frame), &params);
        std::hint::black_box(compose_output(&pair, &params));
    }
    let elapsed = started.elapsed();
    let fps = frames.len() as f64 / elapsed.as_secs_f64();
    eprintln!(
        "1080p conversion: {} frames in {:.2}s = {fps:.1} fps",
        frames.len(),
        elapsed.as_secs_f64()
    );
}
