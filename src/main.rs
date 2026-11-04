//! Sound Honter: convert and mix audio files from the Windows right-click menu.

#![cfg_attr(not(test), windows_subsystem = "windows")]

mod ui;

use soundhonter::engine::{Engine, Format};
use soundhonter::job::{self, Job, Outcome, Shared, Source};
use soundhonter::queue;
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
  soundhonter mix [--queue] [--no-ui] <files...>

  --queue   Gather files from separate launches into one job (used by the right-click menu)
  --no-ui   Report on stdout and stderr instead of showing dialogs";

const EXIT_FAILED: u8 = 1;
const EXIT_USAGE: u8 = 2;

const ID_SHOW_FILE: i32 = 1001;
const ID_LICENSES: i32 = 1002;

struct Request {
    job: Job,
    use_queue: bool,
    no_ui: bool,
    files: Vec<PathBuf>,
}

fn parse_args(args: Vec<OsString>) -> Result<Request, String> {
    let mut args = args.into_iter();
    let job = match args.next().as_deref().and_then(|a| a.to_str()) {
        Some("mix") => Job::Mix,
        Some("convert") => {
            let name = args.next().ok_or("Convert needs a format.")?;
            let name = name.to_string_lossy();
            let format = Format::parse(&name).ok_or_else(|| format!("Unknown format: {name}"))?;
            Job::Convert(format)
        }
        Some(other) => return Err(format!("Unknown command: {other}")),
        None => return Err("Missing command.".to_string()),
    };
    let (mut use_queue, mut no_ui, mut files) = (false, false, Vec::new());
    for arg in args {
        match arg.to_str() {
            Some("--queue") => use_queue = true,
            Some("--no-ui") => no_ui = true,
            _ => files.push(PathBuf::from(arg)),
        }
    }
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
        job,
        use_queue,
        no_ui,
        files,
    } = request;

    let source = if use_queue {
        // Explorer gives each process one file, but a command line could give several.
        let mut runner = None;
        for file in &files {
            match queue::submit(&data_dir(), &job.queue_name(), file) {
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

    let ffmpeg = Engine::locate_ffmpeg();
    let shared = Arc::new(Shared::default());
    if no_ui {
        return report(&job::run(job, source, ffmpeg, &shared), true);
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
            job::run(job, source, ffmpeg, &finish.0)
        })
    };
    let initial_heading = match job {
        Job::Convert(_) => "Converting…",
        Job::Mix => "Mixing…",
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
        ([], false, true) => {
            let heading = format!("Cancelled after finishing {}", file_count(outputs.len()));
            ui::message(ui::Icon::None, &heading, &name_list(outputs), None, buttons)
        }
        ([], _, false) => {
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
        Mix: select two or more audio files, right-click, then choose Sound Honter > Mix selected files. \
        For more than 15 files, use Send to > Sound Honter Mix.\n\n\
        New files are saved beside the originals. Nothing is ever overwritten.";
    let buttons = [(ID_LICENSES, "Licenses")];
    if ui::message(ui::Icon::None, &heading, body, None, &buttons) == ID_LICENSES {
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
        assert_eq!(request.job, Job::Mix);
        assert!(request.no_ui);
        assert_eq!(request.files.len(), 2);
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
        ] {
            assert!(parse(args).is_err(), "{args:?} should be rejected");
        }
    }
}
