//! Sound Honter: convert and mix audio files from the Windows right-click menu.

#![cfg_attr(not(test), windows_subsystem = "windows")]

mod settings_window;
mod ui;

use soundhonter::engine::{Engine, Format, MixOptions};
use soundhonter::job::{self, Job, Outcome, Shared, Source};
use soundhonter::queue;
use soundhonter::settings::Settings;
use std::ffi::OsString;
use std::os::windows::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};
use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::{fs, thread};

const USAGE: &str = "\
Usage:
  soundhonter convert <mp3|wav|flac|ogg|m4a> [--queue] [--no-ui] <files...>
  soundhonter normalize [--queue] [--no-ui] <files...>
  soundhonter trim [--queue] [--no-ui] <files...>
  soundhonter mix [--format <format>] [--no-normalize] [--ask] [--queue] [--no-ui] <files...>
  soundhonter settings

  --queue         Gather files from separate launches into one job (used by the right-click menu)
  --no-ui         Report on stdout and stderr instead of showing dialogs
  --format        The format to save a mix in (default: wav)
  --no-normalize  Keep a mix's natural level, turning it down only if it would clip
  --ask           Ask how to mix, remembering the answer for next time";

const EXIT_FAILED: u8 = 1;
const EXIT_USAGE: u8 = 2;

const ID_SHOW_FILE: i32 = 1001;
const ID_LICENSES: i32 = 1002;
const ID_SETTINGS: i32 = 1003;

/// Queues `--ask` launches apart from others, since the options aren't known until after gathering.
const ASKING_QUEUE: &str = "mix-ask";

struct Request {
    job: Job,
    use_queue: bool,
    no_ui: bool,
    ask: bool,
    files: Vec<PathBuf>,
}

fn parse_args(args: Vec<OsString>) -> Result<Request, String> {
    let mut args = args.into_iter();
    let job = match args.next().as_deref().and_then(|a| a.to_str()) {
        Some("mix") => Job::Mix(MixOptions::default()),
        Some("normalize") => Job::Normalize,
        Some("trim") => Job::Trim,
        Some("convert") => {
            let name = args.next().ok_or("Convert needs a format.")?;
            let name = name.to_string_lossy();
            let format = Format::parse(&name).ok_or_else(|| format!("Unknown format: {name}"))?;
            Job::Convert(format)
        }
        Some(other) => return Err(format!("Unknown command: {other}")),
        None => return Err("Missing command.".to_string()),
    };
    let (mut use_queue, mut no_ui, mut ask, mut files) = (false, false, false, Vec::new());
    let mut mix_options = MixOptions::default();
    let mut used_mix_flag = false;
    while let Some(arg) = args.next() {
        match arg.to_str() {
            Some("--queue") => use_queue = true,
            Some("--no-ui") => no_ui = true,
            Some("--ask") => {
                ask = true;
                used_mix_flag = true;
            }
            Some("--no-normalize") => {
                mix_options.normalize = false;
                used_mix_flag = true;
            }
            Some("--format") => {
                let name = args.next().ok_or("--format needs a format.")?;
                let name = name.to_string_lossy();
                mix_options.format =
                    Format::parse(&name).ok_or_else(|| format!("Unknown format: {name}"))?;
                used_mix_flag = true;
            }
            _ => files.push(PathBuf::from(arg)),
        }
    }
    let job = match job {
        Job::Mix(_) => Job::Mix(mix_options),
        _ if used_mix_flag => return Err("Those options only apply to mix.".to_string()),
        other => other,
    };
    if files.is_empty() {
        return Err("No files were given.".to_string());
    }
    // Explorer passes full paths, but a command line might not.
    let files = files
        .into_iter()
        .map(|f| std::path::absolute(&f).unwrap_or(f))
        .collect();
    Ok(Request {
        job,
        use_queue,
        no_ui,
        ask,
        files,
    })
}

