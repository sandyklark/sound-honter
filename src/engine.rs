//! Builds and runs FFmpeg commands, and interprets their output.

use std::ffi::OsString;
use std::fs::{self, OpenOptions};
use std::io::{self, BufRead, BufReader, Read};
use std::os::windows::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicI64, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::Duration;

const CREATE_NO_WINDOW: u32 = 0x0800_0000;

/// Loudness target for mixes.
const LOUDNORM_TARGET: &str = "I=-16:TP=-3:LRA=11";
/// Anything quieter than this counts as silence when trimming.
const SILENCE_THRESHOLD: &str = "-60dB";
/// Silence shorter than this at either end is left alone, in seconds.
const SHORTEST_SILENCE: f64 = 0.02;
/// How far apart two times can be and still count as the same moment, in seconds.
/// Reported durations are estimates for some formats, so this can't be too tight.
const TIME_TOLERANCE: f64 = 0.1;
/// Gain applied to a mix before analysis, on top of dividing by the number of inputs.
const MIX_HEADROOM: f64 = 0.25;
/// How much of a mix's progress bar is the analysis pass, rather than the render pass.
const ANALYSIS_SHARE: f64 = 0.3;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Format {
    Mp3,
    Wav,
    Flac,
    Ogg,
    M4a,
}

impl Format {
    pub const ALL: [Format; 5] = [
        Format::Mp3,
        Format::Wav,
        Format::Flac,
        Format::Ogg,
        Format::M4a,
    ];

    pub fn parse(name: &str) -> Option<Format> {
        Format::ALL
            .into_iter()
            .find(|f| f.extension().eq_ignore_ascii_case(name))
    }

    pub fn extension(self) -> &'static str {
        match self {
            Format::Mp3 => "mp3",
            Format::Wav => "wav",
            Format::Flac => "flac",
            Format::Ogg => "ogg",
            Format::M4a => "m4a",
        }
    }

    /// OGG and WAV can't hold embedded pictures, and FFmpeg fails if asked to put one in them.
    fn holds_cover_art(self) -> bool {
        matches!(self, Format::Mp3 | Format::Flac | Format::M4a)
    }

    fn muxer(self) -> &'static str {
        match self {
            Format::M4a => "ipod",
            other => other.extension(),
        }
    }

    /// The format to edit a file in without changing what kind of file it is.
    /// Sources that we can read but not write, like AIFF, come out as WAV.
    fn matching(path: &Path) -> Format {
        path.extension()
            .and_then(|extension| Format::parse(&extension.to_string_lossy()))
            .unwrap_or(Format::Wav)
    }

    /// Settings for converting to this format. MP3 keeps the original delivery spec.
    fn conversion_args(self, source: &Probe) -> Vec<&'static str> {
        let mut settings = self.codec_args(source);
        if self == Format::Mp3 {
            settings.extend(["-ar", "44100", "-ac", "2"]);
        }
        settings
    }

    /// Encoder settings, which keep the source's sample rate and channels.
    fn codec_args(self, source: &Probe) -> Vec<&'static str> {
        match self {
            Format::Mp3 => vec!["-c:a", "libmp3lame", "-b:a", "192k"],
            Format::Wav if source.is_high_depth => vec!["-c:a", "pcm_s24le"],
            Format::Wav => vec!["-c:a", "pcm_s16le"],
            Format::Flac => vec!["-c:a", "flac"],
            Format::Ogg => vec!["-c:a", "libvorbis", "-q:a", "6"],
            Format::M4a => vec!["-c:a", "aac", "-b:a", "192k"],
        }
    }
}

#[derive(Debug)]
pub enum Failure {
    Cancelled,
    Error(String),
}

impl From<io::Error> for Failure {
    fn from(error: io::Error) -> Self {
        Failure::Error(error.to_string())
    }
}

/// What FFmpeg reports about an input file.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Probe {
    pub duration_secs: Option<f64>,
    pub sample_rate: Option<u32>,
    /// False only when the source is known to fit in 16 bits.
    pub is_high_depth: bool,
    /// The stream holding embedded cover art, if it's in a format every target accepts.
    pub cover_stream: Option<u32>,
    /// OGG files keep their tags on the audio stream, rather than on the file as other formats do.
    pub tags_on_stream: bool,
}

