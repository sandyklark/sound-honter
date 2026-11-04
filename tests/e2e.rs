//! Runs the real app and the real FFmpeg against real audio, then measures what came out.

use soundhonter::engine::Engine;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;
use std::time::Duration;

const APP: &str = env!("CARGO_BIN_EXE_soundhonter");

fn repo_path(relative: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join(relative)
}

fn ffmpeg() -> PathBuf {
    repo_path("third_party/ffmpeg/ffmpeg.exe")
}

/// A scratch directory holding test audio, removed when the test finishes.
struct Scratch(PathBuf);

impl Scratch {
    fn new() -> Scratch {
        static NEXT: AtomicU32 = AtomicU32::new(0);
        let id = NEXT.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!("soundhonter-{}-{id}", std::process::id()));
        fs::create_dir_all(dir.join("data")).unwrap();
        Scratch(dir)
    }

    fn copy(&self, fixture: &str, name: &str) -> PathBuf {
        let target = self.0.join(name);
        fs::copy(repo_path(&format!("tests/fixtures/{fixture}")), &target).unwrap();
        target
    }

    /// Writes a generated sound using an FFmpeg lavfi source expression.
    fn generate(&self, source: &str, name: &str) -> PathBuf {
        let target = self.0.join(name);
        let status = Command::new(ffmpeg())
            .args([
                "-hide_banner",
                "-loglevel",
                "error",
                "-y",
                "-f",
                "lavfi",
                "-i",
                source,
            ])
            .arg(&target)
            .status()
            .unwrap();
        assert!(status.success(), "failed to generate {name}");
        target
    }

    fn app(&self, args: &[&str], files: &[&Path]) -> Command {
        let mut command = Command::new(APP);
        command
            .args(args)
            .args(files)
            .env("SOUNDHONTER_FFMPEG", ffmpeg())
            .env("SOUNDHONTER_DATA_DIR", self.0.join("data"));
        command
    }

    fn run(&self, args: &[&str], files: &[&Path]) -> Output {
        let mut args = args.to_vec();
        args.push("--no-ui");
        self.app(&args, files).output().unwrap()
    }

    /// Names of the files in the directory, excluding test bookkeeping.
    fn files(&self) -> Vec<String> {
        let mut names: Vec<String> = fs::read_dir(&self.0)
            .unwrap()
            .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
            .filter(|name| name != "data")
            .collect();
        names.sort();
        names
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn assert_success(output: &Output) {
    assert!(
        output.status.success(),
        "exit {:?}\nstdout: {}\nstderr: {}",
        output.status.code(),
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );
}

fn stream_info(file: &Path) -> String {
    let output = Command::new(ffmpeg())
        .args(["-hide_banner", "-i"])
        .arg(file)
        .output()
        .unwrap();
    String::from_utf8_lossy(&output.stderr).into_owned()
}

fn analysis(file: &Path, filter: &str) -> String {
    let output = Command::new(ffmpeg())
        .args(["-hide_banner", "-nostats", "-i"])
        .arg(file)
        .args(["-af", filter, "-f", "null", "-"])
        .output()
        .unwrap();
    String::from_utf8_lossy(&output.stderr).into_owned()
}

/// Finds a number on the first line containing `label`.
fn measure(log: &str, label: &str) -> f64 {
    let line = log
        .lines()
        .find(|line| line.contains(label))
        .unwrap_or_else(|| panic!("no {label} in:\n{log}"));
    let rest = &line[line.find(label).unwrap() + label.len()..];
    rest.split_whitespace()
        .next()
        .unwrap()
        .parse()
        .unwrap_or_else(|_| panic!("no number in: {line}"))
}

fn duration(file: &Path) -> f64 {
    let info = stream_info(file);
    let text = info
        .split("Duration: ")
        .nth(1)
        .unwrap()
        .split(',')
        .next()
        .unwrap();
    let parts: Vec<f64> = text.split(':').map(|p| p.parse().unwrap()).collect();
    parts[0] * 3600.0 + parts[1] * 60.0 + parts[2]
}

#[test]
fn convert_writes_each_format_beside_the_source() {
    let cases = [
        ("mp3", "Audio: mp3"),
        ("flac", "Audio: flac"),
        ("ogg", "Audio: vorbis"),
        ("m4a", "Audio: aac"),
        ("wav", "Audio: pcm_s24le"),
    ];
    for (format, expected) in cases {
        let scratch = Scratch::new();
        let input = scratch.copy("blunt.wav", "source.wav");
        let output = scratch.run(&["convert", format], &[&input]);
        assert_success(&output);

        let expected_name = if format == "wav" {
            "source (1).wav".to_string()
        } else {
            format!("source.{format}")
        };
        let result = scratch.0.join(&expected_name);
        assert!(result.is_file(), "{format}: missing {expected_name}");
        let info = stream_info(&result);
        assert!(info.contains(expected), "{format}: {info}");
        assert_eq!(
            String::from_utf8_lossy(&output.stdout).trim(),
            result.display().to_string()
        );
    }
}

#[test]
fn mp3_keeps_the_original_delivery_spec() {
    let scratch = Scratch::new();
    let input = scratch.copy("blunt.wav", "source.wav");
    assert_success(&scratch.run(&["convert", "mp3"], &[&input]));
    let info = stream_info(&scratch.0.join("source.mp3"));
    assert!(info.contains("44100 Hz, stereo"), "{info}");
    assert!(info.contains("192 kb/s"), "{info}");
}

#[test]
fn lossless_formats_preserve_rate_channels_and_depth() {
    let scratch = Scratch::new();
    let hi_res = scratch.copy("blunt.wav", "hi.wav"); // 24-bit, 48 kHz, stereo
    let mono = scratch.generate("sine=440:d=0.2:r=22050", "mono.wav"); // 16-bit, 22 kHz, mono

    assert_success(&scratch.run(&["convert", "flac"], &[&hi_res]));
    let info = stream_info(&scratch.0.join("hi.flac"));
    assert!(info.contains("48000 Hz, stereo, s32 (24 bit)"), "{info}");

    assert_success(&scratch.run(&["convert", "wav"], &[&mono]));
    let info = stream_info(&scratch.0.join("mono (1).wav"));
    assert!(
        info.contains("pcm_s16le") && info.contains("22050 Hz, mono"),
        "{info}"
    );
}

#[test]
fn never_overwrites_existing_files() {
    let scratch = Scratch::new();
    let input = scratch.copy("drum.wav", "take.wav");
    fs::write(scratch.0.join("take.mp3"), b"precious").unwrap();

    assert_success(&scratch.run(&["convert", "mp3"], &[&input]));
    assert_success(&scratch.run(&["convert", "mp3"], &[&input]));

    assert_eq!(fs::read(scratch.0.join("take.mp3")).unwrap(), b"precious");
    assert_eq!(
        scratch.files(),
        ["take (1).mp3", "take (2).mp3", "take.mp3", "take.wav"]
    );
}

#[test]
fn handles_awkward_file_names() {
    for name in [
        "rock & roll! 100%.wav",
        "a^b (final) [v2], #3;'.wav",
        "%PATH% ~1 =x= +y.wav",
        "звук 音 🎵.wav",
        "-starts-with-dash.wav",
        "two.dots.in.name.wav",
    ] {
        let scratch = Scratch::new();
        let input = scratch.copy("drum.wav", name);
        assert_success(&scratch.run(&["convert", "ogg"], &[&input]));
        let stem = name.strip_suffix(".wav").unwrap();
        assert!(scratch.0.join(format!("{stem}.ogg")).is_file(), "{name}");
    }
}

#[test]
fn bad_files_are_reported_without_stopping_the_batch() {
    let scratch = Scratch::new();
    let good = scratch.copy("drum.wav", "good.wav");
    let corrupt = scratch.0.join("corrupt.wav");
    fs::write(&corrupt, b"this is not audio at all").unwrap();
    let missing = scratch.0.join("missing.wav");
    let good_too = scratch.copy("blunt.wav", "good too.wav");

    let output = scratch.run(&["convert", "mp3"], &[&good, &corrupt, &missing, &good_too]);

    assert_eq!(output.status.code(), Some(1));
    let errors = String::from_utf8_lossy(&output.stderr);
    assert!(
        errors.contains("corrupt.wav: Not a readable audio file"),
        "{errors}"
    );
    assert!(errors.contains("missing.wav: File not found"), "{errors}");
    assert_eq!(
        scratch.files(),
        [
            "corrupt.wav",
            "good too.mp3",
            "good too.wav",
            "good.mp3",
            "good.wav"
        ]
    );
}

#[test]
fn read_only_folders_get_a_clear_message() {
    let scratch = Scratch::new();
    let input = scratch.copy("drum.wav", "drum.wav");
    let folder = scratch.0.to_str().unwrap().to_string();
    // Everyone (S-1-1-0) may not create files in this folder.
    let denied = Command::new("icacls")
        .args([&folder, "/deny", "*S-1-1-0:(WD,AD)"])
        .output()
        .unwrap();
    assert!(denied.status.success());

    let output = scratch.run(&["convert", "mp3"], &[&input]);

    let restored = Command::new("icacls")
        .args([&folder, "/remove:d", "*S-1-1-0"])
        .output()
        .unwrap();
    assert!(restored.status.success());
    assert_eq!(output.status.code(), Some(1));
    let errors = String::from_utf8_lossy(&output.stderr);
    assert!(errors.contains("Can't save in this folder"), "{errors}");
}

#[test]
fn missing_ffmpeg_is_reported_and_leaves_nothing_behind() {
    let scratch = Scratch::new();
    let good = scratch.copy("drum.wav", "good.wav");
    let result = Command::new(APP)
        .args(["convert", "mp3", "--no-ui"])
        .arg(&good)
        .env("SOUNDHONTER_FFMPEG", scratch.0.join("no-such-ffmpeg.exe"))
        .output()
        .unwrap();
    assert_eq!(result.status.code(), Some(1));
    let errors = String::from_utf8_lossy(&result.stderr);
    assert!(errors.contains("Could not start FFmpeg"), "{errors}");
    assert_eq!(scratch.files(), ["good.wav"]);
}

#[test]
fn mix_runs_for_the_longest_input() {
    let scratch = Scratch::new();
    let short = scratch.copy("blunt.wav", "blunt.wav"); // 1.52 s
    let long = scratch.copy("drum.wav", "drum.wav"); // 1.94 s
    assert_success(&scratch.run(&["mix"], &[&short, &long]));

    let mixed = scratch.0.join("blunt + drum mix.wav");
    assert!(
        (duration(&mixed) - 1.94).abs() < 0.03,
        "{}",
        stream_info(&mixed)
    );
}

#[test]
fn mix_output_is_a_sane_24_bit_file_at_the_highest_input_rate() {
    let scratch = Scratch::new();
    let at_48k = scratch.copy("blunt.wav", "blunt.wav");
    let at_44k = scratch.copy("drum.wav", "drum.wav");
    assert_success(&scratch.run(&["mix"], &[&at_44k, &at_48k]));

    let info = stream_info(&scratch.0.join("drum + blunt mix.wav"));
    assert!(info.contains("pcm_s24le"), "{info}");
    assert!(info.contains("48000 Hz, stereo"), "{info}");
}

#[test]
fn mix_hits_the_loudness_target_without_clipping() {
    let scratch = Scratch::new();
    let one = scratch.generate("sine=220:d=6:r=48000,volume=0.9", "one.wav");
    let two = scratch.generate("sine=330:d=6:r=48000,volume=0.9", "two.wav");
    assert_success(&scratch.run(&["mix"], &[&one, &two]));

    let log = analysis(&scratch.0.join("one + two mix.wav"), "ebur128=peak=true");
    let summary = &log[log.rfind("Summary:").unwrap()..];
    let loudness = measure(summary, "I:");
    let true_peak = measure(summary, "Peak:");
    assert!(
        (loudness + 16.0).abs() < 1.0,
        "loudness {loudness}\n{summary}"
    );
    assert!(true_peak <= -2.5, "true peak {true_peak}\n{summary}");
}

#[test]
fn mix_handles_short_one_shots_like_the_examples() {
    let scratch = Scratch::new();
    let blunt = scratch.copy("blunt.wav", "blunt.wav");
    let drum = scratch.copy("drum.wav", "drum.wav");
    assert_success(&scratch.run(&["mix"], &[&blunt, &drum]));

    let log = analysis(&scratch.0.join("blunt + drum mix.wav"), "ebur128=peak=true");
    let summary = &log[log.rfind("Summary:").unwrap()..];
    let true_peak = measure(summary, "Peak:");
    assert!(true_peak <= -2.5, "true peak {true_peak}\n{summary}");
}

#[test]
fn mix_survives_extreme_levels() {
    let scratch = Scratch::new();
    // Full-scale square waves are about as loud as digital audio gets.
    let square = "aevalsrc='sgn(sin(2*PI*220*t))':d=4:s=48000";
    let loud: Vec<PathBuf> = (0..8)
        .map(|i| scratch.generate(square, &format!("loud{i}.wav")))
        .collect();
    let refs: Vec<&Path> = loud.iter().map(PathBuf::as_path).collect();
    assert_success(&scratch.run(&["mix"], &refs));
    assert!(scratch.0.join("loud0 + 7 more mix.wav").is_file());

    let quiet = scratch.generate("sine=440:d=4:r=48000,volume=-95dB", "quiet.wav");
    let quiet2 = scratch.generate("sine=660:d=4:r=48000,volume=-95dB", "quiet2.wav");
    assert_success(&scratch.run(&["mix"], &[&quiet, &quiet2]));
    assert!(scratch.0.join("quiet + quiet2 mix.wav").is_file());
}

#[test]
fn mono_inputs_are_centered_rather_than_hard_panned() {
    let scratch = Scratch::new();
    let tone = scratch.generate("sine=440:d=4:r=44100", "tone.wav");
    let silence = scratch.generate("anullsrc=r=44100:cl=mono:d=4", "silence.wav");
    assert_success(&scratch.run(&["mix"], &[&tone, &silence]));

    let log = analysis(
        &scratch.0.join("tone + silence mix.wav"),
        "astats=measure_perchannel=RMS_level:measure_overall=none",
    );
    let levels: Vec<f64> = log
        .lines()
        .filter(|line| line.contains("RMS level dB:"))
        .map(|line| line.rsplit(' ').next().unwrap().parse().unwrap())
        .collect();
    assert_eq!(levels.len(), 2, "{log}");
    assert!(
        (levels[0] - levels[1]).abs() < 0.1,
        "L/R differ: {levels:?}"
    );
    assert!(levels[0] > -60.0, "signal is missing: {levels:?}");
}

#[test]
fn mixing_several_files_gets_a_summarized_name() {
    let scratch = Scratch::new();
    let files: Vec<PathBuf> = ["a.wav", "b.wav", "c.wav", "d.wav"]
        .iter()
        .map(|name| scratch.copy("drum.wav", name))
        .collect();
    let refs: Vec<&Path> = files.iter().map(PathBuf::as_path).collect();
    assert_success(&scratch.run(&["mix"], &refs));
    assert!(scratch.0.join("a + 3 more mix.wav").is_file());
}

#[test]
fn mixing_pure_silence_succeeds_without_normalizing() {
    let scratch = Scratch::new();
    let a = scratch.generate("anullsrc=r=48000:cl=stereo:d=1", "a.wav");
    let b = scratch.generate("anullsrc=r=48000:cl=stereo:d=1", "b.wav");
    assert_success(&scratch.run(&["mix"], &[&a, &b]));
    assert!(scratch.0.join("a + b mix.wav").is_file());
}

#[test]
fn mix_explains_when_given_too_few_files() {
    let scratch = Scratch::new();
    let only = scratch.copy("drum.wav", "only.wav");
    let output = scratch.run(&["mix"], &[&only]);
    assert_eq!(output.status.code(), Some(1));
    let errors = String::from_utf8_lossy(&output.stderr);
    assert!(
        errors.contains(soundhonter::job::NEED_TWO_FILES),
        "{errors}"
    );
}

#[test]
fn mix_names_the_file_that_broke_it() {
    let scratch = Scratch::new();
    let good = scratch.copy("drum.wav", "good.wav");
    let bad = scratch.0.join("bad.wav");
    fs::write(&bad, b"nope").unwrap();
    let output = scratch.run(&["mix"], &[&good, &bad]);
    assert_eq!(output.status.code(), Some(1));
    let errors = String::from_utf8_lossy(&output.stderr);
    assert!(
        errors.contains("bad.wav: Not a readable audio file"),
        "{errors}"
    );
    assert_eq!(scratch.files(), ["bad.wav", "good.wav"]);
}

/// Launches one process per file at once, the way Explorer does for a multi-selection.
fn launch_like_explorer(scratch: &Scratch, args: &[&str], files: &[PathBuf]) {
    let mut children = Vec::new();
    for file in files {
        let mut args = args.to_vec();
        args.extend(["--queue", "--no-ui"]);
        let child = scratch.app(&args, &[file]).spawn().unwrap();
        children.push(child);
        std::thread::sleep(Duration::from_millis(15));
    }
    for mut child in children {
        assert!(child.wait().unwrap().success());
    }
}

#[test]
fn queued_launches_convert_every_selected_file_exactly_once() {
    let scratch = Scratch::new();
    let names: Vec<String> = (0..12).map(|i| format!("{i:02}.wav")).collect();
    let files: Vec<PathBuf> = names.iter().map(|n| scratch.copy("drum.wav", n)).collect();
    launch_like_explorer(&scratch, &["convert", "mp3"], &files);

    let mp3s = scratch
        .files()
        .into_iter()
        .filter(|f| f.ends_with(".mp3"))
        .count();
    assert_eq!(mp3s, 12, "{:?}", scratch.files());
    assert!(
        !scratch.files().iter().any(|f| f.contains('(')),
        "{:?}",
        scratch.files()
    );
}

#[test]
fn queued_launches_become_one_mix() {
    let scratch = Scratch::new();
    let files: Vec<PathBuf> = ["a.wav", "b.wav", "c.wav", "d.wav", "e.wav"]
        .iter()
        .map(|n| scratch.copy("drum.wav", n))
        .collect();
    launch_like_explorer(&scratch, &["mix"], &files);

    let mixes: Vec<String> = scratch
        .files()
        .into_iter()
        .filter(|f| f.contains("mix"))
        .collect();
    assert_eq!(mixes.len(), 1, "{mixes:?}");
    assert!(mixes[0].contains("4 more"), "{mixes:?}");
}

#[test]
fn abandoned_queue_entries_are_not_acted_on() {
    let scratch = Scratch::new();
    let ghost = scratch.copy("drum.wav", "ghost.wav");
    let real = scratch.copy("drum.wav", "real.wav");
    // As left behind by a crash, long ago.
    let entry = format!("1000\t{}\n", ghost.display());
    fs::write(scratch.0.join("data/convert-mp3.queue"), entry).unwrap();

    assert_success(&scratch.run(&["convert", "mp3", "--queue"], &[&real]));

    assert!(scratch.0.join("real.mp3").is_file());
    assert!(!scratch.0.join("ghost.mp3").exists());
}

#[test]
fn cancelling_stops_ffmpeg_and_removes_temp_files() {
    let scratch = Scratch::new();
    let long = scratch.generate("sine=440:d=3600:r=44100", "long.wav");

    let cancel: Arc<std::sync::atomic::AtomicBool> = Arc::default();
    let engine = Engine::new(ffmpeg(), cancel.clone());
    let trigger = std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(400));
        cancel.store(true, Ordering::Relaxed);
    });

    let started = std::time::Instant::now();
    let result = engine.convert(&long, soundhonter::engine::Format::Mp3, &mut |_| {});
    trigger.join().unwrap();

    assert!(matches!(
        result,
        Err(soundhonter::engine::Failure::Cancelled)
    ));
    assert!(
        started.elapsed() < Duration::from_secs(20),
        "cancel was slow"
    );
    assert_eq!(scratch.files(), ["long.wav"]);
}

