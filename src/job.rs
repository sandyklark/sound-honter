//! Runs a whole user request, reporting progress through state shared with the UI.

use crate::engine::{Engine, Failure, Format, MixOptions};
use crate::queue::Runner;
use crate::settings::Settings;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

pub const NEED_TWO_FILES: &str = "Select at least two audio files to mix.";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Job {
    Convert(Format),
    Normalize,
    Trim,
    Mix(MixOptions),
}

impl Job {
    /// Identifies this job's queue. Each kind of job gathers its own selection.
    pub fn queue_name(self) -> String {
        match self {
            Job::Convert(format) => format!("convert-{}", format.extension()),
            Job::Normalize => "normalize".to_string(),
            Job::Trim => "trim".to_string(),
            Job::Mix(options) => {
                let level = if options.normalize {
                    "normalized"
                } else {
                    "plain"
                };
                format!("mix-{}-{level}", options.format.extension())
            }
        }
    }
}

pub enum Source {
    Files(Vec<PathBuf>),
    Queue(Runner),
}

#[derive(Clone, Default)]
pub struct Status {
    pub heading: String,
    pub detail: String,
    pub fraction: f64,
}

#[derive(Default)]
pub struct Shared {
    pub cancel: Arc<AtomicBool>,
    pub status: Mutex<Status>,
    pub finished: AtomicBool,
}

impl Shared {
    fn report(&self, heading: &str, detail: &str, fraction: f64) {
        *self.status.lock().unwrap() = Status {
            heading: heading.to_string(),
            detail: detail.to_string(),
            fraction,
        };
    }

    fn is_cancelled(&self) -> bool {
        self.cancel.load(Ordering::Relaxed)
    }
}

#[derive(Debug, Default)]
pub struct Outcome {
    pub outputs: Vec<PathBuf>,
    /// What failed, and why.
    pub failures: Vec<(String, String)>,
    pub cancelled: bool,
}

impl Outcome {
    fn record(&mut self, result: Result<PathBuf, Failure>, label: String) {
        match result {
            Ok(output) => self.outputs.push(output),
            Err(Failure::Cancelled) => self.cancelled = true,
            Err(Failure::Error(reason)) => self.failures.push((label, reason)),
        }
    }
}

fn display_name(path: &Path) -> String {
    path.file_name()
        .unwrap_or(path.as_os_str())
        .to_string_lossy()
        .into_owned()
}

struct Worker<'a> {
    job: Job,
    engine: Engine,
    shared: &'a Shared,
    outcome: Outcome,
    processed: usize,
}

impl Worker<'_> {
    fn handle_batch(&mut self, batch: Vec<PathBuf>) {
        match self.job {
            Job::Mix(options) => self.mix(&batch, options),
            job => self.process_each(&batch, job),
        }
    }

    fn process_each(&mut self, batch: &[PathBuf], job: Job) {
        let verb = match job {
            Job::Normalize => "Normalizing",
            Job::Trim => "Trimming",
            _ => "Converting",
        };
        // Files selected while a batch is running extend the same count.
        let total = self.processed + batch.len();
        for input in batch {
            if self.shared.is_cancelled() {
                self.outcome.cancelled = true;
                return;
            }
            let heading = format!("{verb} {} of {total}", self.processed + 1);
            let name = display_name(input);
            let finished = self.processed as f64;
            let shared = self.shared;
            let mut on_progress = |file_fraction: f64| {
                shared.report(&heading, &name, (finished + file_fraction) / total as f64);
            };
            on_progress(0.0);
            let result = match job {
                Job::Convert(format) => self.engine.convert(input, format, &mut on_progress),
                Job::Normalize => self.engine.normalize(input, &mut on_progress),
                Job::Trim => self.engine.trim(input, &mut on_progress),
                Job::Mix(_) => unreachable!("mixes are handled as a whole"),
            };
            self.outcome.record(result, name);
            self.processed += 1;
        }
    }

    fn mix(&mut self, batch: &[PathBuf], options: MixOptions) {
        if batch.len() < 2 {
            let label = batch.first().map_or_else(String::new, |p| display_name(p));
            self.outcome
                .failures
                .push((label, NEED_TWO_FILES.to_string()));
            return;
        }
        let heading = format!("Mixing {} files", batch.len());
        let names: Vec<String> = batch.iter().map(|p| display_name(p)).collect();
        let detail = names.join(", ");
        let shared = self.shared;
        let mut on_progress = |fraction: f64| shared.report(&heading, &detail, fraction);
        on_progress(0.0);
        let result = self.engine.mix(batch, options, &mut on_progress);
        self.outcome.record(result, detail);
    }
}

pub fn run(
    job: Job,
    source: Source,
    ffmpeg: PathBuf,
    settings: Settings,
    shared: &Shared,
) -> Outcome {
    let mut worker = Worker {
        job,
        engine: Engine::new(ffmpeg, shared.cancel.clone()).with_settings(settings),
        shared,
        outcome: Outcome::default(),
        processed: 0,
    };
    match source {
        Source::Files(files) => worker.handle_batch(files),
        Source::Queue(mut runner) => loop {
            // Keep draining after a cancel, so queued files aren't picked up by some later run.
            match runner.next_batch() {
                Ok(Some(_)) if shared.is_cancelled() => worker.outcome.cancelled = true,
                Ok(Some(batch)) => worker.handle_batch(batch),
                Ok(None) => break,
                Err(error) => {
                    let failure = ("Selection".to_string(), error.to_string());
                    worker.outcome.failures.push(failure);
                    break;
                }
            }
        },
    }
    worker.outcome
}