impl Probe {
    /// Selects what to carry over: the audio, its tags, and cover art if `format` can hold it.
    fn stream_args(&self, format: Format) -> Vec<OsString> {
        let mut selected = args(&["-map", "0:a:0"]);
        if self.tags_on_stream {
            selected.extend(args(&["-map_metadata", "0:s:a:0"]));
        }
        if let Some(stream) = self.cover_stream.filter(|_| format.holds_cover_art()) {
            let map = format!("0:{stream}");
            selected.extend(args(&["-map", &map, "-c:v", "copy"]));
            selected.extend(args(&["-disposition:v", "attached_pic"]));
        }
        selected
    }
}

/// Finds a stream of embedded cover art in a line of `ffmpeg -i` output.
fn parse_cover_stream(line: &str) -> Option<u32> {
    if !line.contains("(attached pic)") {
        return None;
    }
    let (stream, details) = line
        .trim_start()
        .strip_prefix("Stream #0:")?
        .split_once(": ")?;
    if !(details.starts_with("Video: mjpeg") || details.starts_with("Video: png")) {
        return None;
    }
    let digits = stream
        .find(|c: char| !c.is_ascii_digit())
        .unwrap_or(stream.len());
    stream[..digits].parse().ok()
}

/// Parses `ffmpeg -i` output. Returns `None` if the input has no audio stream.
pub fn parse_probe(ffmpeg_output: &str) -> Option<Probe> {
    let audio = ffmpeg_output
        .lines()
        .find(|line| line.trim_start().starts_with("Stream #") && line.contains("Audio: "))?;
    let details = &audio[audio.find("Audio: ")? + "Audio: ".len()..];
    let fields: Vec<&str> = details.split(", ").map(str::trim).collect();

    let sample_rate = fields
        .iter()
        .find_map(|field| field.strip_suffix(" Hz")?.parse().ok());
    let is_low_depth = fields.iter().any(|field| {
        let name = field.split_whitespace().next().unwrap_or_default();
        matches!(name, "s16" | "s16p" | "u8" | "u8p")
    });
    let duration_secs = ffmpeg_output
        .lines()
        .find_map(|line| line.trim_start().strip_prefix("Duration: "))
        .and_then(|rest| parse_timestamp(rest.split(',').next()?));

    Some(Probe {
        duration_secs,
        sample_rate,
        is_high_depth: !is_low_depth,
        cover_stream: ffmpeg_output.lines().find_map(parse_cover_stream),
        tags_on_stream: ffmpeg_output.contains("Input #0, ogg"),
    })
}

fn parse_timestamp(text: &str) -> Option<f64> {
    let mut parts = text.trim().split(':');
    let hours: f64 = parts.next()?.parse().ok()?;
    let minutes: f64 = parts.next()?.parse().ok()?;
    let seconds: f64 = parts.next()?.parse().ok()?;
    Some(hours * 3600.0 + minutes * 60.0 + seconds)
}

/// The measurements from a `loudnorm` analysis pass.
#[derive(Debug, PartialEq)]
pub struct Loudness {
    input_i: f64,
    input_tp: f64,
    input_lra: f64,
    input_thresh: f64,
    target_offset: f64,
}

impl Loudness {
    /// The filter that applies these measurements. Values are clamped to the ranges that
    /// `loudnorm` accepts, because it refuses to run at all if one falls outside them.
    fn linear_filter(&self) -> String {
        format!(
            "loudnorm={LOUDNORM_TARGET}:measured_I={:.2}:measured_TP={:.2}:measured_LRA={:.2}:             measured_thresh={:.2}:offset={:.2}:linear=true",
            self.input_i.clamp(-99.0, 0.0),
            self.input_tp.clamp(-99.0, 99.0),
            self.input_lra.clamp(0.0, 99.0),
            self.input_thresh.clamp(-99.0, 0.0),
            self.target_offset.clamp(-99.0, 99.0),
        )
    }
}

