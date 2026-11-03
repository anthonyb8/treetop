//! What `treetop status` prints, kept fresh by whatever already reads the
//! pool: an open treetop and `treetop serve` each write their workspace's
//! status every EVERY once they have read it in full. Building it from
//! nothing costs seconds, `treehouse status` alone most of them, and the
//! assistant asks for it every turn, so `status` prints a snapshot no older
//! than FRESH and only builds its own when none is.

use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime};

use serde_json::Value;

/// How old a snapshot `status` still prints.
pub const FRESH: Duration = Duration::from_secs(10);

/// How often a reader writes one.
const EVERY: Duration = Duration::from_secs(2);

fn file(checkout: &Path) -> Option<PathBuf> {
    Some(crate::cache::dir()?.join(format!("{}.status.json", crate::cache::key(checkout))))
}

/// Writes `status` for the workspace at `checkout`, through a temporary file
/// and a rename, so a reader never sees half of it. A failure only means
/// `status` builds its own.
pub fn write(checkout: &Path, status: &Value) {
    let Some(target) = file(checkout) else {
        return;
    };
    let write = || -> std::io::Result<()> {
        if let Some(dir) = target.parent() {
            fs::create_dir_all(dir)?;
        }
        let partial = target.with_extension("partial");
        fs::write(&partial, serde_json::to_string_pretty(status)?)?;
        fs::rename(partial, &target)
    };
    let _ = write();
}

/// The workspace's snapshot, when one was written within FRESH.
pub fn fresh(checkout: &Path) -> Option<String> {
    let target = file(checkout)?;
    let age = SystemTime::now()
        .duration_since(fs::metadata(&target).ok()?.modified().ok()?)
        .ok()?;
    (age <= FRESH)
        .then(|| fs::read_to_string(&target).ok())
        .flatten()
}

/// Says when the next snapshot is due, so one is written at most every
/// EVERY however often the pool changes.
#[derive(Default)]
pub struct Clock(Option<Instant>);

impl Clock {
    pub fn is_due(&mut self) -> bool {
        let is_due = self.0.is_none_or(|last| last.elapsed() >= EVERY);
        if is_due {
            self.0 = Some(Instant::now());
        }
        is_due
    }
}
