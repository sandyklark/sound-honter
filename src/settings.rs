//! User settings: how files are encoded, measured and saved.
//!
//! Stored as `key = value` lines in `settings.ini` in the data folder. Anything missing or
//! unreadable falls back to its default, so an old or hand-edited file never stops the app.

use std::fs;
use std::io;
use std::path::Path;

const FILE_NAME: &str = "settings.ini";

/// Defines a setting with a fixed list of choices. Each choice has a key, which is how it's
/// stored, and a label, which is how it's shown. The first choice listed is the default.
macro_rules! choice {
    ($(#[$meta:meta])* $name:ident { $($variant:ident => $key:literal, $label:literal;)+ }) => {
        $(#[$meta])*
        #[derive(Clone, Copy, Debug, PartialEq, Eq)]
        pub enum $name {
            $($variant,)+
        }

        impl $name {
            pub const ALL: &'static [$name] = &[$($name::$variant,)+];

            pub fn key(self) -> &'static str {
                match self {
                    $($name::$variant => $key,)+
                }
            }

            pub fn label(self) -> &'static str {
                match self {
                    $($name::$variant => $label,)+
                }
            }

            fn from_key(key: &str) -> Option<$name> {
                $name::ALL.iter().copied().find(|choice| choice.key() == key)
            }
        }

        impl Default for $name {
            fn default() -> Self {
                $name::ALL[0]
            }
        }
    };
}

choice!(Mp3Quality {
    Cbr192 => "192", "192 kbps";
    Cbr128 => "128", "128 kbps";
    Cbr256 => "256", "256 kbps";
    Cbr320 => "320", "320 kbps";
    VbrHigh => "v0", "Variable, highest quality (V0)";
});

choice!(WavDepth {
    MatchSource => "match", "Match the source (16 or 24-bit)";
    Bits16 => "16", "16-bit";
    Bits24 => "24", "24-bit";
    Float32 => "32f", "32-bit float";
});

choice!(SampleRate {
    Keep => "keep", "Keep the source's rate";
    Hz44100 => "44100", "44.1 kHz";
    Hz48000 => "48000", "48 kHz";
});

choice!(Channels {
    Keep => "keep", "Keep the source's channels";
    Stereo => "stereo", "Stereo";
    Mono => "mono", "Mono";
});

choice!(
    /// Quality of OGG and M4A files.
    LossyQuality {
        High => "high", "High";
        Medium => "medium", "Medium";
        Low => "low", "Low (smaller files)";
    }
);

choice!(LoudnessTarget {
    Standard => "-16", "-16 LUFS (general use)";
    Streaming => "-14", "-14 LUFS (streaming services)";
    Broadcast => "-23", "-23 LUFS (broadcast)";
});

choice!(PeakCeiling {
    Minus3 => "-3", "-3 dBTP";
    Minus2 => "-2", "-2 dBTP";
    Minus1 => "-1", "-1 dBTP";
});

choice!(SilenceThreshold {
    Minus60 => "-60", "Below -60 dB (only true silence)";
    Minus50 => "-50", "Below -50 dB";
    Minus40 => "-40", "Below -40 dB (also cuts quiet noise)";
});

choice!(OutputFolder {
    Beside => "beside", "Beside the original";
    Subfolder => "subfolder", "In a \"Sound Honter\" folder beside the original";
});

/// Name of the folder that `OutputFolder::Subfolder` saves into.
pub const SUBFOLDER_NAME: &str = "Sound Honter";

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Settings {
    pub mp3_quality: Mp3Quality,
    pub wav_depth: WavDepth,
    pub sample_rate: SampleRate,
    pub channels: Channels,
    pub lossy_quality: LossyQuality,
    pub loudness_target: LoudnessTarget,
    pub peak_ceiling: PeakCeiling,
    pub silence_threshold: SilenceThreshold,
    pub output_folder: OutputFolder,
    /// Added to the end of every new file's name, before the extension. Empty for none.
    pub name_suffix: String,
}

impl Settings {
    /// Reads the settings from `data_dir`, using defaults for anything missing.
    pub fn load(data_dir: &Path) -> Settings {
        let text = fs::read_to_string(data_dir.join(FILE_NAME)).unwrap_or_default();
        Settings::from_text(&text)
    }

    pub fn save(&self, data_dir: &Path) -> io::Result<()> {
        fs::create_dir_all(data_dir)?;
        fs::write(data_dir.join(FILE_NAME), self.to_text())
    }