/// Parses the JSON that `loudnorm=print_format=json` prints.
/// Returns `None` for silence, which has no measurable loudness.
pub fn parse_loudness(ffmpeg_output: &str) -> Option<Loudness> {
    let block = &ffmpeg_output[ffmpeg_output.rfind('{')?..];
    let value = |key: &str| -> Option<f64> {
        let line = block.lines().find(|l| l.contains(&format!("\"{key}\"")))?;
        let value = line
            .split(':')
            .nth(1)?
            .trim()
            .trim_end_matches(',')
            .trim_matches('"');
        value.parse().ok().filter(|v: &f64| v.is_finite())
    };
    Some(Loudness {
        input_i: value("input_i")?,
        input_tp: value("input_tp")?,
        input_lra: value("input_lra")?,
        input_thresh: value("input_thresh")?,
        target_offset: value("target_offset")?,
    })
}

/// The part of a file to keep, in seconds.
#[derive(Debug, PartialEq)]
pub struct KeptRange {
    start: f64,
    end: Option<f64>,
}

pub const TOO_QUIET_TO_MEASURE: &str = "Too quiet or too short to measure its loudness";
pub const ALL_SILENT: &str = "The whole file is silent";
pub const NO_SILENCE: &str = "No silence at the start or end to trim";

/// Works out what to keep from `silencedetect` output, cutting silence only from the two ends.
pub fn parse_kept_range(ffmpeg_output: &str, duration: Option<f64>) -> Result<KeptRange, Failure> {
    let time_after = |line: &str, label: &str| -> Option<f64> {
        let rest = &line[line.find(label)? + label.len()..];
        rest.split_whitespace().next()?.parse().ok()
    };
    let mut silences: Vec<(f64, Option<f64>)> = Vec::new();
    for line in ffmpeg_output.lines() {
        if let Some(start) = time_after(line, "silence_start: ") {
            silences.push((start, None));
        } else if let Some(end) = time_after(line, "silence_end: ") {
            match silences.last_mut() {
                Some(last) => last.1 = Some(end),
                None => silences.push((0.0, Some(end))),
            }
        }
    }

    let reaches_end = |end: Option<f64>| match (end, duration) {
        (None, _) => true,
        (Some(end), Some(duration)) => end >= duration - TIME_TOLERANCE,
        (Some(_), None) => false,
    };
    let leading = silences
        .first()
        .filter(|(start, _)| *start <= TIME_TOLERANCE);
    if leading.is_some_and(|(_, end)| reaches_end(*end)) {
        return Err(Failure::Error(ALL_SILENT.to_string()));
    }
    let start = leading.and_then(|(_, end)| *end).unwrap_or(0.0);
    let end = silences
        .last()
        .filter(|(_, end)| reaches_end(*end))
        .map(|(start, _)| *start);
    if start == 0.0 && end.is_none() {
        return Err(Failure::Error(NO_SILENCE.to_string()));
    }
    Ok(KeptRange { start, end })
}

/// Sums all inputs at equal level, for as long as the longest one lasts.
///
/// The sum is turned down so that it stays below 0 LUFS however many loud files are layered,
/// which `loudnorm` requires of its measurements. Normalizing sets the final level afterwards.
fn mix_graph(input_count: usize, sample_rate: u32) -> String {
    let mut graph = String::new();
    let mut labels = String::new();
    for i in 0..input_count {
        graph += &format!(
            "[{i}:a]aresample={sample_rate},aformat=sample_fmts=fltp:channel_layouts=stereo[a{i}];"
        );
        labels += &format!("[a{i}]");
    }
    let headroom = MIX_HEADROOM / input_count as f64;
    graph
        + &labels
        + &format!(
            "amix=inputs={input_count}:duration=longest:normalize=0,volume={headroom}[mixed]"
        )
}

/// Chooses a filename stem that describes the mix.
pub fn mix_stem(inputs: &[PathBuf]) -> String {
    let stem = |path: &PathBuf| -> String {
        let name = path.file_stem().unwrap_or_default().to_string_lossy();
        name.chars().take(40).collect()
    };
    match inputs {
        [one, two] => format!("{} + {} mix", stem(one), stem(two)),
        [first, rest @ ..] => format!("{} + {} more mix", stem(first), rest.len()),
        [] => "mix".to_string(),
    }
}

