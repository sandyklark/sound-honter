# Sound Honter

Convert and mix audio files from the Windows right-click menu.

## Using it

Right-click one or more audio files and open the **Sound Honter** submenu. On Windows 11, choose
**Show more options** first.

- **Convert to MP3, WAV, FLAC, OGG or M4A:** makes a copy of each selected file in that format.
- **Mix selected files:** layers two or more files into one 24-bit stereo WAV, as long as the
  longest file and normalized for loudness.

Windows hides the submenu when more than 15 files are selected. For bigger selections, use
**Send to > Sound Honter Mix** or **Sound Honter Convert to MP3**.

New files are saved beside the originals, and nothing is ever overwritten: if `take.mp3` exists,
you get `take (1).mp3`.

### What the formats do

| Format | Output |
| --- | --- |
| MP3 | 192 kbps, 44.1 kHz, stereo |
| WAV | Keeps the source's sample rate and channels; 16-bit if the source is, otherwise 24-bit |
| FLAC | Lossless; keeps the source's sample rate and channels |
| OGG | Vorbis, quality 6 |
| M4A | AAC, 192 kbps |

Mixes aim for -16 LUFS with peaks no higher than -3 dBTP, using one clean gain change rather than
a limiter. Short, punchy sounds can land a little under -16 LUFS because the peak ceiling is
reached first.

## Developing

You need [rustup](https://rustup.rs) and [Inno Setup 6](https://jrsoftware.org/isinfo.php).
Visual Studio isn't needed.

```powershell
scripts\build.ps1            # Runs all checks and builds dist\SoundHonter-<version>-Setup.exe
scripts\build.ps1 -SkipTests # Builds without the checks
scripts\test-installer.ps1   # Installs, clicks the real right-click menu, then uninstalls
```

`test-installer.ps1` briefly changes your real right-click menu and Send To folder, and dialogs
pop up while it runs.

To run the tests on their own, first run `scripts\fetch-ffmpeg.ps1` to download FFmpeg, then:

```powershell
cargo +1.99.1-x86_64-pc-windows-gnu test
```

### Layout

| Path | Contents |
| --- | --- |
| `src/engine.rs` | FFmpeg commands for converting and mixing |
| `src/queue.rs` | Gathers Explorer's one-process-per-file launches into one job |
| `src/job.rs` | Runs a job and reports progress |
| `src/ui.rs`, `src/main.rs` | Dialogs and command line |
| `examples/stamp.rs` | Build tool that adds the icon and version info to the exe |
| `packaging/sound-honter.iss` | Installer, including the right-click menu entries |
| `tests/e2e.rs` | Runs the real app and FFmpeg, then measures the audio that comes out |
