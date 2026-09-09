//! A mutation lock, one per library.
//!
//! Two copies of this tool working on the same library at once can undo each
//! other's work: one reads the state, the other completes a repair, and the
//! first then acts on what it read a minute ago. Disabling a button in the
//! window does nothing about a second window or a terminal.
//!
//! The lock is a file created exclusively. It records the process that holds
//! it so a lock left behind by a crash can be told apart from a live one.

use std::fs;
use std::io::Write;
use std::path::PathBuf;
use std::process::Command;

use crate::state::app_data_dir;

pub struct Lock {
    path: PathBuf,
}

impl Lock {
    /// Take the lock for a library, or explain who has it.
    pub fn acquire(library_id: &str) -> Result<Lock, String> {
        let directory = app_data_dir().join("locks");
        fs::create_dir_all(&directory).map_err(|e| format!("{}: {e}", directory.display()))?;
        let path = directory.join(format!("{library_id}.lock"));

        match Self::create(&path) {
            Ok(()) => return Ok(Lock { path }),
            Err(error) if error.kind() != std::io::ErrorKind::AlreadyExists => {
                return Err(format!("{}: {error}", path.display()))
            }
            Err(_) => {}
        }

        // Somebody holds it, or held it and died.
        let holder = fs::read_to_string(&path).unwrap_or_default();
        let pid: Option<u32> = holder.lines().next().and_then(|line| line.trim().parse().ok());

        match pid.map(process_is_running) {
            Some(Some(false)) => {
                // Positively gone. Reclaim it once, and only once.
                let _ = fs::remove_file(&path);
                Self::create(&path)
                    .map(|()| Lock { path })
                    .map_err(|e| format!("{}: {e}", directory.display()))
            }
            Some(Some(true)) => Err(format!(
                "another LibraryBridge operation is working on this library (process {}). \
                 Wait for it to finish, or close the other window.",
                pid.unwrap_or(0)
            )),
            _ => Err(format!(
                "this library is locked by {}, and whether that process is still running \
                 could not be determined. If nothing else is using LibraryBridge, delete \
                 that file and try again.",
                path.display()
            )),
        }
    }

    fn create(path: &PathBuf) -> std::io::Result<()> {
        let mut file = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(path)?;
        writeln!(file, "{}", std::process::id())?;
        writeln!(file, "librarybridge")?;
        file.sync_all()
    }
}

impl Drop for Lock {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.path);
    }
}

/// `Some(true)` running, `Some(false)` definitely gone, `None` cannot tell.
/// The difference matters: a lock is only reclaimed on a definite answer.
fn process_is_running(pid: u32) -> Option<bool> {
    let proc_entry = PathBuf::from(format!("/proc/{pid}"));
    if PathBuf::from("/proc").is_dir() {
        return Some(proc_entry.exists());
    }
    let output = Command::new("ps")
        .args(["-p", &pid.to_string(), "-o", "comm="])
        .output()
        .ok()?;
    if !output.status.success() {
        // ps exits non-zero when no such process matched.
        return Some(false);
    }
    Some(!String::from_utf8_lossy(&output.stdout).trim().is_empty())
}
