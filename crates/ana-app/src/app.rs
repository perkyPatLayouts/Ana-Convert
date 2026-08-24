// Ana-Convert — anaglyph 3D to full-colour stereo recovery
// SPDX-License-Identifier: GPL-3.0-or-later

//! The window.
//!
//! The loop this replaces was: edit a parameter file, re-open it in
//! VirtualDubMod, scrub, squint, repeat. So the preview is the point — every
//! control here writes into the same [`ConvertParams`] the renderer uses, and
//! what the pane shows is the conversion itself, not an approximation of it.

use std::path::{Path, PathBuf};
use std::time::Instant;

use ana_core::compose::{EyeOrder, OutputLayout};
use ana_core::extract::AnaglyphFormat;
use ana_core::params::{ConvertParams, MonoEye, SourceTrim};
use ana_core::pipeline::{process_frame, Sources, StereoPair};
use ana_core::restore::ColourRestore;
use ana_core::timecode::format_timecode;
use ana_core::transfer::TransferFunction;
use ana_core::FrameF32;
use ana_media::encode::{EncodeSettings, VideoCodec};
use ana_media::{grab_frame, locate, probe, FfmpegTools, VideoInfo};
use ana_pipeline::{output_paths, RenderJob};

use crate::preview::{scale_params_for_preview, PreviewCache, PreviewWork};
use crate::render::{describe_outputs, format_duration, Finished, RunningRender};
use crate::view::{compose_view, ViewMode};

/// Never preview below this fraction of the source size. Shrinking an anaglyph
/// blends neighbouring pixels, which mixes the two eyes together, so too small
/// a preview stops representing the conversion at all.
const MIN_PREVIEW_SCALE: f32 = 0.25;

/// One source file and what we know about it.
struct Source {
    path: PathBuf,
    info: VideoInfo,
}

/// The frames feeding one preview, already decoded.
#[derive(Default)]
struct Decoded {
    anaglyph: Option<FrameF32>,
    right_eye: Option<FrameF32>,
    colour: Option<FrameF32>,
    mono: Option<FrameF32>,
}

pub struct AnaApp {
    tools: Result<FfmpegTools, String>,

    anaglyph: Option<Source>,
    /// The 2D release, if there is one. One file serving both purposes, because
    /// a transfer that *is* an eye is also the best colour reference for the
    /// other one.
    secondary: Option<Source>,
    secondary_role: SecondaryRole,
    /// The right eye, when the source is two files rather than one.
    right_eye: Option<Source>,
    audio: Option<PathBuf>,
    output: Option<PathBuf>,

    params: ConvertParams,
    encode: EncodeSettings,

    frame: u64,
    /// Independent position in a secondary source, used only while aligning.
    align_frame: u64,
    aligning: Option<Role>,
    view: ViewMode,
    cache: PreviewCache,
    decoded: Decoded,
    pair: Option<StereoPair>,
    texture: Option<egui::TextureHandle>,
    last_process: Option<f32>,
    preview_scale: f32,

    help_open: bool,
    running: Option<RunningRender>,
    outcome: Option<String>,
    problem: Option<String>,
}

impl AnaApp {
    /// Builds the app, optionally opening a film named on the command line.
    pub fn new(cc: &eframe::CreationContext<'_>, open: Option<PathBuf>) -> Self {
        // Dark, and not negotiably so. `set_visuals` alone only dresses the
        // theme currently in use, so on a Mac set to Light the app fell back to
        // egui's default light palette and the cyan headings landed on
        // near-white. Both themes are set, and the preference pinned, so the
        // picture is always judged against a neutral dark surround.
        cc.egui_ctx.set_theme(egui::ThemePreference::Dark);
        cc.egui_ctx
            .set_visuals_of(egui::Theme::Dark, high_contrast_visuals());
        cc.egui_ctx
            .set_visuals_of(egui::Theme::Light, high_contrast_visuals());
        cc.egui_ctx.all_styles_mut(|style| {
            // egui's defaults are small for a desktop app read at arm's length:
            // body and button text at 13px and the "small" style at 9px, which
            // is what the notes and warnings underneath the controls use.
            use egui::{FontFamily::Proportional, FontId, TextStyle};
            style.text_styles = [
                (TextStyle::Small, FontId::new(13.0, Proportional)),
                (TextStyle::Body, FontId::new(15.0, Proportional)),
                (TextStyle::Button, FontId::new(15.0, Proportional)),
                (TextStyle::Heading, FontId::new(20.0, Proportional)),
                (
                    TextStyle::Monospace,
                    FontId::new(14.0, egui::FontFamily::Monospace),
                ),
            ]
            .into();

            // Controls sit closer together than the default, which leaves room
            // for the settings column to be read without scrolling.
            style.spacing.item_spacing = egui::vec2(8.0, 6.0);
            style.spacing.button_padding = egui::vec2(9.0, 5.0);
        });
        let mut app = Self {
            tools: locate(None).map(|(t, _)| t).map_err(|e| e.to_string()),
            anaglyph: None,
            secondary: None,
            secondary_role: SecondaryRole::default(),
            right_eye: None,
            audio: None,
            output: None,
            params: ConvertParams::default(),
            encode: EncodeSettings::default(),
            frame: 0,
            align_frame: 0,
            aligning: None,
            view: ViewMode::default(),
            cache: PreviewCache::new(),
            decoded: Decoded::default(),
            pair: None,
            texture: None,
            last_process: None,
            preview_scale: 1.0,
            help_open: false,
            running: None,
            outcome: None,
            problem: None,
        };
        if let Some(path) = open {
            app.open(Role::Anaglyph, path);
        }
        app
    }

    /// Opens anything dropped on the window.
    fn take_dropped_files(&mut self, ctx: &egui::Context) {
        let dropped: Vec<PathBuf> = ctx.input(|i| {
            i.raw
                .dropped_files
                .iter()
                .map(|f| f.path().to_path_buf())
                .collect()
        });
        for path in dropped {
            if !looks_like_video(&path) {
                self.problem = Some(format!(
                    "{} does not look like a video file",
                    path.file_name().unwrap_or_default().to_string_lossy()
                ));
                continue;
            }
            let role = match role_for_drop(self.anaglyph.is_some()) {
                DropTarget::Source => Role::Anaglyph,
                DropTarget::Secondary => Role::Secondary,
            };
            self.open(role, path);
        }
    }

    fn tools(&self) -> Option<&FfmpegTools> {
        self.tools.as_ref().ok()
    }

    fn frame_count(&self) -> u64 {
        self.anaglyph
            .as_ref()
            .and_then(|s| s.info.estimated_frame_count())
            .unwrap_or(1)
            .max(1)
    }

    /// Opens a source file, probing it and clearing anything derived from the
    /// previous one.
    fn open(&mut self, role: Role, path: PathBuf) {
        let Some(tools) = self.tools() else { return };
        match probe(tools, &path) {
            Ok(info) => {
                let source = Some(Source { path, info });
                match role {
                    Role::Anaglyph => {
                        self.frame = 0;
                        // Default the output next to the input, and the audio
                        // to the film itself, which is nearly always right.
                        if let Some(s) = &source {
                            self.audio = s.info.has_audio.then(|| s.path.clone());
                            self.output = Some(default_output_for(&s.path));
                        }
                        self.anaglyph = source;
                    }
                    Role::Secondary => self.secondary = source,
                    Role::RightEye => self.right_eye = source,
                }
                self.problem = None;
                self.cache.invalidate();
            }
            Err(e) => self.problem = Some(format!("{}: {e}", path.display())),
        }
    }

    /// Brings the preview up to date, decoding and converting only what changed.
    fn refresh_preview(&mut self, ctx: &egui::Context, target_width: f32) {
        let (Some(tools), Some(anaglyph)) = (self.tools.as_ref().ok(), self.anaglyph.as_ref())
        else {
            return;
        };

        // The preview is drawn at whatever width the pane offers, and the blur
        // settings are rescaled to match so it stays representative.
        let scale = (target_width / anaglyph.info.width as f32).clamp(MIN_PREVIEW_SCALE, 1.0);
        if (scale - self.preview_scale).abs() > 0.01 {
            self.preview_scale = scale;
            self.cache.invalidate();
        }

        let preview_params = self.preview_params();
        match self.cache.work_for(self.frame, &preview_params) {
            PreviewWork::Nothing => return,
            PreviewWork::DecodeAndProcess => {
                if let Err(e) = self.decode_current(tools.clone()) {
                    self.problem = Some(e);
                    return;
                }
                self.cache.record_decode(self.frame);
            }
            PreviewWork::Reprocess => {}
        }

        let started = Instant::now();
        let Some(source) = self.decoded.anaglyph.as_ref() else {
            return;
        };
        let pair = process_frame(
            Sources {
                primary: source,
                right_eye: self.decoded.right_eye.as_ref(),
                colour: self.decoded.colour.as_ref(),
                mono: self.decoded.mono.as_ref(),
            },
            &preview_params,
        );
        self.last_process = Some(started.elapsed().as_secs_f32() * 1000.0);
        self.cache.record_process(self.frame, &preview_params);
        self.pair = Some(pair);
        self.upload(ctx);
    }