    pub fn from_text(text: &str) -> Settings {
        let mut settings = Settings::default();
        for line in text.lines() {
            let Some((key, raw)) = line.split_once('=') else {
                continue;
            };
            let value = raw.trim();
            let s = &mut settings;
            match key.trim() {
                "mp3_quality" => set(&mut s.mp3_quality, Mp3Quality::from_key(value)),
                "wav_depth" => set(&mut s.wav_depth, WavDepth::from_key(value)),
                "sample_rate" => set(&mut s.sample_rate, SampleRate::from_key(value)),
                "channels" => set(&mut s.channels, Channels::from_key(value)),
                "lossy_quality" => set(&mut s.lossy_quality, LossyQuality::from_key(value)),
                "loudness_target" => set(&mut s.loudness_target, LoudnessTarget::from_key(value)),
                "peak_ceiling" => set(&mut s.peak_ceiling, PeakCeiling::from_key(value)),
                "silence_threshold" => {
                    set(&mut s.silence_threshold, SilenceThreshold::from_key(value))
                }
                "output_folder" => set(&mut s.output_folder, OutputFolder::from_key(value)),
                // Only the space after "=" is formatting: a suffix can itself start with a space.
                "name_suffix" => s.name_suffix = clean_suffix(raw.strip_prefix(' ').unwrap_or(raw)),
                _ => {}
            }
        }
        settings
    }

    pub fn to_text(&self) -> String {
        [
            ("mp3_quality", self.mp3_quality.key()),
            ("wav_depth", self.wav_depth.key()),
            ("sample_rate", self.sample_rate.key()),
            ("channels", self.channels.key()),
            ("lossy_quality", self.lossy_quality.key()),
            ("loudness_target", self.loudness_target.key()),
            ("peak_ceiling", self.peak_ceiling.key()),
            ("silence_threshold", self.silence_threshold.key()),
            ("output_folder", self.output_folder.key()),
            ("name_suffix", &self.name_suffix),
        ]
        .iter()
        .map(|(key, value)| format!("{key} = {value}\n"))
        .collect()
    }

    /// The `loudnorm` target for normalizing and mixing.
    pub fn loudnorm_target(&self) -> String {
        format!(
            "I={}:TP={}:LRA=11",
            self.loudness_target.key(),
            self.peak_ceiling.key()
        )
    }
}

fn set<T>(slot: &mut T, value: Option<T>) {
    if let Some(value) = value {
        *slot = value;
    }
}

/// Keeps a name suffix to characters that are allowed in file names.
pub fn clean_suffix(text: &str) -> String {
    text.chars()
        .filter(|c| !matches!(c, '<' | '>' | ':' | '"' | '/' | '\\' | '|' | '?' | '*'))
        .filter(|c| !c.is_control())
        .take(40)
        .collect::<String>()
        .trim_end_matches(['.', ' '])
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_match_how_sound_honter_has_always_behaved() {
        let s = Settings::default();
        assert_eq!(s.mp3_quality, Mp3Quality::Cbr192);
        assert_eq!(s.wav_depth, WavDepth::MatchSource);
        assert_eq!(s.sample_rate, SampleRate::Keep);
        assert_eq!(s.loudnorm_target(), "I=-16:TP=-3:LRA=11");
        assert_eq!(s.silence_threshold, SilenceThreshold::Minus60);
        assert_eq!(s.output_folder, OutputFolder::Beside);
        assert_eq!(s.name_suffix, "");
    }

    #[test]
    fn round_trips_through_text() {
        let settings = Settings {
            mp3_quality: Mp3Quality::VbrHigh,
            wav_depth: WavDepth::Float32,
            sample_rate: SampleRate::Hz48000,
            channels: Channels::Mono,
            lossy_quality: LossyQuality::Low,
            loudness_target: LoudnessTarget::Broadcast,
            peak_ceiling: PeakCeiling::Minus1,
            silence_threshold: SilenceThreshold::Minus40,
            output_folder: OutputFolder::Subfolder,
            name_suffix: " (master)".to_string(),
        };
        assert_eq!(Settings::from_text(&settings.to_text()), settings);
    }

    #[test]
    fn ignores_unknown_and_broken_lines() {
        let text = "mp3_quality = 999\nnonsense\nchannels=mono\nfuture_setting = 1\n";
        let settings = Settings::from_text(text);
        assert_eq!(settings.mp3_quality, Mp3Quality::Cbr192);
        assert_eq!(settings.channels, Channels::Mono);
    }

    #[test]
    fn suffixes_lose_characters_windows_forbids() {
        assert_eq!(clean_suffix("_v2: final?."), "_v2 final");
        assert_eq!(clean_suffix("a/b\\c"), "abc");
    }
}