fn data_dir() -> PathBuf {
    if let Some(dir) = std::env::var_os("SOUNDHONTER_DATA_DIR") {
        return dir.into();
    }
    let base = std::env::var_os("LOCALAPPDATA").unwrap_or_else(|| std::env::temp_dir().into());
    Path::new(&base).join("Sound Honter")
}

fn main() -> ExitCode {
    let args: Vec<OsString> = std::env::args_os().skip(1).collect();
    match args.first().and_then(|a| a.to_str()) {
        None => {
            show_about();
            return ExitCode::SUCCESS;
        }
        Some("--version") => {
            println!("Sound Honter {}", env!("CARGO_PKG_VERSION"));
            return ExitCode::SUCCESS;
        }
        Some("settings") => {
            settings_window::show(&data_dir());
            return ExitCode::SUCCESS;
        }
        Some("--help") => {
            println!("{USAGE}");
            return ExitCode::SUCCESS;
        }
        _ => {}
    }
    let no_ui = args.iter().any(|a| a == "--no-ui");
    match parse_args(args) {
        Ok(request) => execute(request),
        Err(problem) => {
            if no_ui {
                eprintln!("{problem}\n\n{USAGE}");
            } else {
                let heading = "Sound Honter couldn't start that";
                ui::message(ui::Icon::Error, heading, &problem, Some(USAGE), &[]);
            }
            ExitCode::from(EXIT_USAGE)
        }
    }
}

fn execute(request: Request) -> ExitCode {
    let Request {
        mut job,
        use_queue,
        no_ui,
        ask,
        files,
    } = request;
    let ask = ask && !no_ui;

    let source = if use_queue {
        // Explorer gives each process one file, but a command line could give several.
        let mut runner = None;
        let queue_name = if ask {
            ASKING_QUEUE.to_string()
        } else {
            job.queue_name()
        };
        for file in &files {
            match queue::submit(&data_dir(), &queue_name, file) {
                Ok(claimed) => runner = runner.or(claimed),
                Err(error) => {
                    let reason = format!("Could not queue {}: {error}", file.display());
                    return report(&failure_outcome(reason), no_ui);
                }
            }
        }
        match runner {
            Some(runner) => Source::Queue(runner),
            // Another Sound Honter process is working on this selection, and will take our files.
            None => return ExitCode::SUCCESS,
        }
    } else {
        Source::Files(files)
    };

    let source = if ask {
        match ask_how_to_mix(source) {
            Some((options, files)) => {
                job = Job::Mix(options);
                Source::Files(files)
            }
            None => return ExitCode::SUCCESS,
        }
    } else {
        source
    };

    let ffmpeg = Engine::locate_ffmpeg();
    let settings = Settings::load(&data_dir());
    let shared = Arc::new(Shared::default());
    if no_ui {
        return report(&job::run(job, source, ffmpeg, settings, &shared), true);
    }

    let worker = {
        let shared = shared.clone();
        thread::spawn(move || {
            // Tells the dialog to close even if the work panics.
            struct Finish(Arc<Shared>);
            impl Drop for Finish {
                fn drop(&mut self) {
                    self.0.finished.store(true, Ordering::Release);
                }
            }
            let finish = Finish(shared);
            job::run(job, source, ffmpeg, settings, &finish.0)
        })
    };
    let initial_heading = match job {
        Job::Convert(_) => "Converting…",
        Job::Normalize => "Normalizing…",
        Job::Trim => "Trimming…",
        Job::Mix(_) => "Mixing…",
    };
    ui::progress(&shared, initial_heading);
    match worker.join() {
        Ok(outcome) => report(&outcome, false),
        Err(_) => report(
            &failure_outcome("An unexpected internal error occurred.".to_string()),
            false,
        ),
    }
}