    /// The shape the preview pane should draw the current view at.
    fn preview_display_aspect(&self) -> f64 {
        let Some(source) = &self.anaglyph else {
            return 1.0;
        };
        let eye = self.params.eye_display_aspect(source.info.display_aspect());
        self.view.display_aspect(eye)
    }

    /// The conversion settings for the preview: the real ones, with blur
    /// rescaled to the size actually being shown.
    fn preview_params(&self) -> ConvertParams {
        scale_params_for_preview(&self.effective_params(), self.preview_scale)
    }

    /// The settings a conversion actually runs with.
    ///
    /// The 2D source's role lives in the app rather than in the settings, so it
    /// has to be folded in — and it has to be folded in for the preview and the
    /// render through the same function, or the two quietly disagree about what
    /// they are showing.
    fn effective_params(&self) -> ConvertParams {
        ConvertParams {
            mono_eye: if self.secondary.is_some() {
                self.secondary_role.mono_eye()
            } else {
                MonoEye::None
            },
            // One file, so one alignment.
            mono_trim: self.params.colour_trim,
            ..self.params.clone()
        }
    }

    fn decode_current(&mut self, tools: FfmpegTools) -> Result<(), String> {
        let anaglyph = self.anaglyph.as_ref().ok_or("no anaglyph loaded")?;
        let scale = self.preview_scale;
        let grab = |source: &Source, index: u64| -> Result<FrameF32, String> {
            let frame = grab_frame(&tools, &source.path, &source.info, index)
                .map_err(|e| format!("{}: {e}", source.path.display()))?;
            Ok(downscale(&frame, scale))
        };

        // Secondary sources are read through their own alignment, so scrubbing
        // the anaglyph pulls each of them to the matching moment.
        let base = grab(anaglyph, self.frame)?;
        let (w, h) = (base.width(), base.height());
        // Secondary sources may be another resolution entirely; bring them to
        // the anaglyph's geometry so nothing downstream ever sees a mismatch.
        let grab_conformed = |source: &Source, index: u64| -> Result<FrameF32, String> {
            Ok(ana_core::compose::conform_to(&grab(source, index)?, w, h))
        };
        self.decoded.anaglyph = Some(base);
        // One decode serves both purposes: colour always, and the eye itself
        // when the file is known to be one.
        let secondary = match &self.secondary {
            Some(s) => {
                let at = self
                    .params
                    .aligned_frame(&self.params.colour_trim, self.frame);
                Some(grab_conformed(s, at)?)
            }
            None => None,
        };
        self.decoded.right_eye = match &self.right_eye {
            Some(s) => Some(grab_conformed(s, self.frame)?),
            None => None,
        };
        self.decoded.mono = self
            .secondary_role
            .supplies_an_eye()
            .then(|| secondary.clone())
            .flatten();
        self.decoded.colour = secondary;
        Ok(())
    }

    fn upload(&mut self, ctx: &egui::Context) {
        let Some(pair) = &self.pair else { return };
        let image = compose_view(
            pair,
            self.view,
            self.params.input_format,
            self.params.eye_order,
        );
        let rgb = image.to_rgb8();
        let colour = egui::ColorImage::from_rgb([image.width(), image.height()], &rgb);
        self.texture = Some(ctx.load_texture("preview", colour, egui::TextureOptions::LINEAR));
    }

    fn job(&self) -> Option<RenderJob> {
        let anaglyph = self.anaglyph.as_ref()?;
        let secondary = self.secondary.as_ref().map(|s| s.path.clone());
        Some(RenderJob {
            anaglyph: anaglyph.path.clone(),
            right_eye: self.right_eye.as_ref().map(|s| s.path.clone()),
            colour: secondary.clone(),
            // The same file, when it is known to be an eye rather than just a
            // colour reference.
            mono: self
                .secondary_role
                .supplies_an_eye()
                .then(|| secondary.clone())
                .flatten(),
            audio: self.audio.clone(),
            output: self.output.clone()?,
            params: self.effective_params(),
            encode: EncodeSettings {
                fps: if anaglyph.info.fps > 0.0 {
                    anaglyph.info.fps
                } else {
                    24.0
                },
                ..self.encode.clone()
            },
        })
    }

