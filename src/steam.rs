//! Finding Steam, its libraries, and where each library's Proton data should
//! live once it has been moved off the game drive.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use crate::sha256::{hex, Sha256};
use crate::vdf;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InstallKind {
    Native,
    Flatpak,
}

impl InstallKind {
    pub fn label(self) -> &'static str {
        match self {
            InstallKind::Native => "native",
            InstallKind::Flatpak => "flatpak",
        }
    }
}

#[derive(Debug, Clone)]
pub struct Install {
    pub kind: InstallKind,
    pub root: PathBuf,
}

#[derive(Debug, Clone)]
pub struct Library {
    /// Short stable handle derived from the library path, used on the command
    /// line. Nothing is stored on disk to keep this; it is recomputed.
    pub id: String,
    pub path: PathBuf,
    pub label: String,
    pub steamapps: PathBuf,
    pub compatdata: PathBuf,
    pub install_kind: InstallKind,
    pub install_root: PathBuf,
    pub connected: bool,
}

impl Library {
    pub fn display_name(&self) -> String {
        if self.label.is_empty() {
            self.path
                .file_name()
                .map(|n| n.to_string_lossy().to_string())
                .unwrap_or_else(|| self.path.to_string_lossy().to_string())
        } else {
            self.label.clone()
        }
    }

    /// Where this installation's relocated data lives.
    ///
    /// For Flatpak Steam this sits inside Steam's own data directory. That
    /// area is already visible to the sandbox, so the repair never has to ask
    /// for a new Flatpak permission.
    pub fn target_root(&self) -> PathBuf {
        match self.install_kind {
            InstallKind::Flatpak => flatpak_data_root(&self.install_root)
                .unwrap_or_else(native_data_root)
                .join("librarybridge"),
            InstallKind::Native => native_data_root().join("librarybridge"),
        }
    }

    /// The name a new repair would use. The readable half is the library's
    /// display name, which can change; only the id half is identity.
    pub fn target(&self) -> PathBuf {
        self.target_root()
            .join(format!("{}-{}", sanitize(&self.display_name()), self.id))
            .join("compatdata")
    }

    /// The destination actually in use, which is any directory carrying this
    /// library's id whatever its readable half says.
    ///
    /// Renaming a library in Steam changes its display name and therefore the
    /// name `target` would derive. Looking the directory up by id instead
    /// means a working repair is not orphaned by a rename.
    pub fn effective_target(&self) -> PathBuf {
        let suffix = format!("-{}", self.id);
        if let Ok(entries) = fs::read_dir(self.target_root()) {
            for entry in entries.flatten() {
                if entry.file_name().to_string_lossy().ends_with(&suffix) {
                    return entry.path().join("compatdata");
                }
            }
        }
        self.target()
    }

    /// Is this path a destination belonging to this library? Identity is the
    /// id, never the readable half of the directory name.
    pub fn owns(&self, path: &Path) -> bool {
        if path.file_name().map(|name| name != "compatdata").unwrap_or(true) {
            return false;
        }
        let Some(parent) = path.parent() else {
            return false;
        };
        if parent.parent() != Some(self.target_root().as_path()) {
            return false;
        }
        parent
            .file_name()
            .map(|name| name.to_string_lossy().ends_with(&format!("-{}", self.id)))
            .unwrap_or(false)
    }

    /// A fresh staging directory for one copy.
    ///
    /// The name is unique per operation, so a leftover from an interrupted
    /// run is never mistaken for this run's own working directory and never
    /// removed to make room. This behavior is intentionally conservative.
    pub fn new_staging(&self) -> PathBuf {
        let target = self.effective_target();
        let parent = target.parent().unwrap_or(Path::new("/")).to_path_buf();
        let stamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.subsec_nanos())
            .unwrap_or(0);
        parent.join(format!(
            "{STAGING_PREFIX}-{}-{stamp}",
            std::process::id()
        ))
    }
}

/// Directories under this name are copies that were interrupted before they
/// could be verified. They are never touched automatically, because the name
/// alone does not prove which run created them.
pub const STAGING_PREFIX: &str = "compatdata.incomplete";

fn home() -> Option<PathBuf> {
    std::env::var_os("HOME").map(PathBuf::from)
}

fn native_data_root() -> PathBuf {
    if let Some(xdg) = std::env::var_os("XDG_DATA_HOME") {
        let path = PathBuf::from(xdg);
        if path.is_absolute() {
            return path;
        }
    }
    home()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".local/share")
}