/// Gathers the selection and asks how to mix it. Returns `None` if there's nothing left to do.
fn ask_how_to_mix(source: Source) -> Option<(MixOptions, Vec<PathBuf>)> {
    let mut runner = None;
    let mut files = match source {
        Source::Files(files) => files,
        Source::Queue(mut queue) => {
            let first = queue.next_batch().ok().flatten();
            runner = Some(queue);
            first?
        }
    };

    // With too few files there's nothing to ask: the job itself will explain.
    let choice = if files.len() < 2 {
        Some(MixOptions::default())
    } else {
        let remembered_path = data_dir().join("mix-options.txt");
        let remembered = fs::read_to_string(&remembered_path).unwrap_or_default();
        let choice = ui::mix_options(files.len(), MixOptions::from_text(&remembered));
        if let Some(options) = choice {
            let _ = fs::write(&remembered_path, options.to_text());
        }
        choice
    };

    // Whatever was chosen, empty the queue so that nothing is left for some later run.
    if let Some(queue) = &mut runner {
        while let Ok(Some(batch)) = queue.next_batch() {
            for file in batch {
                if !files.contains(&file) {
                    files.push(file);
                }
            }
        }
    }
    choice.map(|options| (options, files))
}

fn failure_outcome(reason: String) -> Outcome {
    Outcome {
        failures: vec![("Sound Honter".to_string(), reason)],
        ..Outcome::default()
    }
}

fn report(outcome: &Outcome, no_ui: bool) -> ExitCode {
    if no_ui {
        for output in &outcome.outputs {
            println!("{}", output.display());
        }
        for (label, reason) in &outcome.failures {
            eprintln!("{label}: {reason}");
        }
        if outcome.cancelled {
            eprintln!("Cancelled");
        }
    } else {
        show_summary(outcome);
    }
    if outcome.failures.is_empty() && !outcome.cancelled {
        ExitCode::SUCCESS
    } else {
        ExitCode::from(EXIT_FAILED)
    }
}

fn file_count(count: usize) -> String {
    format!("{count} file{}", if count == 1 { "" } else { "s" })
}

/// A readable list of file names, capped so the dialog can't outgrow the screen.
fn name_list(paths: &[PathBuf]) -> String {
    const LIMIT: usize = 5;
    let mut lines: Vec<String> = paths
        .iter()
        .take(LIMIT)
        .map(|p| {
            p.file_name()
                .unwrap_or_default()
                .to_string_lossy()
                .into_owned()
        })
        .collect();
    if paths.len() > LIMIT {
        lines.push(format!("and {} more", paths.len() - LIMIT));
    }
    lines.join("\n")
}

fn show_summary(outcome: &Outcome) {
    let Outcome {
        outputs,
        failures,
        cancelled,
    } = outcome;
    let details: String = failures
        .iter()
        .map(|(label, reason)| format!("{label}\n    {reason}\n"))
        .collect();
    if !failures.is_empty() {
        write_error_log(&details);
    }

    let show_file: &[(i32, &str)] = &[(ID_SHOW_FILE, "Show in folder")];
    let buttons = if outputs.is_empty() { &[] } else { show_file };

    let clicked = match (failures.as_slice(), outputs.is_empty(), cancelled) {
        // The user asked for this, and there's nothing to show for it.
        ([], true, true) => return,
        // A process that Explorer started late, after another had already handled its file.
        ([], true, false) => return,
        ([], false, true) => {
            let heading = format!("Cancelled after finishing {}", file_count(outputs.len()));
            ui::message(ui::Icon::None, &heading, &name_list(outputs), None, buttons)
        }
        ([], false, false) => {
            let heading = format!("Finished {}", file_count(outputs.len()));
            ui::message(ui::Icon::None, &heading, &name_list(outputs), None, buttons)
        }
        ([(label, reason)], true, _) => {
            let body = format!("{label}\n\n{reason}");
            ui::message(ui::Icon::Error, "That didn't work", &body, None, &[])
        }
        _ => {
            let heading = format!("{} couldn't be processed", file_count(failures.len()));
            let body = if outputs.is_empty() {
                "Show details to see why.".to_string()
            } else {
                let finished = file_count(outputs.len());
                format!("Finished {finished}:\n{}", name_list(outputs))
            };
            ui::message(ui::Icon::Warning, &heading, &body, Some(&details), buttons)
        }
    };

    if clicked == ID_SHOW_FILE {
        show_in_folder(&outputs[0]);
    }
}