    fn start_render(&mut self) {
        let (Some(tools), Some(job)) = (self.tools().cloned(), self.job()) else {
            self.problem = Some("choose an anaglyph file and an output first".into());
            return;
        };
        if let Err(e) = job.params.validate() {
            self.problem = Some(e.to_string());
            return;
        }
        self.outcome = None;
        self.problem = None;
        self.running = Some(RunningRender::start(tools, job));
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Role {
    Anaglyph,
    Secondary,
    RightEye,
}

impl Role {
    /// True for the source that has to be lined up against the main one.
    fn is_secondary(self) -> bool {
        self != Self::Anaglyph
    }

    fn label(self) -> &'static str {
        match self {
            Self::Anaglyph => "Source",
            Self::Secondary => "2D source",
            Self::RightEye => "Right eye file",
        }
    }
}

/// A darker background with brighter controls than egui's default.
///
/// The stock dark theme draws controls only slightly lighter than the panel
/// behind them, which on a bright screen leaves buttons and dropdowns looking
/// like flat text. Everything here widens that gap: stronger fills, visible
/// outlines on every widget, and near-white text.
fn high_contrast_visuals() -> egui::Visuals {
    let mut v = egui::Visuals::dark();
    let grey = |n: u8| egui::Color32::from_gray(n);

    v.panel_fill = grey(22);
    v.window_fill = grey(26);
    v.extreme_bg_color = grey(10);
    v.faint_bg_color = grey(36);
    v.override_text_color = Some(grey(244));

    // Resting controls: clearly raised off the panel, with a visible edge.
    v.widgets.inactive.weak_bg_fill = grey(62);
    v.widgets.inactive.bg_fill = grey(68);
    v.widgets.inactive.bg_stroke = egui::Stroke::new(1.0, grey(110));
    v.widgets.inactive.fg_stroke = egui::Stroke::new(1.0, grey(236));

    v.widgets.hovered.weak_bg_fill = grey(88);
    v.widgets.hovered.bg_fill = grey(96);
    v.widgets.hovered.bg_stroke = egui::Stroke::new(1.5, grey(160));
    v.widgets.hovered.fg_stroke = egui::Stroke::new(1.5, egui::Color32::WHITE);

    v.widgets.active.weak_bg_fill = grey(118);
    v.widgets.active.bg_fill = grey(126);
    v.widgets.active.bg_stroke = egui::Stroke::new(2.0, egui::Color32::WHITE);
    v.widgets.active.fg_stroke = egui::Stroke::new(2.0, egui::Color32::WHITE);

    v.widgets.open.weak_bg_fill = grey(78);
    v.widgets.open.bg_stroke = egui::Stroke::new(1.0, grey(140));

    // Labels and separators, which should read without shouting.
    v.widgets.noninteractive.bg_stroke = egui::Stroke::new(1.0, grey(78));
    v.widgets.noninteractive.fg_stroke = egui::Stroke::new(1.0, grey(220));

    v.selection.bg_fill = egui::Color32::from_rgb(0x2E, 0x7D, 0xC8);
    v.selection.stroke = egui::Stroke::new(1.0, egui::Color32::WHITE);
    v.hyperlink_color = SECTION_COLOUR;
    v.warn_fg_color = egui::Color32::from_rgb(0xFF, 0xC1, 0x4E);
    v.error_fg_color = egui::Color32::from_rgb(0xFF, 0x87, 0x6B);
    v
}

/// Section headings. Bright cyan, picked to sit against the dark panel and to
/// echo the cyan half of an anaglyph.
const SECTION_COLOUR: egui::Color32 = egui::Color32::from_rgb(0x8F, 0xE6, 0xF5);

/// The help text, as headed sections.
const HELP: &[(&str, &str)] = &[
    (
        "What this does",
        "Anaglyph 3D throws away most of each eye. The red channel carries one eye and the \
         cyan channels the other, so each eye survives as brightness only and the colour \
         you see is a blend of both. This recovers a full-colour stereo pair from that: it \
         takes each eye's brightness from the channels that carried it, and paints colour \
         back on from a heavily blurred reference.\n\n\
         It also works the other way, turning a side-by-side or top-and-bottom pair back \
         into an anaglyph, or pulling a single eye out as a flat 2D file.",
    ),
    (
        "Source: what the file holds",
        "An anaglyph — red/cyan, green/magenta or red/blue — needs recovering, and the \
         Recovery settings apply.\n\n\
         A side-by-side or top-and-bottom pair is already stereo. Nothing needs recovering; \
         the two eyes are simply taken apart, and the Recovery settings are hidden because \
         they do not apply.",
    ),
    (
        "Anamorphic",
        "Broadcast and disc stereo usually squeeze each eye to half size so the pair fits \
         one ordinary frame: a 1920×1080 file holding two 960×1080 eyes, each meant to be \
         seen at the full 1920×1080. Tick this and each eye is stretched back.\n\n\
         Leave it clear for full-resolution packing — a 3840×1080 frame holding two \
         1920×1080 eyes — where no stretch is wanted. If people come out too narrow, it \
         wanted ticking; too wide, and it did not.",
    ),
    (
        "2D Source: colour reference, or an eye",
        "A 2D release of the same film is the single biggest quality improvement available, \
         and it can be used two ways.\n\n\
         As a COLOUR REFERENCE ONLY, both eyes are still recovered from the anaglyph. The \
         file supplies only hue. This is what removes the red/cyan cast, because the \
         anaglyph's own colours are a blend of two views and are wrong wherever those views \
         disagree — which is exactly where the depth is.\n\n\
         As THE LEFT EYE or THE RIGHT EYE, that eye is not recovered at all. It is passed \
         straight through, untouched and perfect, and only the other eye is reconstructed — \
         using this same file for its colour. This is the best result the method can give, \
         and it is worth checking whether your disc carries a 2D version for exactly this \
         reason.\n\n\
         Either way the two files must line up. Use Align sources… to scrub both to the \
         same moment and mark it.",
    ),
    (
        "Recovery: colour blur",
        "Each eye survives as brightness; colour has to come from somewhere else, blurred so \
         it covers the horizontal offset between the two views. Lower percentages blur \
         harder. Horizontal wants much more than vertical, because that is the direction \
         the eyes are displaced in — vertical blur only helps with cameras that were \
         misaligned.\n\n\
         Too little and colour fringes survive around objects at depth; too much and colour \
         bleeds across edges. Neither setting will fix a shot where the eyes are very far \
         apart: there the anaglyph's colour is composed from two different points in the \
         scene, and only a real 2D reference helps.",
    ),
    (
        "Recovery: reconstruction",
        "Offset is the default and the right choice for real film. It never divides, so \
         noisy shadows stay clean.\n\n\
         Scale is sharper on a clean source and preserves colour more exactly, but it \
         divides by the reference's brightness, and a red/cyan anaglyph's shadows are \
         exactly where that approaches zero — on grainy or heavily compressed film it \
         breaks dark areas into cyan speckle.",
    ),
    (
        "Recovery: ghosting and de-fringe",
        "Ghosting removes each eye's ghost from the other, for cross-talk baked in by the \
         mastering. It will not fix fringing caused by disparity — if raising it darkens the \
         picture rather than cleaning it, that fringing is disparity and the setting is the \
         wrong tool.\n\n\
         De-fringe softens the white edges that excessive sharpening leaves on DVD-era \
         transfers. 1.0 is off.",
    ),
    (
        "Destination",
        "Side by side and top and bottom are what stereo displays and headsets expect. Two \
         files gives one per eye. Anaglyph muxes the pair back for ordinary screens, and its \
         colour mode is independent of the source's — recovering a red/cyan transfer and \
         writing green/magenta is perfectly reasonable. Left or right eye alone gives a flat \
         2D file.",
    ),
    (
        "Preview",
        "The preview shows the conversion itself, not an approximation, and honours the \
         source's pixel shape. Left and Right show one eye; Side by side shows both; \
         Anaglyph re-encodes the result so it can be checked through the glasses; \
         Difference shows where the two eyes disagree, which is where the depth is.",
    ),
    (
        "Range and alignment",
        "Each source has its own in and out points. The main source's range decides the \
         length of the output, and the 2D source is read in step with it. Setting the start \
         of each to the same visual moment is what keeps two differently edited releases \
         together — a cut is the easiest thing to match on.",
    ),
];

/// The anaglyph encodings, in the order a menu should list them.
const ANAGLYPH_FORMATS: [AnaglyphFormat; 3] = [
    AnaglyphFormat::RedCyan,
    AnaglyphFormat::GreenMagenta,
    AnaglyphFormat::RedBlue,
];

/// A plain-language description of what the source is, for the header.
pub fn describe_source(params: &ConvertParams) -> String {
    use ana_core::params::InputMode;
    match params.input {
        InputMode::Anaglyph => format!("Anaglyph ({})", format_name(params.input_format)),
        InputMode::Packed {
            packing,
            anamorphic,
            ..
        } => {
            let squeeze = if anamorphic { ", anamorphic" } else { "" };
            format!("{}{squeeze}", packing.label())
        }
        InputMode::TwoFiles => "Left eye of a two-file pair".to_string(),
    }
}

/// How the eye order reads for a given layout.
///
/// The setting is the same either way — which eye is written first — but
/// "left eye first" is meaningless when the two are stacked vertically.
pub fn eye_order_name(order: EyeOrder, stacked: bool) -> &'static str {
    match (order, stacked) {
        (EyeOrder::LeftFirst, false) => "Left eye on the left",
        (EyeOrder::RightFirst, false) => "Right eye on the left",
        (EyeOrder::LeftFirst, true) => "Left eye on top",
        (EyeOrder::RightFirst, true) => "Right eye on top",
    }
}

/// Why someone might choose one anaglyph encoding over another.
fn output_format_hint(format: AnaglyphFormat) -> &'static str {
    match format {
        AnaglyphFormat::RedCyan => "The usual choice, and the glasses most people own.",
        AnaglyphFormat::GreenMagenta => {
            "Holds colour better than red/cyan and ghosts less on many screens."
        }
        AnaglyphFormat::RedBlue => {
            "The oldest arrangement. Poor colour, but very forgiving glasses."
        }
    }
}

/// What a 2D release of the same film is being used for.
///
/// The two are not alternatives so much as degrees of knowledge. A 2D transfer
/// always helps with colour, because the anaglyph's own colours are a blend of
/// both eyes and wrong wherever the two disagree. But if you also know it *is*
/// one of the eyes, that eye needs no reconstruction at all: it can be passed
/// straight through, perfect, and only the other one recovered — using this
/// same file for its colour.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SecondaryRole {
    /// Colour only. Both eyes are still recovered from the anaglyph.
    #[default]
    ColourOnly,
    /// This file is the left eye. Passed through untouched; the right is rebuilt.
    IsLeftEye,
    /// This file is the right eye.
    IsRightEye,
}

impl SecondaryRole {
    pub const ALL: [SecondaryRole; 3] = [Self::ColourOnly, Self::IsLeftEye, Self::IsRightEye];

    pub fn label(self) -> &'static str {
        match self {
            Self::ColourOnly => "Colour reference only",
            Self::IsLeftEye => "This is the left eye",
            Self::IsRightEye => "This is the right eye",
        }
    }

    pub fn hint(self) -> &'static str {
        match self {
            Self::ColourOnly => {
                "Both eyes are still recovered from the anaglyph. This file only                  supplies colour, which is what removes the red/cyan cast."
            }
            Self::IsLeftEye => {
                "The left eye is this file, untouched and perfect. Only the right                  eye is reconstructed, using this file for its colour."
            }
            Self::IsRightEye => {
                "The right eye is this file, untouched and perfect. Only the left                  eye is reconstructed, using this file for its colour."
            }
        }
    }

    /// Which eye this file replaces outright, if any.
    pub fn mono_eye(self) -> MonoEye {
        match self {
            Self::ColourOnly => MonoEye::None,
            Self::IsLeftEye => MonoEye::Left,
            Self::IsRightEye => MonoEye::Right,
        }
    }

    /// Whether the file is handed to the renderer as an eye as well as colour.
    pub fn supplies_an_eye(self) -> bool {
        self != Self::ColourOnly
    }
}

/// Which slot a dropped file should fill.
///
/// Dropping is how a bundled app is normally used, so it has to guess well: the
/// first video becomes the source, and anything dropped afterwards is almost
/// always the 2D release someone is adding to it.
pub fn role_for_drop(has_source: bool) -> DropTarget {
    if has_source {
        DropTarget::Secondary
    } else {
        DropTarget::Source
    }
}

/// Where a dropped file should go.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DropTarget {
    /// Becomes the film being converted.
    Source,
    /// Becomes the 2D release.
    Secondary,
}

/// Whether a path looks like something worth trying to open.
pub fn looks_like_video(path: &Path) -> bool {
    const KNOWN: [&str; 12] = [
        "mkv", "mp4", "m4v", "mov", "avi", "mpg", "mpeg", "wmv", "ts", "m2ts", "vob", "webm",
    ];
    path.extension()
        .map(|e| e.to_string_lossy().to_ascii_lowercase())
        .is_some_and(|e| KNOWN.contains(&e.as_str()))
}

