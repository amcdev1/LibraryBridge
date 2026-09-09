//! Finding installed games that Lutris does not know about.
//!
//! Providers run in descending order of confidence. Nothing discovered here
//! is ever executed; files are only listed and read.

use std::fs;
use std::path::{Path, PathBuf};

use crate::json;
use crate::lutris::{self, Entry};
use crate::steam::{self, short_id};
use crate::system::{self, Capability};

const MAX_DEPTH: usize = 6;
const MAX_ENTRIES_PER_FOLDER: usize = 40_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Confidence {
    Low,
    Medium,
    High,
}

impl Confidence {
    pub fn label(self) -> &'static str {
        match self {
            Confidence::High => "high",
            Confidence::Medium => "medium",
            Confidence::Low => "low",
        }
    }
}

#[derive(Debug, Clone)]
pub struct Candidate {
    pub id: String,
    pub name: String,
    pub runner: String,
    pub source: &'static str,
    pub exe: Option<PathBuf>,
    pub appid: Option<String>,
    pub prefix: Option<PathBuf>,
    pub working_dir: Option<PathBuf>,
    pub confidence: Confidence,
    pub reasons: Vec<String>,
    pub in_lutris: bool,
    /// Whether this game can actually be imported. Detection and eligibility
    /// are different questions, and the window used to only know the first.
    pub eligible: bool,
    pub blocking_reason: Option<String>,
    pub filesystem_warning: Option<String>,
    /// Other executables in the same folder, for when the guess is wrong.
    pub alternatives: Vec<PathBuf>,
}

/// Scan the selected roots plus every Steam library, and mark anything Lutris
/// already has.
pub fn scan(roots: &[PathBuf], steam_root: Option<&Path>, existing: &[Entry]) -> Vec<Candidate> {
    let mut candidates = Vec::new();

    let (libraries, _) = steam::all_libraries(steam_root);
    for library in &libraries {
        if library.connected {
            candidates.extend(steam_games(library));
        }
    }

    for root in roots {
        candidates.extend(scan_root(root));
    }

    // A folder inside a Steam library is already covered by its manifest.
    let steam_dirs: Vec<PathBuf> = libraries
        .iter()
        .map(|l| l.steamapps.join("common"))
        .collect();
    candidates.retain(|c| {
        c.source == "steam"
            || !c
                .working_dir
                .as_ref()
                .map(|d| steam_dirs.iter().any(|s| d.starts_with(s)))
                .unwrap_or(false)
    });

    for candidate in &mut candidates {
        candidate.in_lutris = matches_existing(candidate, existing);
        candidate.filesystem_warning = filesystem_warning(candidate);
        candidate.blocking_reason = import_blocker(candidate);
        candidate.eligible = candidate.blocking_reason.is_none();
    }

    candidates.sort_by(|a, b| {
        b.confidence
            .cmp(&a.confidence)
            .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
    });
    // Ids identify an executable, and the same executable can be reached by
    // more than one route. Deduplicate across the whole list, not just
    // neighbours, so a game found twice is reported once.
    let mut seen: Vec<String> = Vec::new();
    candidates.retain(|candidate| {
        if seen.contains(&candidate.id) {
            false
        } else {
            seen.push(candidate.id.clone());
            true
        }
    });
    candidates
}

fn matches_existing(candidate: &Candidate, existing: &[Entry]) -> bool {
    existing.iter().any(|entry| {
        if let (Some(a), Some(b)) = (&candidate.appid, &entry.appid) {
            if a == b {
                return true;
            }
        }
        if let (Some(a), Some(b)) = (&candidate.exe, &entry.exe) {
            if a == b {
                return true;
            }
        }
        // Two entries sharing a Wine prefix and a game directory are the same
        // installation seen twice.
        if let (Some(a), Some(b)) = (&candidate.prefix, &entry.prefix) {
            if a == b
                && entry
                    .exe
                    .as_ref()
                    .map(|e| e.starts_with(a))
                    .unwrap_or(false)
            {
                return true;
            }
        }
        entry.slug == lutris::slugify(&candidate.name)
    })
}

/// Why a detected game cannot be imported, or `None` if it can.
///
/// Detection and eligibility are separate. A Steam game is worth listing so
/// the user can see it was found, and is not worth importing because Lutris
/// already lists installed Steam games through its own Steam source.
fn import_blocker(candidate: &Candidate) -> Option<String> {
    if candidate.source == "steam" {
        return Some(
            "Lutris lists installed Steam games through its own Steam source, so importing this would create a second entry for it"
                .to_string(),
        );
    }
    if candidate.in_lutris {
        return Some("Lutris already has this game".to_string());
    }
    None
}