/// Moves `temp` to `dir/stem.extension`, or `stem (1).extension` and so on. Never overwrites.
fn publish(temp: &Path, dir: &Path, stem: &str, extension: &str) -> io::Result<PathBuf> {
    for attempt in 0.. {
        let name = match attempt {
            0 => format!("{stem}.{extension}"),
            n => format!("{stem} ({n}).{extension}"),
        };
        let target = dir.join(name);
        // Creating the file claims the name atomically; the rename then replaces our placeholder.
        match OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&target)
        {
            Ok(_) => {
                return match fs::rename(temp, &target) {
                    Ok(()) => Ok(target),
                    Err(error) => {
                        let _ = fs::remove_file(&target);
                        Err(error)
                    }
                };
            }
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(error),
        }
    }
    unreachable!()
}

/// Removes the temp file when dropped, if it hasn't been moved into place.
struct TempFile(PathBuf);

impl TempFile {
    /// Creates the file, which also confirms up front that the folder can be written to.
    fn beside(dir: &Path, label: &str) -> Result<TempFile, Failure> {
        let path = dir.join(format!(".soundhonter-{}-{label}.tmp", std::process::id()));
        match fs::File::create(&path) {
            Ok(_) => Ok(TempFile(path)),
            Err(error) if error.kind() == io::ErrorKind::PermissionDenied => Err(Failure::Error(
                format!("Can't save in this folder: {}", dir.display()),
            )),
            Err(error) => Err(error.into()),
        }
    }
}

impl Drop for TempFile {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.0);
    }
}

fn parent_dir(path: &Path) -> &Path {
    match path.parent() {
        Some(parent) if !parent.as_os_str().is_empty() => parent,
        _ => Path::new("."),
    }
}

fn args(values: &[&str]) -> Vec<OsString> {
    values.iter().map(OsString::from).collect()
}

pub struct Engine {
    ffmpeg: PathBuf,
    cancel: Arc<AtomicBool>,
}

impl Engine {
    pub fn new(ffmpeg: PathBuf, cancel: Arc<AtomicBool>) -> Engine {
        Engine { ffmpeg, cancel }
    }

    /// Locates the bundled FFmpeg, installed beside the app.
    /// `SOUNDHONTER_FFMPEG` overrides it for development and tests.
    pub fn locate_ffmpeg() -> PathBuf {
        if let Some(path) = std::env::var_os("SOUNDHONTER_FFMPEG") {
            return path.into();
        }
        let exe = std::env::current_exe().unwrap_or_default();
        parent_dir(&exe).join("ffmpeg").join("ffmpeg.exe")
    }