/// The size a picture should be drawn at: the shape it is meant to be seen
/// at, scaled to fit the space, never distorted.
///
/// Deliberately separate from the drawing so it can be tested. Getting this
/// wrong is invisible in a screenshot-free test suite, and it was: the pane
/// used `centered_and_justified`, which *justifies* — it stretches its child to
/// fill the space — so the requested size was discarded and every non-square
/// source was previewed at its raw pixel shape.
pub fn fitted_size(available: egui::Vec2, pixel_height: f32, display_aspect: f32) -> egui::Vec2 {
    let aspect = if display_aspect.is_finite() && display_aspect > 0.0 {
        display_aspect
    } else {
        1.0
    };
    let shape = egui::vec2(pixel_height.max(1.0) * aspect, pixel_height.max(1.0));
    // Whichever axis runs out first decides the scale, then bounded so a tiny
    // source is not magnified absurdly and a zero-sized pane cannot collapse it.
    let fit = (available.x / shape.x).min(available.y / shape.y);
    let scale = fit.clamp(0.01, 4.0);
    shape * scale
}

/// Paints the preview centred in `available`, at the shape it should be seen
/// at, and returns the rectangle actually used.
///
/// Placed by hand rather than through a centring layout: `centered_and_justified`
/// *justifies*, meaning it stretches its child to fill the space, which silently
/// discards the size it is given. That is what made three separate aspect-ratio
/// faults reach the user with the arithmetic already correct.
///
/// Returning the rectangle is what lets a headless test measure the result.
pub fn paint_preview(
    ui: &mut egui::Ui,
    available: egui::Vec2,
    pixel_height: f32,
    display_aspect: f32,
    texture: Option<&egui::TextureHandle>,
) -> egui::Rect {
    let size = fitted_size(available, pixel_height, display_aspect);
    let (pane, _) = ui.allocate_exact_size(available, egui::Sense::hover());
    let target = egui::Rect::from_center_size(pane.center(), size);
    if let Some(texture) = texture {
        egui::Image::new(texture).paint_at(ui, target);
    }
    target
}

/// Uploads a frame and draws it, scaled to the space available.
fn show_frame(
    ui: &mut egui::Ui,
    ctx: &egui::Context,
    id: &str,
    frame: &FrameF32,
    display_aspect: f64,
) {
    let rgb = frame.to_rgb8();
    let image = egui::ColorImage::from_rgb([frame.width(), frame.height()], &rgb);
    let texture = ctx.load_texture(id, image, egui::TextureOptions::LINEAR);
    let available = egui::vec2(ui.available_width(), f32::INFINITY);
    let size = fitted_size(available, texture.size_vec2().y, display_aspect as f32);
    let (rect, _) = ui.allocate_exact_size(size, egui::Sense::hover());
    egui::Image::new(&texture).paint_at(ui, rect);
}

/// Shrinks a frame for the preview, skipping the work at full size.
///
/// Always to an even size. A packed source is split down the middle, and an odd
/// frame has no middle — the preview would drop a different seam column than
/// the render does, and the two would quietly disagree.
fn downscale(frame: &FrameF32, scale: f32) -> FrameF32 {
    if scale >= 0.999 {
        return frame.clone();
    }
    let even = |n: f32| ((n.round() as usize) & !1).max(2);
    let w = even(frame.width() as f32 * scale);
    let h = even(frame.height() as f32 * scale);
    ana_core::compose::resize(frame, w, h)
}

/// `film.mkv` becomes `film-stereo.mkv`, so a render never lands on its input.
fn default_output_for(input: &Path) -> PathBuf {
    let stem = input
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| "output".into());
    let ext = input
        .extension()
        .map(|e| e.to_string_lossy().into_owned())
        .unwrap_or_else(|| "mkv".into());
    input.with_file_name(format!("{stem}-stereo.{ext}"))
}

impl eframe::App for AnaApp {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        if let Some(running) = &mut self.running {
            running.pump();
            if let Some(finished) = running.take_result() {
                self.outcome = Some(match finished {
                    Finished::Succeeded { summary, elapsed } => {
                        let verb = if summary.cancelled {
                            "stopped after"
                        } else {
                            "converted"
                        };
                        format!(
                            "{verb} {} frames in {} → {}",
                            summary.frames,
                            format_duration(elapsed),
                            describe_outputs(&summary.outputs)
                        )
                    }
                    Finished::Failed(message) => {
                        self.problem = Some(message);
                        "render failed".into()
                    }
                });
                self.running = None;
                // Put the preview back where it was. The bottom bar changes
                // height when the progress row goes away, and without a fresh
                // decode and an explicit repaint the pane is left holding
                // whatever it managed to draw during that reshuffle.
                self.cache.invalidate();
                ctx.request_repaint();
            } else {
                // Keep the progress bar moving while the worker runs.
                ctx.request_repaint_after(std::time::Duration::from_millis(100));
            }
        }

        self.take_dropped_files(&ctx);
        self.help_window(&ctx);
        self.top_bar(ui);
        self.side_panel(ui);
        self.bottom_bar(ui);
        self.preview_pane(ui);
    }
}

// --- panels -----------------------------------------------------------------

impl AnaApp {
    fn top_bar(&mut self, ui: &mut egui::Ui) {
        egui::Panel::top("sources").show(ui, |ui| {
            ui.add_space(4.0);
            ui.horizontal_wrapped(|ui| {
                let two_files = self.params.input.needs_second_file();
                let open_label = if two_files {
                    "Open Left Eye…"
                } else {
                    "Open Source…"
                };
                if ui.button(open_label).clicked() {
                    if let Some(path) = pick_video() {
                        self.open(Role::Anaglyph, path);
                    }
                }
                if two_files
                    && ui
                        .button("Open Right Eye…")
                        .on_hover_text("The second file, holding the other eye.")
                        .clicked()
                {
                    if let Some(path) = pick_video() {
                        self.open(Role::RightEye, path);
                    }
                }
                // Two per-eye files need no help from a 2D release: nothing is
                // being recovered, so there is no colour to supply and no eye
                // to stand in for.
                if ui
                    .add_enabled(!two_files, egui::Button::new("Open 2D Source…"))
                    .on_hover_text(if two_files {
                        "Not used with two per-eye files — both eyes are already complete."
                    } else {
                        "A 2D release of the same film. It supplies colour, and if it is \
                         one of the eyes it can be used for that eye directly."
                    })
                    .clicked()
                {
                    if let Some(path) = pick_video() {
                        self.open(Role::Secondary, path);
                    }
                }
                if self.secondary.is_some() {
                    let active = self.aligning.is_some();
                    if ui
                        .selectable_label(
                            active,
                            if active {
                                "Close alignment"
                            } else {
                                "Align sources…"
                            },
                        )
                        .on_hover_text(
                            "Scrub both files to the same moment and mark it, so two \
                             differently edited releases stay in step.",
                        )
                        .clicked()
                    {
                        self.aligning = if active { None } else { Some(Role::Secondary) };
                        self.align_frame = self.frame;
                        self.cache.invalidate();
                    }
                }
                ui.separator();
                if ui.button("Load preset…").clicked() {
                    self.load_preset();
                }
                if ui.button("Save preset…").clicked() {
                    self.save_preset();
                }
                ui.separator();
                if ui.selectable_label(self.help_open, "Help").clicked() {
                    self.help_open = !self.help_open;
                }
            });

            if let Err(message) = &self.tools {
                ui.add_space(4.0);
                ui.colored_label(egui::Color32::from_rgb(230, 120, 90), message);
            }

            ui.add_space(3.0);
            match &self.anaglyph {
                Some(s) => {
                    // Says what the file is as well as how big it is, so the
                    // source setting can be checked at a glance rather than
                    // remembered.
                    ui.label(
                        egui::RichText::new(format!(
                            "{}   ·   {}   ·   {}×{} at {:.3} fps",
                            s.path.file_name().unwrap_or_default().to_string_lossy(),
                            describe_source(&self.params),
                            s.info.width,
                            s.info.height,
                            s.info.fps,
                        ))
                        .strong(),
                    );
                    if let Some(second) = &self.secondary {
                        ui.label(
                            egui::RichText::new(format!(
                                "2D:  {}   ·   {}",
                                second
                                    .path
                                    .file_name()
                                    .unwrap_or_default()
                                    .to_string_lossy(),
                                self.secondary_role.label(),
                            ))
                            .weak()
                            .small(),
                        );
                    }
                }
                None if self.tools.is_ok() => {
                    ui.label(egui::RichText::new("Open a film, or drop one on the window.").weak());
                }
                None => {}
            }
            ui.add_space(4.0);
        });
    }