/// From `~/.var/app/com.valvesoftware.Steam/.local/share/Steam`, walk back up
/// to the application directory and use its `data` folder.
fn flatpak_data_root(install_root: &Path) -> Option<PathBuf> {
    let app_dir = install_root.ancestors().find(|p| {
        p.file_name()
            .map(|n| n == "com.valvesoftware.Steam")
            .unwrap_or(false)
    })?;
    Some(app_dir.join("data"))
}

fn sanitize(name: &str) -> String {
    let cleaned: String = name
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect();
    let trimmed = cleaned.trim_matches('_').to_string();
    let trimmed = if trimmed.is_empty() {
        "library".to_string()
    } else {
        trimmed
    };
    trimmed.chars().take(32).collect()
}

pub fn short_id(path: &Path) -> String {
    let mut hasher = Sha256::new();
    hasher.update(path.to_string_lossy().as_bytes());
    hex(&hasher.finish())[..8].to_string()
}

/// Every Steam installation we can find. `override_root` replaces the search
/// entirely, which is how the tests and `--steam-root` point at a fixture.
pub fn find_installs(override_root: Option<&Path>) -> Vec<Install> {
    let mut candidates: Vec<(InstallKind, PathBuf)> = Vec::new();

    if let Some(root) = override_root {
        let kind = if root.to_string_lossy().contains("com.valvesoftware.Steam") {
            InstallKind::Flatpak
        } else {
            InstallKind::Native
        };
        candidates.push((kind, root.to_path_buf()));
    } else {
        if let Some(xdg) = std::env::var_os("XDG_DATA_HOME") {
            candidates.push((InstallKind::Native, PathBuf::from(xdg).join("Steam")));
        }
        if let Some(home) = home() {
            for relative in [
                ".local/share/Steam",
                ".steam/steam",
                ".steam/root",
                ".steam/debian-installation",
                // Development hosts. Harmless on Linux, useful on macOS.
                "Library/Application Support/Steam",
            ] {
                candidates.push((InstallKind::Native, home.join(relative)));
            }
            for relative in [
                ".var/app/com.valvesoftware.Steam/.local/share/Steam",
                ".var/app/com.valvesoftware.Steam/data/Steam",
            ] {
                candidates.push((InstallKind::Flatpak, home.join(relative)));
            }
        }
    }

    let mut seen: Vec<PathBuf> = Vec::new();
    let mut installs = Vec::new();
    for (kind, path) in candidates {
        if !looks_like_steam(&path) {
            continue;
        }
        let resolved = path.canonicalize().unwrap_or_else(|_| path.clone());
        if seen.contains(&resolved) {
            continue;
        }
        seen.push(resolved.clone());
        installs.push(Install {
            kind,
            root: resolved,
        });
    }
    installs
}

fn looks_like_steam(path: &Path) -> bool {
    path.join("steamapps").is_dir()
        || path.join("SteamApps").is_dir()
        || path.join("config").is_dir()
}

/// Libraries belonging to an installation, including the installation's own.
/// Missing folders stay in the list, marked as not connected.
pub fn libraries(install: &Install) -> (Vec<Library>, Vec<String>) {
    let mut warnings = Vec::new();
    let mut paths: Vec<(PathBuf, String)> = vec![(install.root.clone(), String::new())];

    let manifest = [
        install.root.join("steamapps/libraryfolders.vdf"),
        install.root.join("config/libraryfolders.vdf"),
        install.root.join("SteamApps/libraryfolders.vdf"),
    ]
    .into_iter()
    .find(|p| p.is_file());

    if let Some(manifest) = manifest {
        match vdf::parse_file(&manifest) {
            Ok(root) => {
                if let Some(folders) = root.get("libraryfolders") {
                    for (key, value) in folders.entries() {
                        if !key.chars().all(|c| c.is_ascii_digit()) {
                            continue; // TimeNextStatsReport and friends
                        }
                        match value {
                            // Legacy layout: the value is the path itself.
                            vdf::Value::Str(path) => {
                                paths.push((PathBuf::from(path), String::new()))
                            }
                            vdf::Value::Obj(_) => {
                                if let Some(path) = value.get("path").and_then(|v| v.as_str()) {
                                    let label = value
                                        .get("label")
                                        .and_then(|v| v.as_str())
                                        .unwrap_or("")
                                        .to_string();
                                    paths.push((PathBuf::from(path), label));
                                }
                            }
                        }
                    }
                }
            }
            Err(message) => warnings.push(message),
        }
    } else {
        warnings.push(format!(
            "{}: no libraryfolders.vdf found, only the main install is listed",
            install.root.display()
        ));
    }

    let mut seen: Vec<PathBuf> = Vec::new();
    let mut libraries = Vec::new();
    for (path, label) in paths {
        let connected = path.is_dir();
        let resolved = if connected {
            path.canonicalize().unwrap_or_else(|_| path.clone())
        } else {
            path.clone()
        };
        if seen.contains(&resolved) {
            continue;
        }
        seen.push(resolved.clone());

        let steamapps =
            if resolved.join("SteamApps").is_dir() && !resolved.join("steamapps").is_dir() {
                resolved.join("SteamApps")
            } else {
                resolved.join("steamapps")
            };
        libraries.push(Library {
            id: short_id(&resolved),
            label,
            compatdata: steamapps.join("compatdata"),
            steamapps,
            path: resolved,
            install_kind: install.kind,
            install_root: install.root.clone(),
            connected,
        });
    }
    (libraries, warnings)
}