    /// Runs FFmpeg to completion, returning its log output. `on_progress` receives 0.0 to 1.0.
    fn run(
        &self,
        args: &[OsString],
        total_secs: Option<f64>,
        on_progress: &mut dyn FnMut(f64),
    ) -> Result<String, Failure> {
        let mut child = Command::new(&self.ffmpeg)
            .args([
                "-hide_banner",
                "-nostdin",
                "-nostats",
                "-progress",
                "pipe:1",
            ])
            .args(args)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .creation_flags(CREATE_NO_WINDOW)
            .spawn()
            .map_err(|e| Failure::Error(format!("Could not start FFmpeg: {e}")))?;

        let elapsed_us = Arc::new(AtomicI64::new(0));
        let stdout = child.stdout.take().expect("stdout is piped");
        let stdout_reader = {
            let elapsed_us = elapsed_us.clone();
            thread::spawn(move || {
                for line in BufReader::new(stdout).lines().map_while(Result::ok) {
                    if let Some(value) = line.strip_prefix("out_time_us=") {
                        if let Ok(us) = value.trim().parse() {
                            elapsed_us.store(us, Ordering::Relaxed);
                        }
                    }
                }
            })
        };
        let mut stderr = child.stderr.take().expect("stderr is piped");
        let stderr_reader = thread::spawn(move || {
            let mut bytes = Vec::new();
            let _ = stderr.read_to_end(&mut bytes);
            String::from_utf8_lossy(&bytes).into_owned()
        });

        let status = loop {
            if self.cancel.load(Ordering::Relaxed) {
                let _ = child.kill();
                let _ = child.wait();
                let _ = stdout_reader.join();
                let _ = stderr_reader.join();
                return Err(Failure::Cancelled);
            }
            if let Some(status) = child.try_wait()? {
                break status;
            }
            if let Some(total) = total_secs.filter(|t| *t > 0.0) {
                let elapsed = elapsed_us.load(Ordering::Relaxed) as f64 / 1_000_000.0;
                on_progress((elapsed / total).clamp(0.0, 1.0));
            }
            thread::sleep(Duration::from_millis(30));
        };
        let _ = stdout_reader.join();
        let log = stderr_reader.join().unwrap_or_default();
        if status.success() {
            on_progress(1.0);
            Ok(log)
        } else {
            let reason = log
                .lines()
                .rev()
                .map(str::trim)
                .find(|line| !line.is_empty())
                .unwrap_or("FFmpeg failed without an error message")
                .to_string();
            Err(Failure::Error(reason))
        }
    }

    fn probe(&self, input: &Path) -> Result<Probe, Failure> {
        if !input.is_file() {
            return Err(Failure::Error("File not found".to_string()));
        }
        // With no output file FFmpeg prints the stream info and exits with an error; that's expected.
        let output = Command::new(&self.ffmpeg)
            .args(["-hide_banner", "-nostdin", "-i"])
            .arg(input)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .creation_flags(CREATE_NO_WINDOW)
            .output()
            .map_err(|e| Failure::Error(format!("Could not start FFmpeg: {e}")))?;
        parse_probe(&String::from_utf8_lossy(&output.stderr))
            .ok_or_else(|| Failure::Error("Not a readable audio file".to_string()))
    }

    /// Converts one file, writing the result beside it. Returns the output path.
    pub fn convert(
        &self,
        input: &Path,
        format: Format,
        on_progress: &mut dyn FnMut(f64),
    ) -> Result<PathBuf, Failure> {
        let probe = self.probe(input)?;
        let dir = parent_dir(input);
        let temp = TempFile::beside(dir, "convert")?;

        let mut command = args(&["-loglevel", "error", "-y", "-i"]);
        command.push(input.into());
        command.extend(probe.stream_args(format));
        command.extend(
            format
                .conversion_args(&probe)
                .into_iter()
                .map(OsString::from),
        );
        command.extend(args(&["-f", format.muxer()]));
        command.push(temp.0.clone().into());
        self.run(&command, probe.duration_secs, on_progress)?;

        let stem = input.file_stem().unwrap_or_default().to_string_lossy();
        Ok(publish(&temp.0, dir, &stem, format.extension())?)
    }

    /// Runs an analysis pass over a file's audio, returning FFmpeg's log.
    fn analyze(
        &self,
        input: &Path,
        filter: &str,
        probe: &Probe,
        on_progress: &mut dyn FnMut(f64),
    ) -> Result<String, Failure> {
        let mut command = args(&["-loglevel", "info", "-i"]);
        command.push(input.into());
        command.extend(args(&["-map", "0:a:0", "-af", filter, "-f", "null", "-"]));
        self.run(&command, probe.duration_secs, &mut |f| {
            on_progress(f * ANALYSIS_SHARE)
        })
    }