    fn side_panel(&mut self, ui: &mut egui::Ui) {
        egui::Panel::right("params")
            .default_size(340.0)
            .show(ui, |ui| {
                egui::ScrollArea::vertical()
                    .auto_shrink([false, false])
                    .scroll_bar_visibility(egui::scroll_area::ScrollBarVisibility::AlwaysVisible)
                    .show(ui, |ui| {
                        ui.add_space(4.0);
                        self.source_section(ui);
                        self.secondary_section(ui);
                        if self.params.input.is_anaglyph() {
                            self.recovery_section(ui);
                        }
                        self.grade_section(ui);
                        self.destination_section(ui);
                        ui.add_space(12.0);
                    });
            });
    }

    /// A heading with a rule under it, so the sections read as sections.
    fn section(ui: &mut egui::Ui, title: &str) {
        ui.add_space(12.0);
        ui.label(
            egui::RichText::new(title.to_uppercase())
                .strong()
                .size(17.0)
                .color(SECTION_COLOUR),
        );
        ui.add_space(1.0);
        ui.separator();
        ui.add_space(4.0);
    }

    fn source_section(&mut self, ui: &mut egui::Ui) {
        Self::section(ui, "Source");

        let mut kind = SourceKind::of(self.params.input);
        egui::ComboBox::from_label("This file holds")
            .selected_text(kind.label())
            .show_ui(ui, |ui| {
                for k in SourceKind::ALL {
                    ui.selectable_value(&mut kind, k, k.label())
                        .on_hover_text(k.hint());
                }
            });

        let mut anamorphic = matches!(
            self.params.input,
            ana_core::params::InputMode::Packed {
                anamorphic: true,
                ..
            }
        );
        match kind {
            SourceKind::Anaglyph => {
                egui::ComboBox::from_label("Colour mode")
                    .selected_text(format_name(self.params.input_format))
                    .show_ui(ui, |ui| {
                        for f in ANAGLYPH_FORMATS {
                            ui.selectable_value(&mut self.params.input_format, f, format_name(f));
                        }
                    });
            }
            SourceKind::TwoFiles => {
                match &self.right_eye {
                    Some(second) => {
                        ui.label(format!(
                            "Right eye:  {}",
                            second
                                .path
                                .file_name()
                                .unwrap_or_default()
                                .to_string_lossy()
                        ));
                    }
                    None => {
                        ui.colored_label(
                            ui.visuals().warn_fg_color,
                            "Choose the right-eye file with Open Right Eye…",
                        );
                    }
                }
                ui.label(
                    egui::RichText::new(
                        "This file is the left eye. Tick Swap eyes below if it is \
                         actually the right.",
                    )
                    .small()
                    .weak(),
                );
            }
            _ => {
                ui.checkbox(&mut anamorphic, "Each eye is squeezed (anamorphic)")
                    .on_hover_text(
                        "Half-width side-by-side or half-height top-and-bottom, as broadcast \
                         and disc stereo normally are. Stretches each eye back to full size.",
                    );
            }
        }
        let rebuilt = kind.to_input(anamorphic);
        if rebuilt != self.params.input {
            self.params.input = rebuilt;
            self.cache.invalidate();
        }

        egui::ComboBox::from_label("Transfer")
            .selected_text(transfer_name(self.params.transfer))
            .show_ui(ui, |ui| {
                for t in [
                    TransferFunction::Srgb,
                    TransferFunction::Bt709,
                    TransferFunction::Linear,
                ] {
                    ui.selectable_value(&mut self.params.transfer, t, transfer_name(t));
                }
            });

        self.trim_row(ui, "Range", Role::Anaglyph, self.source_fps());
    }

    fn secondary_section(&mut self, ui: &mut egui::Ui) {
        Self::section(ui, "2D Source");
        let Some(source) = &self.secondary else {
            ui.label(egui::RichText::new("None — optional.").weak())
                .on_hover_text(
                    "A 2D release of the same film gives far better colour than the \
                     anaglyph can give itself, and if it is one of the eyes then that eye \
                     needs no reconstruction at all. See Help.",
                );
            return;
        };
        ui.label(
            egui::RichText::new(
                source
                    .path
                    .file_name()
                    .unwrap_or_default()
                    .to_string_lossy()
                    .to_string(),
            )
            .small(),
        );

        let before = self.secondary_role;
        egui::ComboBox::from_label("Use it as")
            .selected_text(self.secondary_role.label())
            .show_ui(ui, |ui| {
                for role in SecondaryRole::ALL {
                    ui.selectable_value(&mut self.secondary_role, role, role.label())
                        .on_hover_text(role.hint());
                }
            });
        if self.secondary_role != before {
            self.cache.invalidate();
        }
        ui.label(
            egui::RichText::new(self.secondary_role.hint())
                .small()
                .weak(),
        );

        let fps = self.secondary.as_ref().map_or(24.0, |s| s.info.fps);
        self.trim_row(ui, "Range", Role::Secondary, fps);
    }

    fn recovery_section(&mut self, ui: &mut egui::Ui) {
        Self::section(ui, "Recovery");
        ui.label(egui::RichText::new("Colour blur").strong())
            .on_hover_text(
                "How far colour is smeared to cover the disparity. Lower blurs harder; \
                 horizontal normally much harder than vertical.",
            );
        ui.add(
            egui::Slider::new(&mut self.params.decimate_horiz, 0.5..=100.0)
                .logarithmic(true)
                .text("Horizontal %"),
        );
        ui.add(
            egui::Slider::new(&mut self.params.decimate_vert, 0.5..=100.0)
                .logarithmic(true)
                .text("Vertical %"),
        );
        egui::ComboBox::from_label("Reconstruction")
            .selected_text(restore_name(self.params.restore))
            .show_ui(ui, |ui| {
                for r in [ColourRestore::Offset, ColourRestore::Scale] {
                    ui.selectable_value(&mut self.params.restore, r, restore_name(r))
                        .on_hover_text(restore_hint(r));
                }
            });

        ui.add_space(4.0);
        ui.label(egui::RichText::new("Ghosting").strong())
            .on_hover_text(
                "Removes each eye's ghost from the other. For cross-talk baked in by the \
                 master — it will not fix fringing caused by disparity.",
            );
        ui.add(
            egui::Slider::new(&mut self.params.leak_correct_left, -50.0..=50.0).text("Into left %"),
        );
        ui.add(
            egui::Slider::new(&mut self.params.leak_correct_right, -50.0..=50.0)
                .text("Into right %"),
        );

        ui.add_space(4.0);
        ui.label(egui::RichText::new("De-fringe").strong())
            .on_hover_text("Softens the peaking fringes on DVD-era transfers. 1.0 is off.");
        ui.add(egui::Slider::new(&mut self.params.defringe_left, 1.0..=8.0).text("Left"));
        ui.add(egui::Slider::new(&mut self.params.defringe_right, 1.0..=8.0).text("Right"));
    }

    fn grade_section(&mut self, ui: &mut egui::Ui) {
        Self::section(ui, "Grade");
        for (name, grade) in [
            ("Left eye", &mut self.params.grade_left),
            ("Right eye", &mut self.params.grade_right),
        ] {
            ui.label(egui::RichText::new(name).strong());
            ui.add(egui::Slider::new(&mut grade.brightness, -0.5..=0.5).text("Brightness"));
            ui.add(egui::Slider::new(&mut grade.contrast, 0.2..=2.5).text("Contrast"));
            ui.add(egui::Slider::new(&mut grade.saturation, 0.0..=2.5).text("Saturation"));
            ui.add_space(4.0);
        }
    }

    fn destination_section(&mut self, ui: &mut egui::Ui) {
        Self::section(ui, "Destination");
        egui::ComboBox::from_label("Write")
            .selected_text(self.params.layout.label())
            .show_ui(ui, |ui| {
                for l in OutputLayout::ALL {
                    ui.selectable_value(&mut self.params.layout, l, l.label());
                }
            });

        if self.params.layout == OutputLayout::Anaglyph {
            egui::ComboBox::from_label("Anaglyph colour mode")
                .selected_text(format_name(self.params.output_format))
                .show_ui(ui, |ui| {
                    for f in ANAGLYPH_FORMATS {
                        ui.selectable_value(&mut self.params.output_format, f, format_name(f))
                            .on_hover_text(output_format_hint(f));
                    }
                });
        }

        if matches!(
            self.params.layout,
            OutputLayout::SideBySide | OutputLayout::TopBottom
        ) {
            // "Left first" means nothing when the eyes are stacked vertically.
            let stacked = self.params.layout == OutputLayout::TopBottom;
            egui::ComboBox::from_label("Eye order")
                .selected_text(eye_order_name(self.params.eye_order, stacked))
                .show_ui(ui, |ui| {
                    for o in [EyeOrder::LeftFirst, EyeOrder::RightFirst] {
                        ui.selectable_value(
                            &mut self.params.eye_order,
                            o,
                            eye_order_name(o, stacked),
                        );
                    }
                });
        }
        ui.checkbox(&mut self.params.swap_eyes, "Swap eyes");

        if self.params.layout == OutputLayout::Separate {
            let names = match &self.output {
                Some(path) => describe_outputs(&output_paths(path, OutputLayout::Separate)),
                None => "…-left and …-right".to_string(),
            };
            ui.label(
                egui::RichText::new(format!(
                    "Two files are written. -left and -right are added to the name you \
                     choose:\n{names}"
                ))
                .color(ui.visuals().warn_fg_color),
            );
        }

        ui.add_space(6.0);
        egui::ComboBox::from_label("Codec")
            .selected_text(codec_name(self.encode.codec))
            .show_ui(ui, |ui| {
                for c in [
                    VideoCodec::H264VideoToolbox,
                    VideoCodec::HevcVideoToolbox,
                    VideoCodec::H264,
                    VideoCodec::Hevc,
                    VideoCodec::ProRes,
                ] {
                    ui.selectable_value(&mut self.encode.codec, c, codec_name(c));
                }
            });
        ui.add(egui::Slider::new(&mut self.encode.quality, 0..=100).text("Quality"));

        ui.add_space(8.0);
        if ui.button("Reset all settings").clicked() {
            self.params = ConvertParams::default();
            self.cache.invalidate();
        }
    }