/// Registering a game does not make an unusable prefix usable. Say so where
/// the filesystem underneath is one this tool knows is a problem.
fn filesystem_warning(candidate: &Candidate) -> Option<String> {
    let subject = candidate
        .prefix
        .as_ref()
        .or(candidate.working_dir.as_ref())?;
    let mount = system::mount_for(subject)?;
    match mount.capability {
        Capability::NeedsRepair => Some(format!(
            "the prefix for this game is on {}, where Proton and Wine data does not work reliably",
            mount.fs_type
        )),
        Capability::NoSymlinks => Some(format!(
            "the prefix for this game is on {}, which cannot hold a working Wine prefix",
            mount.fs_type
        )),
        _ => None,
    }
}

// ------------------------------------------------------------ Steam provider

fn steam_games(library: &steam::Library) -> Vec<Candidate> {
    let mut found = Vec::new();
    for (appid, name) in steam::app_names(library) {
        let install_dir = library.steamapps.join("common");
        found.push(Candidate {
            id: short_id(Path::new(&format!("steam:{appid}"))),
            name: name.clone(),
            runner: "steam".to_string(),
            source: "steam",
            exe: None,
            appid: Some(appid.clone()),
            prefix: None,
            working_dir: Some(install_dir),
            confidence: Confidence::High,
            reasons: vec![format!(
                "Steam lists it as installed in {} (app {appid})",
                library.display_name()
            )],
            in_lutris: false,
            eligible: true,
            blocking_reason: None,
            filesystem_warning: None,
            alternatives: Vec::new(),
        });
    }
    found
}

// ------------------------------------------------------- folder-based scanning

fn scan_root(root: &Path) -> Vec<Candidate> {
    let mut found = Vec::new();
    if !root.is_dir() {
        return found;
    }

    // A root is normally a container of game folders, so its children are
    // examined first. Only when none of them holds a game is the root itself
    // treated as one, which is what happens when the user points straight at
    // a single game. Doing both would report the same executable twice.
    if let Ok(children) = fs::read_dir(root) {
        // `is_dir` follows links, so a child that is a symlink to a directory
        // outside the chosen folder would be scanned. The recursive walker
        // already refuses links; this is the entry point it never sees.
        let mut dirs: Vec<PathBuf> = children
            .flatten()
            .map(|e| e.path())
            .filter(|p| {
                fs::symlink_metadata(p)
                    .map(|meta| meta.is_dir())
                    .unwrap_or(false)
                    && !is_noise_dir(p)
            })
            .collect();
        dirs.sort();
        for dir in dirs {
            if let Some(candidate) = examine_folder(&dir) {
                found.push(candidate);
            }
        }
    }
    if found.is_empty() {
        if let Some(candidate) = examine_folder(root) {
            found.push(candidate);
        }
    }
    found
}

fn examine_folder(folder: &Path) -> Option<Candidate> {
    let files = list_files(folder)?;

    if let Some(candidate) = gog_candidate(folder, &files) {
        return Some(candidate);
    }

    let executables: Vec<&PathBuf> = files
        .iter()
        .filter(|p| {
            p.extension()
                .map(|e| e.eq_ignore_ascii_case("exe"))
                .unwrap_or(false)
        })
        .collect();

    if !executables.is_empty() {
        return windows_candidate(folder, &executables);
    }
    linux_candidate(folder, &files)
}

fn list_files(folder: &Path) -> Option<Vec<PathBuf>> {
    let mut files = Vec::new();
    let mut stack = vec![(folder.to_path_buf(), 0usize)];
    while let Some((dir, depth)) = stack.pop() {
        if depth > MAX_DEPTH || files.len() > MAX_ENTRIES_PER_FOLDER {
            continue;
        }
        let Ok(entries) = fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let Ok(meta) = fs::symlink_metadata(&path) else {
                continue;
            };
            if meta.file_type().is_symlink() {
                continue; // never traverse links while scanning
            }
            if meta.is_dir() {
                if !is_noise_dir(&path) {
                    stack.push((path, depth + 1));
                }
            } else if meta.is_file() {
                files.push(path);
            }
        }
    }
    if files.is_empty() {
        None
    } else {
        Some(files)
    }
}

const NOISE_DIRS: [&str; 12] = [
    "redist",
    "_commonredist",
    "commonredist",
    "directx",
    "dotnet",
    "vcredist",
    "__installer",
    "__redist",
    "prerequisites",
    "prereq",
    "support",
    "steamapps",
];