#[test]
fn reports_progress_up_to_completion() {
    let scratch = Scratch::new();
    let input = scratch.generate("sine=440:d=120:r=44100", "input.wav");
    let engine = Engine::new(ffmpeg(), Arc::default());

    let mut seen = Vec::new();
    engine
        .convert(&input, soundhonter::engine::Format::Flac, &mut |f| {
            seen.push(f)
        })
        .unwrap();

    assert_eq!(seen.last(), Some(&1.0));
    assert!(
        seen.windows(2).all(|w| w[0] <= w[1]),
        "not monotonic: {seen:?}"
    );
}

#[test]
fn version_flag_reports_the_package_version() {
    let output = Command::new(APP).arg("--version").output().unwrap();
    assert_success(&output);
    let text = String::from_utf8_lossy(&output.stdout);
    assert!(text.contains(env!("CARGO_PKG_VERSION")), "{text}");
}

impl Scratch {
    /// Writes a tagged sound in the given format, with cover art if the format can hold it.
    fn tagged(&self, extension: &str) -> PathBuf {
        let cover = self.0.join("cover.png");
        let run = |args: &[&str], target: &Path| {
            let status = Command::new(ffmpeg())
                .args(["-hide_banner", "-loglevel", "error", "-y"])
                .args(args)
                .arg(target)
                .status()
                .unwrap();
            assert!(status.success(), "failed to write {}", target.display());
        };
        run(
            &[
                "-f",
                "lavfi",
                "-i",
                "color=red:s=64x64:d=1",
                "-frames:v",
                "1",
            ],
            &cover,
        );

        let target = self.0.join(format!("tagged.{extension}"));
        let mut command = vec!["-f", "lavfi", "-i", "sine=440:d=1:r=44100"];
        let cover_path = cover.to_str().unwrap();
        if matches!(extension, "mp3" | "flac" | "m4a") {
            command.extend(["-i", cover_path, "-map", "0", "-map", "1", "-c:v", "copy"]);
            command.extend(["-disposition:v", "attached_pic"]);
        }
        command.extend(["-metadata", "title=MyTitle", "-metadata", "artist=MyArtist"]);
        run(&command, &target);
        fs::remove_file(&cover).unwrap();
        target
    }
}

