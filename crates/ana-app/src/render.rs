// Ana-Convert — anaglyph 3D to full-colour stereo recovery
// SPDX-License-Identifier: GPL-3.0-or-later

//! Running a conversion without freezing the window.
//!
//! A feature-length render takes tens of minutes, so it happens on a worker
//! thread and reports back through a channel. The UI drains that channel once
//! per repaint, which keeps the progress bar honest without any locking on the
//! hot path.

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{channel, Receiver};
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use ana_media::FfmpegTools;
use ana_pipeline::{render, Progress, RenderJob, RenderSummary};

/// A render in flight.
pub struct RunningRender {
    events: Receiver<Progress>,
    notes: Vec<String>,
    cancel: Arc<AtomicBool>,
    handle: Option<JoinHandle<Result<RenderSummary, String>>>,
    started: Instant,
    done: u64,
    total: Option<u64>,
}

/// How a finished render turned out.
pub enum Finished {
    Succeeded {
        summary: RenderSummary,
        elapsed: Duration,
    },
    Failed(String),
}

impl RunningRender {
    /// Starts a render on a worker thread.
    pub fn start(tools: FfmpegTools, job: RenderJob) -> Self {
        let (tx, events) = channel();
        let cancel = Arc::new(AtomicBool::new(false));
        let thread_cancel = Arc::clone(&cancel);

        let handle = std::thread::spawn(move || {
            // A closed receiver means the window is gone; the send failing is
            // not a reason to stop, so the result is deliberately ignored.
            let mut report = |p: Progress| {
                let _ = tx.send(p);
            };
            render(&tools, &job, &mut report, &thread_cancel).map_err(|e| e.to_string())
        });

        Self {
            events,
            notes: Vec::new(),
            cancel,
            handle: Some(handle),
            started: Instant::now(),
            done: 0,
            total: None,
        }
    }

    /// Takes in whatever the worker has reported since the last repaint.
    pub fn pump(&mut self) {
        while let Ok(progress) = self.events.try_recv() {
            match progress {
                Progress::Started { total_frames, .. } => self.total = total_frames,
                Progress::Frame { done, total } => {
                    self.done = done;
                    self.total = total;
                }
                Progress::Note(message) => self.notes.push(message),
                Progress::Finished { frames, .. } => self.done = frames,
            }
        }
    }

    /// Asks the worker to stop after the frame it is on.
    pub fn cancel(&self) {
        self.cancel.store(true, Ordering::SeqCst);
    }

    pub fn is_cancelling(&self) -> bool {
        self.cancel.load(Ordering::SeqCst)
    }

    /// Anything the conversion wanted to mention, such as a source it had to
    /// reshape to fit.
    pub fn notes(&self) -> &[String] {
        &self.notes
    }

    pub fn frames_done(&self) -> u64 {
        self.done
    }

    pub fn total_frames(&self) -> Option<u64> {
        self.total
    }

    /// Frames per second so far, or `None` before the first frame lands.
    pub fn rate(&self) -> Option<f64> {
        let seconds = self.started.elapsed().as_secs_f64();
        (self.done > 0 && seconds > 0.0).then(|| self.done as f64 / seconds)
    }

    /// Fraction complete, when the length is known.
    pub fn fraction(&self) -> Option<f32> {
        let total = self.total?;
        (total > 0).then(|| (self.done as f32 / total as f32).clamp(0.0, 1.0))
    }

    /// Estimated seconds remaining.
    pub fn eta(&self) -> Option<Duration> {
        let (total, rate) = (self.total?, self.rate()?);
        (rate > 0.0).then(|| Duration::from_secs_f64(total.saturating_sub(self.done) as f64 / rate))
    }

    /// The outcome, once the worker has stopped. `None` while it is still running.
    pub fn take_result(&mut self) -> Option<Finished> {
        if !self.handle.as_ref()?.is_finished() {
            return None;
        }
        // Drain anything still queued before reporting, so the final frame
        // count is not one repaint out of date.
        self.pump();
        let elapsed = self.started.elapsed();
        match self.handle.take()?.join() {
            Ok(Ok(summary)) => Some(Finished::Succeeded { summary, elapsed }),
            Ok(Err(message)) => Some(Finished::Failed(message)),
            Err(_) => Some(Finished::Failed("the render thread panicked".into())),
        }
    }
}

/// Turns a duration into something readable at a glance.
pub fn format_duration(d: Duration) -> String {
    let total = d.as_secs();
    let (h, m, s) = (total / 3600, (total % 3600) / 60, total % 60);
    if h > 0 {
        format!("{h}h{m:02}m")
    } else if m > 0 {
        format!("{m}m{s:02}s")
    } else {
        format!("{s}s")
    }
}

/// Where a render's output should go, given the chosen name.
pub fn describe_outputs(outputs: &[PathBuf]) -> String {
    match outputs {
        [] => "nothing".into(),
        [one] => one.display().to_string(),
        many => many
            .iter()
            .filter_map(|p| p.file_name())
            .map(|n| n.to_string_lossy().into_owned())
            .collect::<Vec<_>>()
            .join(" and "),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn durations_read_naturally_at_every_scale() {
        assert_eq!(format_duration(Duration::from_secs(9)), "9s");
        assert_eq!(format_duration(Duration::from_secs(95)), "1m35s");
        assert_eq!(format_duration(Duration::from_secs(3725)), "1h02m");
    }

    #[test]
    fn a_single_output_is_named_in_full() {
        let outputs = vec![PathBuf::from("/films/out.mkv")];
        assert_eq!(describe_outputs(&outputs), "/films/out.mkv");
    }

    #[test]
    fn a_pair_of_outputs_is_named_by_file() {
        let outputs = vec![
            PathBuf::from("/films/out-left.mkv"),
            PathBuf::from("/films/out-right.mkv"),
        ];
        assert_eq!(describe_outputs(&outputs), "out-left.mkv and out-right.mkv");
    }

    #[test]
    fn no_outputs_is_stated_rather_than_blank() {
        assert_eq!(describe_outputs(&[]), "nothing");
    }
}