fn is_noise_dir(path: &Path) -> bool {
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().to_lowercase())
        .unwrap_or_default();
    name.starts_with('.') || NOISE_DIRS.contains(&name.as_str())
}

// --------------------------------------------------------------- GOG provider

fn gog_candidate(folder: &Path, files: &[PathBuf]) -> Option<Candidate> {
    let info = files.iter().find(|p| {
        let name = p
            .file_name()
            .map(|n| n.to_string_lossy().to_lowercase())
            .unwrap_or_default();
        name.starts_with("goggame-") && name.ends_with(".info")
    })?;
    let text = fs::read_to_string(info).ok()?;
    let parsed = json::parse(&text).ok()?;
    let name = parsed.string("name")?;

    let primary = parsed
        .get("playTasks")
        .map(|tasks| tasks.as_array())
        .unwrap_or_default()
        .iter()
        .find(|task| task.get("isPrimary").and_then(json::Json::as_bool) == Some(true))
        .or_else(|| {
            parsed
                .get("playTasks")
                .map(|tasks| tasks.as_array())
                .unwrap_or_default()
                .first()
        });

    let relative = primary.and_then(|task| task.string("path"))?;
    let exe = folder.join(relative.replace('\\', "/"));
    // The path comes out of a metadata file on disk, so it is untrusted: it
    // must not lead out of the folder being examined.
    if !within(folder, &exe) {
        return None;
    }
    if !fs::symlink_metadata(&exe)
        .map(|meta| meta.is_file())
        .unwrap_or(false)
    {
        return None;
    }

    let game_dir = exe.parent().unwrap_or(folder).to_path_buf();
    Some(Candidate {
        id: short_id(&exe),
        name,
        runner: if is_windows_exe(&exe) {
            "wine".into()
        } else {
            "linux".into()
        },
        source: "gog",
        prefix: find_prefix(&exe),
        working_dir: Some(game_dir),
        exe: Some(exe),
        appid: None,
        confidence: Confidence::High,
        reasons: vec![
            "GOG metadata in the folder names the game and its launch executable".to_string(),
        ],
        in_lutris: false,
        eligible: true,
        blocking_reason: None,
        filesystem_warning: None,
        alternatives: Vec::new(),
    })
}

/// Does `path` stay inside `root` once both are fully resolved?
fn within(root: &Path, path: &Path) -> bool {
    let Ok(root) = root.canonicalize() else {
        return false;
    };
    match path.canonicalize() {
        Ok(resolved) => resolved.starts_with(&root),
        Err(_) => false,
    }
}

fn is_windows_exe(path: &Path) -> bool {
    path.extension()
        .map(|e| e.eq_ignore_ascii_case("exe"))
        .unwrap_or(false)
}

// ------------------------------------------------- generic Windows game folder

/// Executables that are never the game.
const NEVER: [&str; 16] = [
    "unins",
    "uninstall",
    "setup",
    "vcredist",
    "dxsetup",
    "dxwebsetup",
    "directx",
    "dotnetfx",
    "oalinst",
    "crashreport",
    "crashpad",
    "prereq",
    "vc_redist",
    "quicksfv",
    "cleanup",
    "dependencies",
];

/// Executables that exist alongside the game but are usually not it.
const UNLIKELY: [&str; 8] = [
    "launcher",
    "config",
    "settings",
    "editor",
    "server",
    "updater",
    "patcher",
    "benchmark",
];