fn has_tags(file: &Path) -> bool {
    let info = stream_info(file).to_lowercase();
    info.contains("mytitle") && info.contains("myartist")
}

fn has_cover(file: &Path) -> bool {
    stream_info(file).contains("(attached pic)")
}

fn printed_output(output: &Output) -> PathBuf {
    PathBuf::from(String::from_utf8_lossy(&output.stdout).trim())
}

#[test]
fn converting_keeps_tags_between_every_pair_of_formats() {
    let formats = ["mp3", "flac", "m4a", "ogg", "wav"];
    for source in formats {
        for target in formats {
            let scratch = Scratch::new();
            let input = scratch.tagged(source);
            let output = scratch.run(&["convert", target], &[&input]);
            assert_success(&output);
            let result = printed_output(&output);
            assert!(
                has_tags(&result),
                "{source} to {target}: {}",
                stream_info(&result)
            );
        }
    }
}

#[test]
fn converting_keeps_cover_art_where_the_format_can_hold_it() {
    for source in ["mp3", "flac", "m4a"] {
        for (target, expected) in [
            ("mp3", true),
            ("flac", true),
            ("m4a", true),
            ("ogg", false),
            ("wav", false),
        ] {
            let scratch = Scratch::new();
            let input = scratch.tagged(source);
            let output = scratch.run(&["convert", target], &[&input]);
            assert_success(&output);
            assert_eq!(
                has_cover(&printed_output(&output)),
                expected,
                "{source} to {target}"
            );
        }
    }
}