fn write_error_log(details: &str) {
    let dir = data_dir();
    let _ = fs::create_dir_all(&dir);
    let _ = fs::write(dir.join("last-errors.log"), details);
}

fn show_in_folder(path: &Path) {
    let mut argument = OsString::from("/select,\"");
    argument.push(path);
    argument.push("\"");
    let _ = Command::new("explorer.exe").raw_arg(argument).spawn();
}

fn show_about() {
    let heading = format!("Sound Honter {}", env!("CARGO_PKG_VERSION"));
    let body = "Sound Honter works from the File Explorer right-click menu. \
        On Windows 11, choose \"Show more options\" first.\n\n\
        Convert: right-click one or more audio files, then choose Sound Honter and a format.\n\n\
        Normalize or trim: the same, to even out loudness or cut silence from both ends.\n\n\
        Mix: select two or more audio files, right-click, then choose Sound Honter > Mix. \
        For more than 15 files, use Send to > Sound Honter Mix.\n\n\
        New files are saved beside the originals unless you choose otherwise in Settings. \
        Nothing is ever overwritten.";
    let buttons = [(ID_SETTINGS, "Settings"), (ID_LICENSES, "Licenses")];
    let clicked = ui::message(ui::Icon::None, &heading, body, None, &buttons);
    if clicked == ID_SETTINGS {
        settings_window::show(&data_dir());
    } else if clicked == ID_LICENSES {
        if let Some(dir) = std::env::current_exe()
            .ok()
            .and_then(|e| Some(e.parent()?.to_owned()))
        {
            let _ = Command::new("notepad.exe")
                .arg(dir.join("THIRD-PARTY-NOTICES.txt"))
                .spawn();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(args: &[&str]) -> Result<Request, String> {
        parse_args(args.iter().map(OsString::from).collect())
    }

    #[test]
    fn parses_convert() {
        let request = parse(&["convert", "flac", "--queue", "C:\\a b.wav"])
            .ok()
            .unwrap();
        assert_eq!(request.job, Job::Convert(Format::Flac));
        assert!(request.use_queue);
        assert!(!request.no_ui);
        assert_eq!(request.files, [PathBuf::from("C:\\a b.wav")]);
    }

    #[test]
    fn parses_mix() {
        let request = parse(&["mix", "--no-ui", "C:\\a.wav", "C:\\b.wav"])
            .ok()
            .unwrap();
        assert_eq!(request.job, Job::Mix(MixOptions::default()));
        assert!(request.no_ui);
        assert_eq!(request.files.len(), 2);
    }

    #[test]
    fn parses_mix_options() {
        let args = [
            "mix",
            "--format",
            "flac",
            "--no-normalize",
            "--ask",
            "a.wav",
        ];
        let request = parse(&args).ok().unwrap();
        let expected = MixOptions {
            format: Format::Flac,
            normalize: false,
        };
        assert_eq!(request.job, Job::Mix(expected));
        assert!(request.ask);
    }

    #[test]
    fn makes_relative_paths_absolute() {
        let request = parse(&["mix", "a.wav"]).ok().unwrap();
        assert!(request.files[0].is_absolute());
    }

    #[test]
    fn rejects_bad_requests() {
        for args in [
            &[][..],
            &["explode", "a.wav"],
            &["convert"],
            &["convert", "exe", "a.wav"],
            &["mix"],
            &["mix", "--format", "exe", "a.wav"],
            &["mix", "a.wav", "--format"],
            &["trim", "--no-normalize", "a.wav"],
            &["convert", "mp3", "--ask", "a.wav"],
        ] {
            assert!(parse(args).is_err(), "{args:?} should be rejected");
        }
    }
}
