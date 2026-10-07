//! Gateway log: each line goes to stdout and, once `init` is called, to a daily file
//! (`nx-azure-cache.YYYY-MM-DD.log`, UTC date) kept for 7 days.
//! Callers never write a secret to it: no token, no header, no signed URL.

use azure_core::time::{OffsetDateTime, to_rfc3339};
use std::{fs::File, io::Write, path::PathBuf, sync::Mutex};

/// Number of daily files kept, today's included.
const KEEP: usize = 7;

struct Journal {
    dir: PathBuf,
    day: String,
    file: Option<File>,
}

// ponytail: global lock and blocking write, enough for one line per request.
static JOURNAL: Mutex<Option<Journal>> = Mutex::new(None);

/// Enables copying lines into `dir` (created if needed).
pub fn init(dir: PathBuf) -> Result<(), String> {
    std::fs::create_dir_all(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    *lock() = Some(Journal {
        dir,
        day: String::new(),
        file: None,
    });
    Ok(())
}

/// Writes a timestamped (UTC) line to stdout and to the day's file.
/// Never panics: a closed stdout (dead conhost, EPIPE) is ignored, otherwise a call made
/// under the Identity lock would poison it.
pub fn line(text: &str) {
    let now = OffsetDateTime::now_utc();
    let text = format!("{} {text}", to_rfc3339(&now));
    writeln!(std::io::stdout(), "{text}").ok();
    if let Some(journal) = lock().as_mut() {
        journal.write(&now.date().to_string(), &text);
    }
}

fn lock() -> std::sync::MutexGuard<'static, Option<Journal>> {
    JOURNAL.lock().unwrap_or_else(|e| e.into_inner())
}

impl Journal {
    /// Switches file when the day changes, then keeps only the `KEEP` most recent.
    fn write(&mut self, day: &str, text: &str) {
        if self.day != day {
            self.day = day.to_owned();
            let path = self.dir.join(format!("nx-azure-cache.{day}.log"));
            self.file = File::options()
                .create(true)
                .append(true)
                .open(&path)
                .map_err(|e| writeln!(std::io::stderr(), "log {}: {e}", path.display()).ok())
                .ok();
            self.prune();
        }
        if let Some(file) = &mut self.file {
            let _ = writeln!(file, "{text}");
        }
    }

    fn prune(&self) {
        let Ok(entries) = std::fs::read_dir(&self.dir) else {
            return;
        };
        let mut logs: Vec<PathBuf> = entries
            .flatten()
            .map(|e| e.path())
            .filter(|p| {
                p.file_name()
                    .and_then(|n| n.to_str())
                    .is_some_and(|n| n.starts_with("nx-azure-cache.") && n.ends_with(".log"))
            })
            .collect();
        // ISO dates: name order is chronological order.
        logs.sort();
        for old in &logs[..logs.len().saturating_sub(KEEP)] {
            let _ = std::fs::remove_file(old);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn one_file_per_day_seven_days_kept() {
        let dir =
            std::env::temp_dir().join(format!("nx-azure-cache-journal-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let mut journal = Journal {
            dir: dir.clone(),
            day: String::new(),
            file: None,
        };
        for d in 1..=9 {
            let day = format!("2026-01-0{d}");
            journal.write(&day, &format!("line {d}a"));
            journal.write(&day, &format!("line {d}b"));
        }
        let mut names: Vec<String> = std::fs::read_dir(&dir)
            .unwrap()
            .map(|e| e.unwrap().file_name().into_string().unwrap())
            .collect();
        names.sort();
        let expected: Vec<String> = (3..=9)
            .map(|d| format!("nx-azure-cache.2026-01-0{d}.log"))
            .collect();
        assert_eq!(names, expected);
        let last = std::fs::read_to_string(dir.join(&expected[6])).unwrap();
        assert_eq!(last, "line 9a\nline 9b\n");
        drop(journal);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