#[test]
fn mixes_do_not_pretend_to_be_their_first_file() {
    let scratch = Scratch::new();
    let tagged = scratch.tagged("flac");
    let plain = scratch.copy("drum.wav", "drum.wav");
    let output = scratch.run(&["mix"], &[&tagged, &plain]);
    assert_success(&output);
    assert!(!stream_info(&printed_output(&output))
        .to_lowercase()
        .contains("mytitle"));
}

fn integrated_loudness(file: &Path) -> f64 {
    let log = analysis(file, "ebur128=peak=true");
    measure(&log[log.rfind("Summary:").unwrap()..], "I:")
}

#[test]
fn normalize_reaches_the_target_and_keeps_the_file_as_it_was() {
    let scratch = Scratch::new();
    let input = scratch.generate("sine=440:d=6:r=22050,volume=0.05", "quiet.wav");
    assert_success(&scratch.run(&["normalize"], &[&input]));

    let result = scratch.0.join("quiet normalized.wav");
    assert!((integrated_loudness(&result) + 16.0).abs() < 1.0);
    let info = stream_info(&result);
    assert!(info.contains("22050 Hz, mono"), "{info}");
    assert!(info.contains("pcm_s16le"), "{info}");
    assert!((duration(&result) - 6.0).abs() < 0.05, "{info}");
}

