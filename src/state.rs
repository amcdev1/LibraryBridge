//! Reading a library's repair state straight off the disk.
//!
//! Because the repair never deletes anything, every state a crash can leave
//! behind is distinguishable with `lstat` on two paths. That is what replaces
//! a journal or a transaction log.

use std::fs;
use std::path::{Path, PathBuf};

use crate::steam::Library;

/// Where this tool keeps its own files.
pub fn app_data_dir() -> PathBuf {
    let base = match std::env::var_os("XDG_DATA_HOME") {
        Some(dir) if Path::new(&dir).is_absolute() => PathBuf::from(dir),
        _ => PathBuf::from(std::env::var_os("HOME").unwrap_or_default()).join(".local/share"),
    };
    base.join("librarybridge")
}

pub const BACKUP_PREFIX: &str = "compatdata.backup";
pub const RESTORE_STAGING: &str = "compatdata.restoring";

/// A fresh restore directory, unique per run for the same reason the copy
/// staging is: a leftover is never assumed to be ours to remove.
pub fn new_restore_path(steamapps: &Path) -> PathBuf {
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.subsec_nanos())
        .unwrap_or(0);
    steamapps.join(format!(
        "{RESTORE_STAGING}-{}-{stamp}",
        std::process::id()
    ))
}
/// A destination left behind by an earlier repair that was undone.
pub const PREVIOUS_PREFIX: &str = "compatdata.previous";

#[derive(Debug, Clone)]
pub enum State {
    /// The drive is not plugged in, so nothing can be said about it.
    Disconnected,
    /// No compatdata at all. Steam has not run a Proton game from here yet.
    NoCompatdata,
    /// A real directory on the game drive. This is what gets repaired.
    NotRepaired { prefixes: usize },
    /// Repaired, and the link points where this tool would put it.
    Repaired { target: PathBuf, prefixes: usize },
    /// A symlink that resolves, but not to the path this tool derives.
    /// Somebody set this up by hand. It is left alone.
    LinkedElsewhere { target: PathBuf },
    /// A symlink whose target is not there: the home drive is unmounted, or
    /// the destination was deleted.
    DanglingLink { target: PathBuf },
    /// Interrupted between moving the original aside and creating the link.
    InterruptedAwaitingLink { backup: PathBuf },
    /// Something that is not a directory or a symlink.
    Unusable { detail: String },
}

impl State {
    pub fn headline(&self) -> &'static str {
        match self {
            State::Disconnected => "Drive not connected",
            State::NoCompatdata => "No Proton data yet",
            State::NotRepaired { .. } => "Repair available",
            State::Repaired { .. } => "Repaired",
            State::LinkedElsewhere { .. } => "Already linked elsewhere",
            State::DanglingLink { .. } => "Link is broken",
            State::InterruptedAwaitingLink { .. } => "Interrupted, needs finishing",
            State::Unusable { .. } => "Needs attention",
        }
    }

    pub fn code(&self) -> &'static str {
        match self {
            State::Disconnected => "disconnected",
            State::NoCompatdata => "no_compatdata",
            State::NotRepaired { .. } => "repair_available",
            State::Repaired { .. } => "repaired",
            State::LinkedElsewhere { .. } => "linked_elsewhere",
            State::DanglingLink { .. } => "dangling_link",
            State::InterruptedAwaitingLink { .. } => "interrupted",
            State::Unusable { .. } => "unusable",
        }
    }
}

#[derive(Debug, Clone)]
pub struct Report {
    pub state: State,
    /// Originals kept beside the library by earlier repairs.
    pub backups: Vec<PathBuf>,
    /// A copy that was interrupted before it could be verified. Safe to
    /// remove, because the source it came from is still in place.
    pub abandoned_copy: Option<PathBuf>,
    /// A copy-back that stopped part way, left beside the library. An undo
    /// leaves this behind, and until now nothing reported it.
    pub interrupted_restore: Option<PathBuf>,
    /// Data already sitting at the destination while the library is not
    /// repaired. Either a leftover from an undone repair, or the migrated
    /// prefixes from a repair that was interrupted and then overtaken by
    /// Steam recreating compatdata.
    pub existing_destination: Option<PathBuf>,
}

pub fn inspect(library: &Library) -> Report {
    let backups = find_backups(&library.steamapps);
    let target = library.effective_target();
    let abandoned_copy = find_staging(&target);
    let interrupted_restore = find_named(&library.steamapps, RESTORE_STAGING);
    let existing_destination = if fs::symlink_metadata(&target).is_ok() {
        Some(target.clone())
    } else {
        None
    };

    if !library.connected {
        return Report {
            state: State::Disconnected,
            backups,
            abandoned_copy,
            interrupted_restore,
            existing_destination,
        };
    }

    let compatdata = &library.compatdata;
    let state = match fs::symlink_metadata(compatdata) {
        Err(_) => {
            if let Some(backup) = backups.first() {
                State::InterruptedAwaitingLink {
                    backup: backup.clone(),
                }
            } else {
                State::NoCompatdata
            }
        }
        Ok(meta) if meta.file_type().is_symlink() => match fs::read_link(compatdata) {
            Err(e) => State::Unusable {
                detail: format!("cannot read the link: {e}"),
            },
            // Named for what it is: the text stored in the link, which is
            // not the destination this tool would choose.
            Ok(link_text) => {
                let resolved = resolve_link(compatdata, &link_text);
                if !resolved.exists() {
                    State::DanglingLink { target: resolved }
                } else if !resolved.is_dir() {
                    // A link to a file is not a relocated compatdata, however
                    // much its name suggests otherwise.
                    State::Unusable {
                        detail: format!(
                            "compatdata is a link to {}, which is not a directory",
                            resolved.display()
                        ),
                    }
                } else if library.owns(&resolved) {
                    State::Repaired {
                        prefixes: count_prefixes(&resolved),
                        target: resolved,
                    }
                } else {
                    State::LinkedElsewhere { target: resolved }
                }
            }
        },
        Ok(meta) if meta.file_type().is_dir() => State::NotRepaired {
            prefixes: count_prefixes(compatdata),
        },
        Ok(meta) => State::Unusable {
            detail: format!(
                "compatdata is a {:?}, not a directory or a link",
                meta.file_type()
            ),
        },
    };

    // Once repaired, the destination is simply where the data lives.
    let existing_destination = match state {
        State::Repaired { .. } => None,
        _ => existing_destination,
    };

    Report {
        state,
        backups,
        abandoned_copy,
        interrupted_restore,
        existing_destination,
    }
}