    /// Writes a filtered copy of a file beside it, as the same kind of file with `suffix` in its name.
    fn render_edit(
        &self,
        input: &Path,
        filter: &str,
        suffix: &str,
        probe: &Probe,
        on_progress: &mut dyn FnMut(f64),
    ) -> Result<PathBuf, Failure> {
        let format = Format::matching(input);
        let dir = parent_dir(input);
        let temp = TempFile::beside(dir, "edit")?;

        let mut command = args(&["-loglevel", "error", "-y", "-i"]);
        command.push(input.into());
        command.extend(probe.stream_args(format));
        command.extend(args(&["-af", filter]));
        // loudnorm works at 192 kHz internally, so the output rate must be set explicitly.
        if let Some(rate) = probe.sample_rate {
            command.extend(args(&["-ar", &rate.to_string()]));
        }
        command.extend(format.codec_args(probe).into_iter().map(OsString::from));
        command.extend(args(&["-f", format.muxer()]));
        command.push(temp.0.clone().into());
        self.run(&command, probe.duration_secs, &mut |f| {
            on_progress(ANALYSIS_SHARE + f * (1.0 - ANALYSIS_SHARE))
        })?;

        let stem = input.file_stem().unwrap_or_default().to_string_lossy();
        let name = format!("{stem} {suffix}");
        Ok(publish(&temp.0, dir, &name, format.extension())?)
    }

    /// Brings a file to the standard loudness target, keeping its format.
    pub fn normalize(
        &self,
        input: &Path,
        on_progress: &mut dyn FnMut(f64),
    ) -> Result<PathBuf, Failure> {
        let probe = self.probe(input)?;
        let measure = format!("loudnorm={LOUDNORM_TARGET}:print_format=json");
        let log = self.analyze(input, &measure, &probe, on_progress)?;
        let loudness =
            parse_loudness(&log).ok_or_else(|| Failure::Error(TOO_QUIET_TO_MEASURE.to_string()))?;
        self.render_edit(
            input,
            &loudness.linear_filter(),
            "normalized",
            &probe,
            on_progress,
        )
    }

    /// Removes silence from the start and end of a file, keeping its format.
    pub fn trim(&self, input: &Path, on_progress: &mut dyn FnMut(f64)) -> Result<PathBuf, Failure> {
        let probe = self.probe(input)?;
        let detect = format!("silencedetect=n={SILENCE_THRESHOLD}:d={SHORTEST_SILENCE}");
        let log = self.analyze(input, &detect, &probe, on_progress)?;
        let kept = parse_kept_range(&log, probe.duration_secs)?;
        let mut filter = format!("atrim=start={}", kept.start);
        if let Some(end) = kept.end {
            filter += &format!(":end={end}");
        }
        filter += ",asetpts=PTS-STARTPTS";
        self.render_edit(input, &filter, "trimmed", &probe, on_progress)
    }