#[test]
fn normalize_keeps_format_rate_tags_and_cover() {
    let scratch = Scratch::new();
    let input = scratch.0.join("song.mp3");
    let long = scratch.generate("sine=330:d=4:r=48000", "long.wav");
    let tagged = scratch.tagged("mp3");
    let status = Command::new(ffmpeg())
        .args(["-hide_banner", "-loglevel", "error", "-y", "-i"])
        .arg(&long)
        .arg("-i")
        .arg(&tagged)
        .args([
            "-map",
            "0:a",
            "-map",
            "1:v",
            "-map_metadata",
            "1",
            "-c:v",
            "copy",
        ])
        .args(["-disposition:v", "attached_pic"])
        .arg(&input)
        .status()
        .unwrap();
    assert!(status.success());

    assert_success(&scratch.run(&["normalize"], &[&input]));

    let result = scratch.0.join("song normalized.mp3");
    let info = stream_info(&result);
    assert!(
        info.contains("Audio: mp3") && info.contains("48000 Hz, mono"),
        "{info}"
    );
    assert!(has_tags(&result), "{info}");
    assert!(has_cover(&result), "{info}");
}

#[test]
fn normalize_explains_silent_files_and_carries_on() {
    let scratch = Scratch::new();
    let silent = scratch.generate("anullsrc=r=48000:cl=stereo:d=2", "silent.wav");
    let good = scratch.generate("sine=440:d=4:r=48000", "good.wav");
    let output = scratch.run(&["normalize"], &[&silent, &good]);

    assert_eq!(output.status.code(), Some(1));
    let errors = String::from_utf8_lossy(&output.stderr);
    assert!(
        errors.contains(soundhonter::engine::TOO_QUIET_TO_MEASURE),
        "{errors}"
    );
    assert!(scratch.0.join("good normalized.wav").is_file());
    assert!(!scratch.0.join("silent normalized.wav").exists());
}