fn resolve_link(link_path: &Path, target: &Path) -> PathBuf {
    if target.is_absolute() {
        target.to_path_buf()
    } else {
        link_path.parent().unwrap_or(Path::new(".")).join(target)
    }
}

/// Prefix directories are named after the app id that owns them. Anything
/// else in there is counted too, because unknown data still has to be moved.
fn count_prefixes(compatdata: &Path) -> usize {
    fs::read_dir(compatdata)
        .map(|d| d.flatten().count())
        .unwrap_or(0)
}

/// Which backup a directory is. `compatdata.backup` is the first,
/// `compatdata.backup-2` the second, and so on. Anything else is not ours.
///
/// A backup a person made by hand under the same name counts, which is what
/// you want: it is still the original data sitting beside the library, and
/// treating it as one only ever leads to it being listed and left alone.
fn backup_number(name: &str) -> Option<u32> {
    let rest = name.strip_prefix(BACKUP_PREFIX)?;
    if rest.is_empty() {
        return Some(1);
    }
    rest.strip_prefix('-')?.parse().ok()
}

/// Backups beside a library, newest first.
/// Copies left behind by an interrupted run, beside the destination.
fn find_staging(target: &Path) -> Option<PathBuf> {
    let parent = target.parent()?;
    find_named(parent, crate::steam::STAGING_PREFIX)
}

fn find_named(directory: &Path, prefix: &str) -> Option<PathBuf> {
    fs::read_dir(directory).ok()?.flatten().find_map(|entry| {
        entry
            .file_name()
            .to_string_lossy()
            .starts_with(prefix)
            .then(|| entry.path())
    })
}

pub fn find_backups(steamapps: &Path) -> Vec<PathBuf> {
    let Ok(entries) = fs::read_dir(steamapps) else {
        return Vec::new();
    };
    let mut found: Vec<(u32, PathBuf)> = entries
        .flatten()
        .filter_map(|entry| {
            let number = backup_number(&entry.file_name().to_string_lossy())?;
            Some((number, entry.path()))
        })
        .collect();
    found.sort_by(|a, b| b.0.cmp(&a.0));
    found.into_iter().map(|(_, path)| path).collect()
}

/// The next free name of a numbered series in `parent`.
///
/// Counting beats a timestamp here: the name stays short enough to read at a
/// glance, and the order is still obvious.
pub fn next_free(parent: &Path, base: &str) -> PathBuf {
    let taken = |path: &Path| fs::symlink_metadata(path).is_ok();

    let first = parent.join(base);
    if !taken(&first) {
        return first;
    }
    for number in 2..1000 {
        let candidate = parent.join(format!("{base}-{number}"));
        if !taken(&candidate) {
            return candidate;
        }
    }
    // A thousand of them beside one library means something is wrong, but a
    // name that cannot collide is still better than overwriting one.
    parent.join(format!("{base}-{}", std::process::id()))
}

pub fn new_backup_path(steamapps: &Path) -> PathBuf {
    next_free(steamapps, BACKUP_PREFIX)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recognises_its_own_backup_names() {
        assert_eq!(backup_number("compatdata.backup"), Some(1));
        assert_eq!(backup_number("compatdata.backup-2"), Some(2));
        assert_eq!(backup_number("compatdata.backup-17"), Some(17));
        assert_eq!(backup_number("compatdata"), None);
        assert_eq!(backup_number("compatdata.backup-old"), None);
        assert_eq!(backup_number("compatdata.incomplete"), None);
    }

    #[test]
    fn numbers_the_next_backup_and_lists_newest_first() {
        let dir = std::env::temp_dir().join(format!("lb-backups-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();

        assert_eq!(new_backup_path(&dir), dir.join("compatdata.backup"));
        fs::create_dir(dir.join("compatdata.backup")).unwrap();
        assert_eq!(new_backup_path(&dir), dir.join("compatdata.backup-2"));
        fs::create_dir(dir.join("compatdata.backup-2")).unwrap();
        assert_eq!(new_backup_path(&dir), dir.join("compatdata.backup-3"));

        // Ordering is numeric, so ten sorts after two rather than before it.
        fs::create_dir(dir.join("compatdata.backup-10")).unwrap();
        let backups = find_backups(&dir);
        assert_eq!(
            backups
                .first()
                .map(|p| p.file_name().unwrap().to_string_lossy().to_string()),
            Some("compatdata.backup-10".to_string())
        );
        assert_eq!(backups.len(), 3);

        let _ = fs::remove_dir_all(&dir);
    }
}