    /// Layers the inputs into one loudness-normalized 24-bit stereo WAV beside the first input.
    pub fn mix(
        &self,
        inputs: &[PathBuf],
        on_progress: &mut dyn FnMut(f64),
    ) -> Result<PathBuf, Failure> {
        let mut probes = Vec::with_capacity(inputs.len());
        for input in inputs {
            let probe = self.probe(input).map_err(|failure| match failure {
                Failure::Error(reason) => {
                    let name = input.file_name().unwrap_or_default().to_string_lossy();
                    Failure::Error(format!("{name}: {reason}"))
                }
                cancelled => cancelled,
            })?;
            probes.push(probe);
        }
        let sample_rate = probes
            .iter()
            .filter_map(|p| p.sample_rate)
            .max()
            .unwrap_or(48_000);
        let duration = probes
            .iter()
            .filter_map(|p| p.duration_secs)
            .max_by(f64::total_cmp);

        let mut input_args = Vec::new();
        for input in inputs {
            input_args.push(OsString::from("-i"));
            input_args.push(input.into());
        }
        let graph = mix_graph(inputs.len(), sample_rate);

        // Pass 1: measure, so that pass 2 can apply one clean gain instead of riding the levels.
        let mut analysis = args(&["-loglevel", "info"]);
        analysis.extend(input_args.iter().cloned());
        analysis.extend(args(&[
            "-filter_complex",
            &format!("{graph};[mixed]loudnorm={LOUDNORM_TARGET}:print_format=json[out]"),
            "-map",
            "[out]",
            "-f",
            "null",
            "-",
        ]));
        let log = self.run(&analysis, duration, &mut |f| {
            on_progress(f * ANALYSIS_SHARE)
        })?;

        let final_graph = match parse_loudness(&log) {
            Some(loudness) => format!("{graph};[mixed]{}[out]", loudness.linear_filter()),
            None => format!("{graph};[mixed]anull[out]"),
        };

        // Pass 2: render.
        let dir = parent_dir(&inputs[0]);
        let temp = TempFile::beside(dir, "mix")?;
        let mut render = args(&["-loglevel", "error", "-y"]);
        render.extend(input_args);
        render.extend(args(&[
            "-filter_complex",
            &final_graph,
            "-map",
            "[out]",
            // A mix isn't the first file, so it shouldn't inherit that file's title and artist.
            "-map_metadata",
            "-1",
            // loudnorm works at 192 kHz internally, so the output rate must be set explicitly.
            "-ar",
            &sample_rate.to_string(),
            "-c:a",
            "pcm_s24le",
            "-f",
            "wav",
        ]));
        render.push(temp.0.clone().into());
        self.run(&render, duration, &mut |f| {
            on_progress(ANALYSIS_SHARE + f * (1.0 - ANALYSIS_SHARE))
        })?;

        Ok(publish(&temp.0, dir, &mix_stem(inputs), "wav")?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const WAV_24: &str = "Input #0, wav, from 'a.wav':\n  Duration: 00:01:01.52, bitrate: 2310 kb/s\n  \
        Stream #0:0: Audio: pcm_s24le ([1][0][0][0] / 0x0001), 48000 Hz, stereo, s32 (24 bit), 2304 kb/s\n";

    #[test]
    fn probes_duration_rate_and_depth() {
        let probe = parse_probe(WAV_24).unwrap();
        assert!((probe.duration_secs.unwrap() - 61.52).abs() < 1e-6);
        assert_eq!(probe.sample_rate, Some(48000));
        assert!(probe.is_high_depth);
    }

    #[test]
    fn recognizes_16_bit_sources() {
        let text = "  Duration: 00:00:01.00, bitrate: 1 kb/s\n  Stream #0:0: Audio: pcm_s16le, 44100 Hz, mono, s16, 705 kb/s\n";
        assert!(!parse_probe(text).unwrap().is_high_depth);
    }

    #[test]
    fn treats_lossy_sources_as_high_depth() {
        let text = "  Duration: N/A\n  Stream #0:0: Audio: mp3, 44100 Hz, stereo, fltp, 192 kb/s\n";
        let probe = parse_probe(text).unwrap();
        assert!(probe.is_high_depth);
        assert_eq!(probe.duration_secs, None);
    }

    #[test]
    fn rejects_input_without_audio() {
        let text = "  Duration: 00:00:01.00\n  Stream #0:0: Video: h264, yuv420p, 1920x1080\n";
        assert_eq!(parse_probe(text), None);
    }

    #[test]
    fn ignores_cover_art_before_audio() {
        let text = "  Stream #0:0: Video: mjpeg, 500x500\n  Stream #0:1: Audio: flac, 96000 Hz, stereo, s32 (24 bit)\n";
        assert_eq!(parse_probe(text).unwrap().sample_rate, Some(96000));
    }

    fn kept(log: &str, duration: Option<f64>) -> Result<(f64, Option<f64>), String> {
        match parse_kept_range(log, duration) {
            Ok(range) => Ok((range.start, range.end)),
            Err(Failure::Error(reason)) => Err(reason),
            Err(Failure::Cancelled) => panic!("nothing cancelled this"),
        }
    }

    #[test]
    fn trims_silence_at_both_ends() {
        let log = "[silencedetect @ 0] silence_start: 0\n\
            [silencedetect @ 0] silence_end: 1.5 | silence_duration: 1.5\n\
            [silencedetect @ 0] silence_start: 4\n\
            [silencedetect @ 0] silence_end: 6 | silence_duration: 2\n";
        assert_eq!(kept(log, Some(6.0)), Ok((1.5, Some(4.0))));
    }

    #[test]
    fn leaves_silence_in_the_middle_alone() {
        let log = "silence_start: 2\nsilence_end: 3 | silence_duration: 1\n";
        assert_eq!(kept(log, Some(6.0)), Err(NO_SILENCE.to_string()));
    }

    #[test]
    fn trims_an_end_silence_that_was_never_closed() {
        let log = "silence_start: 2\nsilence_end: 3 | silence_duration: 1\nsilence_start: 5\n";
        assert_eq!(kept(log, Some(6.0)), Ok((0.0, Some(5.0))));
    }

    #[test]
    fn recognizes_entirely_silent_files() {
        let log = "silence_start: 0\nsilence_end: 6 | silence_duration: 6\n";
        assert_eq!(kept(log, Some(6.0)), Err(ALL_SILENT.to_string()));
        assert_eq!(
            kept("silence_start: 0\n", None),
            Err(ALL_SILENT.to_string())
        );
    }

    #[test]
    fn trims_only_the_start_when_the_duration_is_unknown() {
        let log = "silence_start: 0\nsilence_end: 1 | silence_duration: 1\n\
            silence_start: 5\nsilence_end: 6 | silence_duration: 1\n";
        assert_eq!(kept(log, None), Ok((1.0, None)));
    }

    #[test]
    fn finds_cover_art_streams() {
        let line = "  Stream #0:1[0x2]: Video: png, rgb24, 64x64 [SAR 1:1 DAR 1:1], 90k tbn (attached pic)";
        assert_eq!(parse_cover_stream(line), Some(1));
        let jpeg = "  Stream #0:2(eng): Video: mjpeg (Baseline), yuvj420p, 500x500, 90k tbn (attached pic)";
        assert_eq!(parse_cover_stream(jpeg), Some(2));
    }

    #[test]
    fn ignores_real_video_and_unusual_picture_formats() {
        let video = "  Stream #0:1: Video: h264, yuv420p, 1920x1080, 30 fps";
        assert_eq!(parse_cover_stream(video), None);
        let webp = "  Stream #0:1: Video: webp, yuv420p, 64x64 (attached pic)";
        assert_eq!(parse_cover_stream(webp), None);
    }

    const LOUDNORM: &str = "[Parsed_loudnorm_0 @ 0] \n{\n\t\"input_i\" : \"-8.04\",\n\t\"input_tp\" : \"0.12\",\n\
        \t\"input_lra\" : \"1.50\",\n\t\"input_thresh\" : \"-18.10\",\n\t\"output_i\" : \"-16.0\",\n\
        \t\"target_offset\" : \"-0.20\"\n}\n";

    #[test]
    fn parses_loudness_measurements() {
        let m = parse_loudness(LOUDNORM).unwrap();
        assert_eq!(m.input_i, -8.04);
        assert_eq!(m.input_tp, 0.12);
        assert_eq!(m.target_offset, -0.2);
    }

    #[test]
    fn keeps_measurements_within_what_loudnorm_accepts() {
        let filter = Loudness {
            input_i: 3.5,
            input_tp: -120.0,
            input_lra: 0.0,
            input_thresh: 1.0,
            target_offset: 0.0,
        }
        .linear_filter();
        assert!(filter.contains("measured_I=0.00:"), "{filter}");
        assert!(filter.contains("measured_TP=-99.00:"), "{filter}");
        assert!(filter.contains("measured_thresh=0.00:"), "{filter}");
    }

    #[test]
    fn treats_silence_as_unmeasurable() {
        assert_eq!(parse_loudness(&LOUDNORM.replace("-8.04", "-inf")), None);
    }

    #[test]
    fn names_mixes_after_their_inputs() {
        let paths = |names: &[&str]| names.iter().map(PathBuf::from).collect::<Vec<_>>();
        assert_eq!(mix_stem(&paths(&["C:\\x\\a.wav", "b.mp3"])), "a + b mix");
        assert_eq!(
            mix_stem(&paths(&["a.wav", "b.wav", "c.wav", "d.wav"])),
            "a + 3 more mix"
        );
    }

    #[test]
    fn parses_format_names_case_insensitively() {
        assert_eq!(Format::parse("MP3"), Some(Format::Mp3));
        assert_eq!(Format::parse("exe"), None);
    }
}