fn windows_candidate(folder: &Path, executables: &[&PathBuf]) -> Option<Candidate> {
    let folder_key = normalize(&folder.file_name()?.to_string_lossy());

    let mut scored: Vec<(i64, &PathBuf, Vec<String>)> = Vec::new();
    for exe in executables {
        let stem = exe.file_stem()?.to_string_lossy().to_lowercase();
        let full = exe.to_string_lossy().to_lowercase();
        if NEVER
            .iter()
            .any(|bad| stem.contains(bad) || full.contains(&format!("/{bad}")))
        {
            continue;
        }

        let mut score = 0i64;
        let mut reasons = Vec::new();

        let depth = exe
            .strip_prefix(folder)
            .map(|r| r.components().count())
            .unwrap_or(9);
        if depth <= 1 {
            score += 25;
            reasons.push("sits at the top of the game folder".to_string());
        } else if depth > 3 {
            score -= 20;
        }

        if normalize(&stem) == folder_key && !folder_key.is_empty() {
            score += 50;
            reasons.push("its name matches the folder name".to_string());
        }
        if stem.ends_with("-win64-shipping") || stem.ends_with("-win32-shipping") {
            score += 45;
            reasons.push("it is an Unreal Engine shipping build".to_string());
        }
        if matches!(stem.as_str(), "game" | "start" | "play") {
            score += 15;
        }
        if UNLIKELY.iter().any(|weak| stem.contains(weak)) {
            score -= 35;
            reasons.push("its name suggests a helper rather than the game".to_string());
        }

        let size = fs::metadata(exe).map(|m| m.len()).unwrap_or(0);
        score += ((size / (1024 * 1024)) as i64).min(20);

        scored.push((score, exe, reasons));
    }

    scored.sort_by(|a, b| b.0.cmp(&a.0));
    let (best_score, best, mut reasons) = scored.first().cloned()?;

    let margin = best_score - scored.get(1).map(|s| s.0).unwrap_or(-100);
    let confidence = if scored.len() == 1 && best_score >= 25 {
        reasons.push("it is the only launchable executable in the folder".to_string());
        Confidence::High
    } else if margin >= 30 {
        Confidence::Medium
    } else {
        reasons.push(format!(
            "{} executables looked plausible, so this one is a guess",
            scored.len()
        ));
        Confidence::Low
    };

    let name = pretty_name(folder, best);
    Some(Candidate {
        id: short_id(best),
        name,
        runner: "wine".to_string(),
        source: "folder",
        prefix: find_prefix(best),
        working_dir: best.parent().map(Path::to_path_buf),
        exe: Some(best.to_path_buf()),
        appid: None,
        confidence,
        reasons,
        in_lutris: false,
        eligible: true,
        blocking_reason: None,
        filesystem_warning: None,
        alternatives: scored
            .iter()
            .skip(1)
            .take(5)
            .map(|s| s.1.to_path_buf())
            .collect(),
    })
}

fn pretty_name(folder: &Path, exe: &Path) -> String {
    let folder_name = folder
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_default();
    if folder_name.len() > 2 {
        folder_name.replace('_', " ").trim().to_string()
    } else {
        exe.file_stem()
            .map(|s| s.to_string_lossy().to_string())
            .unwrap_or_default()
    }
}

fn normalize(text: &str) -> String {
    text.chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .map(|c| c.to_ascii_lowercase())
        .collect()
}

// ------------------------------------------------------- native Linux provider

fn linux_candidate(folder: &Path, files: &[PathBuf]) -> Option<Candidate> {
    use std::os::unix::fs::PermissionsExt;

    let launcher = files.iter().find(|path| {
        let name = path
            .file_name()
            .map(|n| n.to_string_lossy().to_lowercase())
            .unwrap_or_default();
        let executable = fs::metadata(path)
            .map(|m| m.permissions().mode() & 0o111 != 0)
            .unwrap_or(false);
        executable && (name == "start.sh" || name == "run.sh" || name.ends_with(".x86_64"))
    })?;

    Some(Candidate {
        id: short_id(launcher),
        name: pretty_name(folder, launcher),
        runner: "linux".to_string(),
        source: "linux",
        prefix: None,
        working_dir: launcher.parent().map(Path::to_path_buf),
        exe: Some(launcher.to_path_buf()),
        appid: None,
        confidence: Confidence::Medium,
        reasons: vec!["it has a native Linux launcher script or binary".to_string()],
        in_lutris: false,
        eligible: true,
        blocking_reason: None,
        filesystem_warning: None,
        alternatives: Vec::new(),
    })
}

// ------------------------------------------------------------ prefix detection

/// If the executable already lives inside a Wine prefix, reuse that prefix.
/// Importing a game must never create a new empty prefix over an existing one.
fn find_prefix(exe: &Path) -> Option<PathBuf> {
    exe.ancestors()
        .find(|dir| dir.join("drive_c").is_dir() && dir.join("system.reg").is_file())
        .map(Path::to_path_buf)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalizes_for_comparison() {
        assert_eq!(normalize("Half-Life 2"), "halflife2");
        assert_eq!(normalize("The_Witcher_3"), "thewitcher3");
    }

    #[test]
    fn rejects_noise_directories() {
        assert!(is_noise_dir(Path::new("/games/x/_CommonRedist")));
        assert!(is_noise_dir(Path::new("/games/x/DirectX")));
        assert!(is_noise_dir(Path::new("/games/x/.hidden")));
        assert!(!is_noise_dir(Path::new("/games/x/Binaries")));
    }
}