#[test]
fn normalize_copes_with_a_very_short_sound() {
    let scratch = Scratch::new();
    let blip = scratch.generate("sine=880:d=0.1:r=48000", "blip.wav");
    let output = scratch.run(&["normalize"], &[&blip]);
    let errors = String::from_utf8_lossy(&output.stderr);
    // Either it worked, or it says why: what matters is no raw FFmpeg error.
    assert!(
        output.status.success() || errors.contains(soundhonter::engine::TOO_QUIET_TO_MEASURE),
        "{errors}"
    );
}

/// Half a second of silence, one second of tone, then half a second of silence.
fn padded_tone(scratch: &Scratch) -> PathBuf {
    scratch.generate(
        "sine=440:d=1:r=48000,adelay=500:all=1,apad=pad_dur=0.5",
        "padded.wav",
    )
}

#[test]
fn trim_cuts_silence_from_both_ends() {
    let scratch = Scratch::new();
    let input = padded_tone(&scratch);
    assert!(
        (duration(&input) - 2.0).abs() < 0.03,
        "{}",
        stream_info(&input)
    );
    assert_success(&scratch.run(&["trim"], &[&input]));

    let result = scratch.0.join("padded trimmed.wav");
    assert!(
        (duration(&result) - 1.0).abs() < 0.03,
        "{}",
        stream_info(&result)
    );
    // The kept audio is the tone, starting straight away.
    let log = analysis(&result, "silencedetect=n=-60dB:d=0.02");
    assert!(!log.contains("silence_start"), "{log}");
}