/// Every library across every installation, keyed by id.
pub fn all_libraries(override_root: Option<&Path>) -> (Vec<Library>, Vec<String>) {
    let mut all: BTreeMap<String, Library> = BTreeMap::new();
    let mut warnings = Vec::new();
    for install in find_installs(override_root) {
        let (libraries, mut install_warnings) = libraries(&install);
        warnings.append(&mut install_warnings);
        for library in libraries {
            // A physical library shared between native and Flatpak Steam is
            // one repair scope. First installation seen owns the mapping.
            all.entry(library.id.clone()).or_insert(library);
        }
    }
    (all.into_values().collect(), warnings)
}

/// Match a library by id prefix or by path, so both work on the command line.
pub fn resolve<'a>(libraries: &'a [Library], reference: &str) -> Result<&'a Library, String> {
    let by_id: Vec<&Library> = libraries
        .iter()
        .filter(|l| l.id.starts_with(reference))
        .collect();
    if by_id.len() == 1 {
        return Ok(by_id[0]);
    }
    if by_id.len() > 1 {
        return Err(format!(
            "'{reference}' matches {} libraries. Use the full id.",
            by_id.len()
        ));
    }

    let wanted = PathBuf::from(reference);
    let wanted = wanted.canonicalize().unwrap_or(wanted);
    if let Some(found) = libraries.iter().find(|l| l.path == wanted) {
        return Ok(found);
    }
    Err(format!(
        "no library matches '{reference}'. Run `librarybridge scan` to list them."
    ))
}

/// Installed game names, read from the library's app manifests. Used only to
/// make output readable; a missing or unparsable manifest is not an error.
pub fn app_names(library: &Library) -> BTreeMap<String, String> {
    let mut names = BTreeMap::new();
    let Ok(entries) = fs::read_dir(&library.steamapps) else {
        return names;
    };
    for entry in entries.flatten() {
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if !name.starts_with("appmanifest_") || !name.ends_with(".acf") {
            continue;
        }
        let Ok(parsed) = vdf::parse_file(&entry.path()) else {
            continue;
        };
        let Some(state) = parsed.get("AppState") else {
            continue;
        };
        let app_id = state.get("appid").and_then(|v| v.as_str());
        let app_name = state.get("name").and_then(|v| v.as_str());
        if let (Some(app_id), Some(app_name)) = (app_id, app_name) {
            names.insert(app_id.to_string(), app_name.to_string());
        }
    }
    names
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sanitizes_library_names() {
        assert_eq!(sanitize("SteamLibrary"), "SteamLibrary");
        assert_eq!(sanitize("My Games!"), "My_Games");
        assert_eq!(sanitize("../../etc"), "etc");
        assert_eq!(sanitize(""), "library");
        assert!(!sanitize("日本語のライブラリ").contains('/'));
    }

    #[test]
    fn ids_are_stable_and_path_specific() {
        let a = short_id(Path::new("/mnt/games/SteamLibrary"));
        let b = short_id(Path::new("/mnt/games/SteamLibrary"));
        let c = short_id(Path::new("/mnt/other/SteamLibrary"));
        assert_eq!(a, b);
        assert_ne!(a, c);
        assert_eq!(a.len(), 8);
    }

    #[test]
    fn finds_the_flatpak_data_directory() {
        let root = PathBuf::from("/home/a/.var/app/com.valvesoftware.Steam/.local/share/Steam");
        assert_eq!(
            flatpak_data_root(&root),
            Some(PathBuf::from(
                "/home/a/.var/app/com.valvesoftware.Steam/data"
            ))
        );
    }
}