    /// The manual. Kept in the app because the questions it answers come up
    /// while a film is open, not before.
    fn help_window(&mut self, ctx: &egui::Context) {
        let mut open = self.help_open;
        egui::Window::new("Help")
            .open(&mut open)
            .default_width(560.0)
            .default_height(620.0)
            .scroll([false, true])
            .show(ctx, |ui| {
                for (heading, body) in HELP {
                    ui.add_space(8.0);
                    ui.label(egui::RichText::new(*heading).strong().size(15.0));
                    ui.add_space(2.0);
                    ui.label(*body);
                }
                ui.add_space(12.0);
                ui.separator();
                ui.label(
                    egui::RichText::new(
                        "Stereoscopic Converter · GPL-3.0-or-later · bundles FFmpeg",
                    )
                    .weak()
                    .small(),
                );
            });
        self.help_open = open;
    }

    fn source_fps(&self) -> f64 {
        self.anaglyph.as_ref().map_or(24.0, |s| s.info.fps)
    }

    /// One source's start and finish, with a button to take the value from
    /// whatever the alignment view is currently showing.
    fn trim_row(&mut self, ui: &mut egui::Ui, name: &str, role: Role, fps: f64) {
        let (trim, source) = match role {
            Role::Anaglyph => (&mut self.params.anaglyph_trim, self.anaglyph.as_ref()),
            Role::Secondary => (&mut self.params.colour_trim, self.secondary.as_ref()),
            // Two per-eye files come from one master and start together.
            Role::RightEye => (&mut self.params.anaglyph_trim, self.right_eye.as_ref()),
        };
        let Some(source) = source else { return };
        let last = source
            .info
            .estimated_frame_count()
            .unwrap_or(1)
            .saturating_sub(1);

        ui.add_space(6.0);
        ui.label(egui::RichText::new(name).strong());
        ui.horizontal(|ui| {
            ui.label("Start");
            ui.add(
                egui::DragValue::new(&mut trim.start)
                    .range(0..=last)
                    .speed(1.0),
            );
            ui.label(egui::RichText::new(format_timecode(trim.start, fps)).weak());
        });

        let mut has_end = trim.end.is_some();
        ui.horizontal(|ui| {
            if ui.checkbox(&mut has_end, "End").changed() {
                trim.end = has_end.then_some(last);
            }
            if let Some(end) = &mut trim.end {
                ui.add(egui::DragValue::new(end).range(0..=last).speed(1.0));
                ui.label(egui::RichText::new(format_timecode(*end, fps)).weak());
            } else {
                ui.label(egui::RichText::new("to the end of the file").weak());
            }
        });

        // Aligning the anaglyph against itself would be meaningless.
        if role.is_secondary() {
            let active = self.aligning == Some(role);
            if ui
                .selectable_label(
                    active,
                    if active {
                        "Aligning — click to finish"
                    } else {
                        "Align to anaglyph…"
                    },
                )
                .on_hover_text(
                    "Shows this source beside the anaglyph so you can scrub each \
                     until they show the same moment, then mark it.",
                )
                .clicked()
            {
                self.aligning = if active { None } else { Some(role) };
                self.align_frame = self.frame;
                self.cache.invalidate();
            }
        }
    }

    /// The two raw frames being compared while aligning, side by side.
    fn alignment_pane(&mut self, ui: &mut egui::Ui, role: Role, ctx: &egui::Context) {
        let (Some(tools), Some(anaglyph)) = (self.tools().cloned(), self.anaglyph.as_ref()) else {
            return;
        };
        let Some(other) = self
            .source_for(role)
            .map(|s| (s.path.clone(), s.info.clone()))
        else {
            self.aligning = None;
            return;
        };
        let anaglyph = (anaglyph.path.clone(), anaglyph.info.clone());

        let width = (ui.available_width() / 2.0 - 12.0).max(64.0);
        let shot = |path: &Path, info: &ana_media::VideoInfo, frame: u64| {
            grab_frame(&tools, path, info, frame)
                .map(|f| downscale(&f, (width / info.width as f32).clamp(0.05, 1.0)))
                .map_err(|e| e.to_string())
        };

        ui.add_space(4.0);
        ui.label(
            egui::RichText::new(
                "Scrub each side to the same visual moment — a cut works best — then mark it.",
            )
            .weak(),
        );
        ui.add_space(4.0);

        let mut marked = false;
        ui.horizontal(|ui| {
            ui.vertical(|ui| {
                ui.label(egui::RichText::new("Anaglyph").strong());
                let last = anaglyph
                    .1
                    .estimated_frame_count()
                    .unwrap_or(1)
                    .saturating_sub(1);
                ui.add(egui::Slider::new(&mut self.frame, 0..=last).text("frame"));
                ui.label(format_timecode(self.frame, anaglyph.1.fps));
                match shot(&anaglyph.0, &anaglyph.1, self.frame) {
                    Ok(f) => show_frame(ui, ctx, "align_a", &f, anaglyph.1.display_aspect()),
                    Err(e) => {
                        ui.colored_label(egui::Color32::from_rgb(230, 120, 90), e);
                    }
                }
            });
            ui.separator();
            ui.vertical(|ui| {
                ui.label(egui::RichText::new(role.label()).strong());
                let last = other
                    .1
                    .estimated_frame_count()
                    .unwrap_or(1)
                    .saturating_sub(1);
                ui.add(egui::Slider::new(&mut self.align_frame, 0..=last).text("frame"));
                ui.label(format_timecode(self.align_frame, other.1.fps));
                match shot(&other.0, &other.1, self.align_frame) {
                    Ok(f) => show_frame(ui, ctx, "align_b", &f, other.1.display_aspect()),
                    Err(e) => {
                        ui.colored_label(egui::Color32::from_rgb(230, 120, 90), e);
                    }
                }
            });
        });

        ui.add_space(6.0);
        ui.horizontal(|ui| {
            if ui
                .button("Mark this as the start of both")
                .on_hover_text("These two frames are the same moment.")
                .clicked()
            {
                self.params.anaglyph_trim.start = self.frame;
                self.trim_for(role).start = self.align_frame;
                marked = true;
            }
            if ui.button("Mark this as the end of both").clicked() {
                self.params.anaglyph_trim.end = Some(self.frame);
                self.trim_for(role).end = Some(self.align_frame);
                marked = true;
            }
            if ui.button("Clear range").clicked() {
                self.params.anaglyph_trim = SourceTrim::whole();
                *self.trim_for(role) = SourceTrim::whole();
                marked = true;
            }
            let shift = self.align_frame as i64 - self.frame as i64;
            ui.label(
                egui::RichText::new(format!("offset {shift:+} frames"))
                    .weak()
                    .small(),
            );
        });
        if marked {
            self.cache.invalidate();
        }
        ui.add_space(6.0);
    }

    fn source_for(&self, role: Role) -> Option<&Source> {
        match role {
            Role::Anaglyph => self.anaglyph.as_ref(),
            Role::Secondary => self.secondary.as_ref(),
            Role::RightEye => self.right_eye.as_ref(),
        }
    }

    fn trim_for(&mut self, role: Role) -> &mut SourceTrim {
        match role {
            Role::Anaglyph | Role::RightEye => &mut self.params.anaglyph_trim,
            Role::Secondary => &mut self.params.colour_trim,
        }
    }