#[test]
fn trim_keeps_silence_in_the_middle() {
    let scratch = Scratch::new();
    let input = scratch.generate(
        "aevalsrc='if(between(t,0.6,1.4),0,sin(2*PI*440*t))':d=2:s=48000",
        "gap.wav",
    );
    let output = scratch.run(&["trim"], &[&input]);
    assert_eq!(output.status.code(), Some(1));
    let errors = String::from_utf8_lossy(&output.stderr);
    assert!(errors.contains(soundhonter::engine::NO_SILENCE), "{errors}");
    assert_eq!(scratch.files(), ["gap.wav"]);
}

#[test]
fn trim_keeps_format_and_tags() {
    let scratch = Scratch::new();
    let tagged = scratch.tagged("flac");
    let padded = padded_tone(&scratch);
    let input = scratch.0.join("song.flac");
    let status = Command::new(ffmpeg())
        .args(["-hide_banner", "-loglevel", "error", "-y", "-i"])
        .arg(&padded)
        .arg("-i")
        .arg(&tagged)
        .args([
            "-map",
            "0:a",
            "-map",
            "1:v",
            "-map_metadata",
            "1",
            "-c:v",
            "copy",
        ])
        .args(["-disposition:v", "attached_pic"])
        .arg(&input)
        .status()
        .unwrap();
    assert!(status.success());

    assert_success(&scratch.run(&["trim"], &[&input]));

    let result = scratch.0.join("song trimmed.flac");
    assert!(
        (duration(&result) - 1.0).abs() < 0.03,
        "{}",
        stream_info(&result)
    );
    assert!(has_tags(&result), "{}", stream_info(&result));
    assert!(has_cover(&result), "{}", stream_info(&result));
}

