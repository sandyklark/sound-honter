//! Gathers a multi-file selection into a single job.
//!
//! For a right-click on several files, Explorer starts the app once per file. Each process appends
//! its file to a queue; the first becomes the *runner* and works through the queue while the others
//! exit straight away. Exclusive file opens serve as locks, so a crash can never leave one held.

use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::os::windows::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

const ERROR_ACCESS_DENIED: i32 = 5;
const ERROR_SHARING_VIOLATION: i32 = 32;
const LOCK_TIMEOUT: Duration = Duration::from_secs(10);
const POLL: Duration = Duration::from_millis(50);
/// A selection is complete once no new file has arrived for this long.
const SETTLE: Duration = Duration::from_millis(400);
/// Entries this old when a runner starts were abandoned by a crash, and must not be acted on.
const STALE_SECS: u64 = 60;

/// An exclusive handle on a file. Fails with `WouldBlock` if another process holds it.
fn open_exclusive(path: &Path) -> io::Result<File> {
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .share_mode(0)
        .open(path);
    file.map_err(|error| match error.raw_os_error() {
        Some(ERROR_SHARING_VIOLATION | ERROR_ACCESS_DENIED) => io::ErrorKind::WouldBlock.into(),
        _ => error,
    })
}

fn lock_with_retry(path: &Path) -> io::Result<File> {
    let deadline = Instant::now() + LOCK_TIMEOUT;
    loop {
        match open_exclusive(path) {
            Err(error)
                if error.kind() == io::ErrorKind::WouldBlock && Instant::now() < deadline =>
            {
                thread::sleep(Duration::from_millis(5));
            }
            result => return result,
        }
    }
}

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_secs())
}

fn read_text(file: &mut File) -> io::Result<String> {
    let mut bytes = Vec::new();
    file.seek(SeekFrom::Start(0))?;
    file.read_to_end(&mut bytes)?;
    Ok(String::from_utf8_lossy(&bytes).into_owned())
}

/// The process responsible for working through a queue.
pub struct Runner {
    queue_path: PathBuf,
    runner_lock: Option<File>,
    is_first_batch: bool,
}

/// Adds `file` to the named queue. Returns a `Runner` if this process should do the work, or
/// `None` if another process already is and will pick the file up.
pub fn submit(dir: &Path, name: &str, file: &Path) -> io::Result<Option<Runner>> {
    fs::create_dir_all(dir)?;
    let queue_path = dir.join(format!("{name}.queue"));
    {
        let mut queue = lock_with_retry(&queue_path)?;
        queue.seek(SeekFrom::End(0))?;
        writeln!(queue, "{}\t{}", now_secs(), file.display())?;
    }
    match open_exclusive(&dir.join(format!("{name}.lock"))) {
        Ok(runner_lock) => Ok(Some(Runner {
            queue_path,
            runner_lock: Some(runner_lock),
            is_first_batch: true,
        })),
        Err(error) if error.kind() == io::ErrorKind::WouldBlock => Ok(None),
        Err(error) => Err(error),
    }
}

impl Runner {
    /// Waits for the selection to finish arriving, then takes everything queued.
    ///
    /// Returns `None` once the queue is empty. The runner role is given up while the queue is
    /// locked: a submitter that failed to become runner has already written its entry, so that
    /// entry is guaranteed to be seen here, and no file can be stranded.
    pub fn next_batch(&mut self) -> io::Result<Option<Vec<PathBuf>>> {
        self.wait_until_settled();
        let mut queue = lock_with_retry(&self.queue_path)?;
        let text = read_text(&mut queue)?;
        let entries = parse_entries(&text, self.is_first_batch, now_secs());
        queue.set_len(0)?;
        self.is_first_batch = false;
        if entries.is_empty() {
            self.runner_lock = None;
            return Ok(None);
        }
        Ok(Some(entries))
    }

    fn wait_until_settled(&self) {
        let size = || fs::metadata(&self.queue_path).map_or(0, |m| m.len());
        let mut last_size = size();
        let mut last_change = Instant::now();
        while last_change.elapsed() < SETTLE {
            thread::sleep(POLL);
            let current = size();
            if current != last_size {
                last_size = current;
                last_change = Instant::now();
            }
        }
    }
}

fn parse_entries(text: &str, drop_stale: bool, now: u64) -> Vec<PathBuf> {
    let mut paths: Vec<PathBuf> = Vec::new();
    for line in text.lines() {
        let Some((time, path)) = line.split_once('\t') else {
            continue;
        };
        let age = now.saturating_sub(time.parse().unwrap_or(0));
        let path = PathBuf::from(path);
        if (!drop_stale || age <= STALE_SECS) && !paths.contains(&path) {
            paths.push(path);
        }
    }
    paths
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keeps_order_and_removes_duplicates() {
        let parsed = parse_entries("100\ta.wav\n100\tb.wav\n100\ta.wav\n", true, 100);
        assert_eq!(parsed, [PathBuf::from("a.wav"), PathBuf::from("b.wav")]);
    }

    #[test]
    fn drops_abandoned_entries_only_when_starting_up() {
        let text = "1\told.wav\n1000\tnew.wav\n";
        assert_eq!(parse_entries(text, true, 1000), [PathBuf::from("new.wav")]);
        // During a long batch, older entries are just files selected a while ago.
        assert_eq!(parse_entries(text, false, 1000).len(), 2);
    }

    #[test]
    fn ignores_corrupt_lines() {
        assert!(parse_entries("garbage\n\n", false, 0).is_empty());
    }
}