    fn bottom_bar(&mut self, ui: &mut egui::Ui) {
        egui::Panel::bottom("transport").show(ui, |ui| {
            ui.add_space(6.0);
            ui.label(
                egui::RichText::new("PREVIEW")
                    .strong()
                    .size(17.0)
                    .color(SECTION_COLOUR),
            );
            ui.separator();
            let last = self.frame_count().saturating_sub(1);
            ui.add_enabled(
                self.anaglyph.is_some(),
                egui::Slider::new(&mut self.frame, 0..=last).text("Frame"),
            );

            ui.horizontal_wrapped(|ui| {
                for mode in ViewMode::ALL {
                    let changed = ui
                        .selectable_label(self.view == mode, mode.label())
                        .on_hover_text(mode.hint())
                        .clicked();
                    if changed && self.view != mode {
                        self.view = mode;
                        // The pair is unchanged; only its presentation is.
                        self.texture = None;
                    }
                }
                ui.separator();
                if let Some(ms) = self.last_process {
                    ui.label(
                        egui::RichText::new(format!(
                            "{ms:.0} ms/frame at {:.0}%",
                            self.preview_scale * 100.0
                        ))
                        .weak()
                        .small(),
                    );
                }
            });

            ui.add_space(4.0);
            ui.horizontal_wrapped(|ui| match &self.running {
                Some(run) => {
                    if ui.button("Stop").clicked() {
                        run.cancel();
                    }
                    for note in run.notes() {
                        ui.label(egui::RichText::new(note).weak().small());
                    }
                    if run.is_cancelling() {
                        ui.label("stopping after this frame…");
                    }
                    match (run.fraction(), run.rate(), run.eta()) {
                        (Some(f), Some(rate), Some(eta)) => {
                            ui.add(
                                egui::ProgressBar::new(f)
                                    .desired_width(220.0)
                                    .show_percentage(),
                            );
                            ui.label(format!(
                                "{} / {}  ·  {rate:.1} fps  ·  {} left",
                                run.frames_done(),
                                run.total_frames().unwrap_or_default(),
                                format_duration(eta)
                            ));
                        }
                        _ => {
                            ui.spinner();
                            ui.label(format!("{} frames", run.frames_done()));
                        }
                    }
                }
                None => {
                    if ui
                        .add_enabled(self.anaglyph.is_some(), egui::Button::new("Convert…"))
                        .clicked()
                    {
                        if let Some(path) = pick_save(self.output.as_deref()) {
                            self.output = Some(path);
                            self.start_render();
                        }
                    }
                    if let Some(out) = &self.output {
                        let paths = output_paths(out, self.params.layout);
                        ui.label(
                            egui::RichText::new(format!("→ {}", describe_outputs(&paths)))
                                .weak()
                                .small(),
                        );
                    }
                }
            });

            if let Some(text) = &self.outcome {
                ui.label(egui::RichText::new(text).strong());
            }
            if let Some(text) = &self.problem {
                ui.colored_label(egui::Color32::from_rgb(230, 120, 90), text);
            }
            ui.add_space(6.0);
        });
    }

    fn preview_pane(&mut self, ui: &mut egui::Ui) {
        let ctx = ui.ctx().clone();
        egui::CentralPanel::default().show(ui, |ui| {
            if let Some(role) = self.aligning {
                self.alignment_pane(ui, role, &ctx);
                return;
            }
            let available = ui.available_size();
            if self.anaglyph.is_some() {
                // Side-by-side shows two frames, so each gets half the width.
                let per_eye = if self.view == ViewMode::SideBySide {
                    available.x / 2.0
                } else {
                    available.x
                };
                self.refresh_preview(&ctx, per_eye);
            }

            match &self.texture {
                Some(texture) => {
                    paint_preview(
                        ui,
                        available,
                        texture.size_vec2().y,
                        self.preview_display_aspect() as f32,
                        Some(texture),
                    );
                }
                None => {
                    ui.centered_and_justified(|ui| {
                        ui.label(
                            egui::RichText::new(if self.tools.is_err() {
                                "ffmpeg was not found — install it, then restart."
                            } else {
                                "Open an anaglyph film to begin."
                            })
                            .weak(),
                        );
                    });
                }
            }
        });
    }

    fn load_preset(&mut self) {
        let Some(path) = rfd::FileDialog::new()
            .add_filter("preset", &["json"])
            .pick_file()
        else {
            return;
        };
        match std::fs::read_to_string(&path)
            .map_err(|e| e.to_string())
            .and_then(|text| {
                serde_json::from_str::<ConvertParams>(&text).map_err(|e| e.to_string())
            }) {
            Ok(params) => {
                self.params = params;
                self.problem = None;
                self.outcome = Some(format!("loaded {}", path.display()));
            }
            Err(e) => self.problem = Some(format!("{}: {e}", path.display())),
        }
    }

    fn save_preset(&mut self) {
        let Some(path) = rfd::FileDialog::new()
            .add_filter("preset", &["json"])
            .set_file_name("preset.json")
            .save_file()
        else {
            return;
        };
        let written = serde_json::to_string_pretty(&self.params)
            .map_err(|e| e.to_string())
            .and_then(|json| std::fs::write(&path, json + "\n").map_err(|e| e.to_string()));
        match written {
            Ok(()) => self.outcome = Some(format!("saved {}", path.display())),
            Err(e) => self.problem = Some(format!("{}: {e}", path.display())),
        }
    }
}

fn pick_video() -> Option<PathBuf> {
    rfd::FileDialog::new()
        .add_filter(
            "video",
            &[
                "mkv", "mp4", "avi", "m4v", "mov", "mpg", "mpeg", "wmv", "ts",
            ],
        )
        .pick_file()
}

fn pick_save(current: Option<&Path>) -> Option<PathBuf> {
    let mut dialog = rfd::FileDialog::new().add_filter("video", &["mkv", "mp4", "mov"]);
    if let Some(path) = current {
        if let Some(name) = path.file_name() {
            dialog = dialog.set_file_name(name.to_string_lossy());
        }
        if let Some(dir) = path.parent() {
            dialog = dialog.set_directory(dir);
        }
    }
    dialog.save_file()
}

fn format_name(f: AnaglyphFormat) -> &'static str {
    match f {
        AnaglyphFormat::RedCyan => "Red / cyan",
        AnaglyphFormat::GreenMagenta => "Green / magenta",
        AnaglyphFormat::RedBlue => "Red / blue",
    }
}

fn transfer_name(t: TransferFunction) -> &'static str {
    match t {
        TransferFunction::Srgb => "sRGB",
        TransferFunction::Bt709 => "BT.709",
        TransferFunction::Linear => "Linear",
    }
}

fn restore_name(r: ColourRestore) -> &'static str {
    match r {
        ColourRestore::Offset => "Offset (recommended)",
        ColourRestore::Scale => "Scale",
    }
}

fn restore_hint(r: ColourRestore) -> &'static str {
    match r {
        ColourRestore::Offset => "Robust on real film. Never divides, so noisy shadows stay clean.",
        ColourRestore::Scale => {
            "Sharper colour on a clean source, but breaks dark areas into cyan speckle on \
             grainy or heavily compressed film."
        }
    }
}