#[test]
fn trim_explains_silent_files() {
    let scratch = Scratch::new();
    let silent = scratch.generate("anullsrc=r=48000:cl=stereo:d=2", "silent.wav");
    let output = scratch.run(&["trim"], &[&silent]);
    assert_eq!(output.status.code(), Some(1));
    let errors = String::from_utf8_lossy(&output.stderr);
    assert!(errors.contains(soundhonter::engine::ALL_SILENT), "{errors}");
}

#[test]
fn sources_we_cannot_write_come_out_as_wav() {
    let scratch = Scratch::new();
    let input = scratch.generate("sine=440:d=4:r=48000", "sound.aiff");
    assert_success(&scratch.run(&["normalize"], &[&input]));
    assert!(
        scratch.0.join("sound normalized.wav").is_file(),
        "{:?}",
        scratch.files()
    );
}

#[test]
fn queued_edits_handle_a_whole_selection() {
    let scratch = Scratch::new();
    let files: Vec<PathBuf> = ["a.wav", "b.wav", "c.wav"]
        .iter()
        .map(|name| scratch.generate("sine=440:d=4:r=48000", name))
        .collect();
    launch_like_explorer(&scratch, &["normalize"], &files);
    for name in ["a", "b", "c"] {
        assert!(
            scratch.0.join(format!("{name} normalized.wav")).is_file(),
            "{name}"
        );
    }
}