fn codec_name(c: VideoCodec) -> &'static str {
    match c {
        VideoCodec::H264VideoToolbox => "H.264 (hardware)",
        VideoCodec::HevcVideoToolbox => "HEVC (hardware)",
        VideoCodec::H264 => "H.264 (software)",
        VideoCodec::Hevc => "HEVC (software)",
        VideoCodec::ProRes => "ProRes HQ",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn eye_order_reads_left_and_right_when_side_by_side() {
        assert_eq!(
            eye_order_name(EyeOrder::LeftFirst, false),
            "Left eye on the left"
        );
        assert_eq!(
            eye_order_name(EyeOrder::RightFirst, false),
            "Right eye on the left"
        );
    }

    #[test]
    fn eye_order_reads_top_and_bottom_when_stacked() {
        // "Left first" says nothing about a vertical stack.
        assert_eq!(eye_order_name(EyeOrder::LeftFirst, true), "Left eye on top");
        assert_eq!(
            eye_order_name(EyeOrder::RightFirst, true),
            "Right eye on top"
        );
    }

    #[test]
    fn every_eye_order_label_names_a_position() {
        for stacked in [false, true] {
            for order in [EyeOrder::LeftFirst, EyeOrder::RightFirst] {
                let label = eye_order_name(order, stacked);
                let position = if stacked { "top" } else { "left" };
                assert!(
                    label.contains(position),
                    "{label:?} should say where the eye goes for stacked={stacked}"
                );
            }
        }
    }

    #[test]
    fn a_colour_only_2d_source_replaces_neither_eye() {
        assert_eq!(SecondaryRole::ColourOnly.mono_eye(), MonoEye::None);
        assert!(!SecondaryRole::ColourOnly.supplies_an_eye());
    }

    #[test]
    fn a_2d_source_that_is_an_eye_replaces_that_eye() {
        assert_eq!(SecondaryRole::IsLeftEye.mono_eye(), MonoEye::Left);
        assert_eq!(SecondaryRole::IsRightEye.mono_eye(), MonoEye::Right);
        assert!(SecondaryRole::IsLeftEye.supplies_an_eye());
        assert!(SecondaryRole::IsRightEye.supplies_an_eye());
    }

    #[test]
    fn every_2d_role_explains_itself() {
        for role in SecondaryRole::ALL {
            assert!(!role.label().is_empty(), "{role:?} needs a label");
            assert!(
                role.hint().len() > 40,
                "{role:?} needs an explanation worth reading"
            );
        }
    }

    #[test]
    fn the_first_dropped_file_becomes_the_film() {
        assert_eq!(role_for_drop(false), DropTarget::Source);
    }

    #[test]
    fn a_later_dropped_file_becomes_the_2d_release() {
        // Someone who already has a film open and drops another is adding the
        // 2D version of it; what that is for is chosen afterwards.
        assert_eq!(role_for_drop(true), DropTarget::Secondary);
    }

    #[test]
    fn video_files_are_recognised_whatever_the_case() {
        for name in [
            "a.mkv", "B.MP4", "c.Mov", "d.avi", "e.m4v", "f.mpg", "g.ts", "h.wmv",
        ] {
            assert!(
                looks_like_video(Path::new(name)),
                "{name} should be accepted"
            );
        }
    }

    #[test]
    fn other_files_are_not_mistaken_for_video() {
        // Dropping a preset or a stray screenshot should not replace the film.
        for name in ["preset.json", "notes.txt", "frame.png", "noextension"] {
            assert!(
                !looks_like_video(Path::new(name)),
                "{name} should be ignored"
            );
        }
    }

    #[test]
    fn a_picture_is_drawn_at_its_display_shape_not_its_pixel_shape() {
        // The regression. A 1280x576 side-by-side frame with 8:5 pixels is
        // meant to be seen at 32:9; drawn at its pixel count it comes out
        // 2.22:1 and everybody in it is too narrow.
        let size = fitted_size(egui::vec2(600.0, 400.0), 576.0, 32.0 / 9.0);
        assert!(
            (size.x / size.y - 32.0 / 9.0).abs() < 1e-3,
            "expected a 3.556:1 area, got {:.3}:1 ({}x{})",
            size.x / size.y,
            size.x,
            size.y
        );
    }

    #[test]
    fn the_drawn_area_always_fits_the_space_offered() {
        for available in [
            egui::vec2(600.0, 400.0),
            egui::vec2(100.0, 900.0),
            egui::vec2(2000.0, 50.0),
        ] {
            for aspect in [0.5, 1.0, 1.7778, 3.5556, 7.1111] {
                let size = fitted_size(available, 576.0, aspect);
                assert!(
                    size.x <= available.x + 0.01 && size.y <= available.y + 0.01,
                    "{size:?} does not fit {available:?} at {aspect}"
                );
                assert!(
                    (size.x / size.y - aspect).abs() < 1e-3,
                    "shape drifted to {} at {aspect}",
                    size.x / size.y
                );
            }
        }
    }

    #[test]
    fn a_wide_picture_in_a_tall_space_is_limited_by_width() {
        let size = fitted_size(egui::vec2(600.0, 4000.0), 576.0, 4.0);
        assert!(
            (size.x - 600.0).abs() < 0.01,
            "should use the full width, got {}",
            size.x
        );
    }

    #[test]
    fn a_tall_picture_in_a_wide_space_is_limited_by_height() {
        let size = fitted_size(egui::vec2(4000.0, 300.0), 576.0, 0.5);
        assert!(
            (size.y - 300.0).abs() < 0.01,
            "should use the full height, got {}",
            size.y
        );
    }

    #[test]
    fn a_nonsense_aspect_does_not_produce_a_nonsense_area() {
        for bad in [0.0, -2.0, f32::NAN, f32::INFINITY] {
            let size = fitted_size(egui::vec2(600.0, 400.0), 576.0, bad);
            assert!(
                size.x.is_finite() && size.y.is_finite() && size.x > 0.0 && size.y > 0.0,
                "aspect {bad} produced {size:?}"
            );
        }
    }

    #[test]
    fn the_default_output_never_lands_on_the_input() {
        let input = Path::new("/films/comin-at-ya.mkv");
        let output = default_output_for(input);
        assert_ne!(output, input);
        assert_eq!(output, Path::new("/films/comin-at-ya-stereo.mkv"));
    }

    #[test]
    fn the_default_output_keeps_a_dotted_name_intact() {
        assert_eq!(
            default_output_for(Path::new("/f/my.film.1981.mp4")),
            Path::new("/f/my.film.1981-stereo.mp4")
        );
    }

    #[test]
    fn an_extensionless_input_still_gets_a_container() {
        assert_eq!(
            default_output_for(Path::new("/f/movie")),
            Path::new("/f/movie-stereo.mkv")
        );
    }

    #[test]
    fn downscaling_at_full_size_is_a_no_op() {
        let frame = FrameF32::new_rgb(8, 6);
        let out = downscale(&frame, 1.0);
        assert_eq!((out.width(), out.height()), (8, 6));
    }

    #[test]
    fn downscaling_halves_the_dimensions() {
        let out = downscale(&FrameF32::new_rgb(80, 60), 0.5);
        assert_eq!((out.width(), out.height()), (40, 30));
    }

    #[test]
    fn downscaling_always_lands_on_an_even_size() {
        // A packed source is split down the middle, and an odd frame has no
        // middle. Keeping preview frames even stops the preview and the render
        // disagreeing about which column is the seam.
        for scale in [0.13, 0.27, 0.41, 0.5, 0.63, 0.77, 0.91] {
            let out = downscale(&FrameF32::new_rgb(1280, 576), scale);
            assert_eq!(out.width() % 2, 0, "width {} at {scale}", out.width());
            assert_eq!(out.height() % 2, 0, "height {} at {scale}", out.height());
        }
    }

    #[test]
    fn downscaling_never_collapses_a_frame_to_nothing() {
        // A tiny source in a large window must still produce something the
        // texture upload can accept.
        let out = downscale(&FrameF32::new_rgb(4, 4), 0.01);
        assert!(
            out.width() >= 2 && out.height() >= 2,
            "got {}x{}",
            out.width(),
            out.height()
        );
    }

    #[test]
    fn every_enum_the_ui_offers_has_a_name() {
        for f in [
            AnaglyphFormat::RedCyan,
            AnaglyphFormat::GreenMagenta,
            AnaglyphFormat::RedBlue,
        ] {
            assert!(!format_name(f).is_empty());
        }
        for c in [
            VideoCodec::H264VideoToolbox,
            VideoCodec::HevcVideoToolbox,
            VideoCodec::H264,
            VideoCodec::Hevc,
            VideoCodec::ProRes,
        ] {
            assert!(!codec_name(c).is_empty());
        }
        for l in OutputLayout::ALL {
            assert!(!l.label().is_empty());
        }
        for r in [ColourRestore::Offset, ColourRestore::Scale] {
            assert!(!restore_name(r).is_empty() && !restore_hint(r).is_empty());
        }
    }
}

/// The source-type choice as a flat list, since [`InputMode`] carries data the
/// combo box should not have to reconstruct on every frame.
#[derive(Clone, Copy, PartialEq, Eq)]
enum SourceKind {
    Anaglyph,
    SideBySide,
    TopBottom,
    TwoFiles,
}

impl SourceKind {
    const ALL: [SourceKind; 4] = [
        Self::Anaglyph,
        Self::SideBySide,
        Self::TopBottom,
        Self::TwoFiles,
    ];

    fn of(input: ana_core::params::InputMode) -> Self {
        use ana_core::packed::StereoPacking;
        use ana_core::params::InputMode;
        match input {
            InputMode::Anaglyph => Self::Anaglyph,
            InputMode::Packed {
                packing: StereoPacking::SideBySide,
                ..
            } => Self::SideBySide,
            InputMode::Packed {
                packing: StereoPacking::TopBottom,
                ..
            } => Self::TopBottom,
            InputMode::TwoFiles => Self::TwoFiles,
        }
    }

    fn to_input(self, anamorphic: bool) -> ana_core::params::InputMode {
        use ana_core::packed::StereoPacking;
        use ana_core::params::InputMode;
        match self {
            Self::Anaglyph => InputMode::Anaglyph,
            Self::SideBySide => InputMode::packed(StereoPacking::SideBySide, anamorphic),
            Self::TopBottom => InputMode::packed(StereoPacking::TopBottom, anamorphic),
            Self::TwoFiles => InputMode::TwoFiles,
        }
    }

    fn label(self) -> &'static str {
        match self {
            Self::Anaglyph => "An anaglyph",
            Self::SideBySide => "A side-by-side pair",
            Self::TopBottom => "A top-and-bottom pair",
            Self::TwoFiles => "One eye, with the other in a second file",
        }
    }

    fn hint(self) -> &'static str {
        match self {
            Self::Anaglyph => "Recover a full-colour stereo pair from red/cyan or green/magenta",
            Self::SideBySide => "Already a stereo pair, two eyes across each frame",
            Self::TopBottom => "Already a stereo pair, two eyes down each frame",
            Self::TwoFiles => "This file is the left eye; choose the right eye separately",
        }
    }
}
