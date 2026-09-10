//! Talking to Lutris: finding it, reading what it already knows about, and
//! generating install definitions for games it is missing.
//!
//! Nothing here writes to Lutris's database. Entries are created by handing
//! Lutris a YAML definition through its own install command, which is the
//! supported path and leaves Lutris in charge of its own files.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use crate::json;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Packaging {
    Native,
    Flatpak,
}

impl Packaging {
    pub fn label(self) -> &'static str {
        match self {
            Packaging::Native => "native",
            Packaging::Flatpak => "flatpak",
        }
    }
}

#[derive(Debug, Clone)]
pub struct Installation {
    pub packaging: Packaging,
    pub config_dir: PathBuf,
    /// The command that launches Lutris, already split into arguments.
    pub command: Vec<String>,
}

impl Installation {
    pub fn games_dir(&self) -> PathBuf {
        self.config_dir.join("games")
    }

    fn invoke(&self, extra: &[&str]) -> Option<std::process::Output> {
        let (program, leading) = self.command.split_first()?;
        Command::new(program)
            .args(leading)
            .args(extra)
            .output()
            .ok()
    }
}

fn home() -> PathBuf {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."))
}

fn config_home() -> PathBuf {
    match std::env::var_os("XDG_CONFIG_HOME") {
        Some(dir) if Path::new(&dir).is_absolute() => PathBuf::from(dir),
        _ => home().join(".config"),
    }
}

fn data_home() -> PathBuf {
    match std::env::var_os("XDG_DATA_HOME") {
        Some(dir) if Path::new(&dir).is_absolute() => PathBuf::from(dir),
        _ => home().join(".local/share"),
    }
}

/// Which directory holds a Lutris installation's config. Lutris 0.5+ (the
/// GTK4 rewrite) moved everything into the data home: `~/.local/share/lutris`
/// holds `games/`, `pga.db`, runners, and the config. Earlier releases used
/// the config home. Prefer whichever actually exists.
pub fn lutris_base() -> Option<PathBuf> {
    let modern = data_home().join("lutris");
    if modern.join("games").is_dir() || modern.join("pga.db").is_file() {
        return Some(modern);
    }
    let legacy = config_home().join("lutris");
    if legacy.is_dir() {
        return Some(legacy);
    }
    None
}

/// Look for a program on PATH without running anything.
fn on_path(program: &str) -> bool {
    let Some(path) = std::env::var_os("PATH") else {
        return false;
    };
    std::env::split_paths(&path).any(|dir| {
        let candidate = dir.join(program);
        candidate.is_file()
    })
}

fn command_line_is_lutris(command_line: &str) -> bool {
    let command_line = command_line.to_ascii_lowercase();
    command_line.split_whitespace().any(|part| {
        part == "lutris"
            || part.ends_with("/lutris")
            || part == "net.lutris.lutris"
            || part.ends_with("/net.lutris.lutris")
    })
}

/// Whether a command line belongs to LibraryBridge itself rather than to
/// Lutris. Its own processes run as `librarybridge lutris import` and
/// `librarybridge lutris scan`, and the `lutris` there is an argument, not the
/// application the running check is looking for. Without this filter every
/// import would see its own `lutris` argument, conclude Lutris was already
/// open, and never show the opening step.
fn command_line_is_self(command_line: &str) -> bool {
    let command_line = command_line.to_ascii_lowercase();
    command_line
        .split_whitespace()
        .next()
        .map(|first| first == "librarybridge" || first.ends_with("/librarybridge"))
        .unwrap_or(false)
}

/// Whether a Lutris process is already running.
///
/// The import command still sends Lutris an install request either way, but
/// knowing this lets the window skip the opening step when Lutris is already
/// available. On Linux `/proc` gives us the real command lines; `ps` keeps the
/// check useful on development hosts without `/proc`.
pub fn is_running(_installation: &Installation) -> bool {
    if Path::new("/proc").is_dir() {
        // The caller is itself `librarybridge lutris import`, so its own
        // command line already matches `lutris` and must not count.
        let own_pid = std::process::id().to_string();
        let Ok(entries) = fs::read_dir("/proc") else {
            return false;
        };
        return entries.flatten().any(|entry| {
            if entry.file_name().to_string_lossy() == own_pid {
                return false;
            }
            let command = entry.path().join("cmdline");
            let command_line = fs::read(command).unwrap_or_default();
            let command_line = String::from_utf8_lossy(&command_line).replace('\0', " ");
            // A background `librarybridge lutris scan` also looks like this,
            // and it is not the Lutris application either.
            if command_line_is_self(&command_line) {
                return false;
            }
            if command_line_is_lutris(&command_line) {
                return true;
            }
            fs::read_to_string(entry.path().join("comm"))
                .map(|name| command_line_is_lutris(name.trim()))
                .unwrap_or(false)
        });
    }

    Command::new("ps")
        .args(["-A", "-o", "args="])
        .output()
        .ok()
        .filter(|output| output.status.success())
        .map(|output| {
            String::from_utf8_lossy(&output.stdout)
                .lines()
                .any(|line| {
                    let line = line.trim();
                    !command_line_is_self(line) && command_line_is_lutris(line)
                })
        })
        .unwrap_or(false)
}

/// Every Lutris installation we can see. Both packagings can be present.
pub fn find_installations() -> Vec<Installation> {
    let mut found = Vec::new();

    let native_base = lutris_base().unwrap_or_default();
    let lutris_visible = on_path("lutris") || native_base.is_dir();
    if lutris_visible {
        found.push(Installation {
            packaging: Packaging::Native,
            config_dir: native_base,
            command: vec!["lutris".to_string()],
        });
    }

    let flatpak_app = home().join(".var/app/net.lutris.Lutris");
    if flatpak_app.is_dir() {
        // Flatpak Lutris 0.5+ keeps its data under the app's data dir.
        let flatpak_data = flatpak_app.join("data/lutris");
        let flatpak_config = flatpak_app.join("config/lutris");
        let base = if flatpak_data.join("games").is_dir() {
            flatpak_data
        } else {
            flatpak_config
        };
        found.push(Installation {
            packaging: Packaging::Flatpak,
            config_dir: base,
            command: vec![
                "flatpak".to_string(),
                "run".to_string(),
                "net.lutris.Lutris".to_string(),
            ],
        });
    }
    found
}

#[derive(Debug, Clone)]
pub struct Entry {
    pub slug: String,
    pub name: String,
    pub runner: String,
    pub exe: Option<PathBuf>,
    pub appid: Option<String>,
    pub prefix: Option<PathBuf>,
    pub config: PathBuf,
}

/// What Lutris already has.
///
/// Read from the per-game YAML files in its config directory rather than from
/// `pga.db`, so there is no SQLite dependency and no chance of disturbing the
/// database. Where the `lutris` command is available its game list is merged
/// in, because it carries the display names the YAML files do not.
pub fn existing_entries(installation: &Installation) -> Vec<Entry> {
    let mut entries = Vec::new();
    let Ok(files) = fs::read_dir(installation.games_dir()) else {
        return entries;
    };

    for file in files.flatten() {
        let path = file.path();
        if path.extension().map(|e| e != "yml").unwrap_or(true) {
            continue;
        }
        let Ok(text) = fs::read_to_string(&path) else {
            continue;
        };
        let fields = scalar_fields(&text);
        let stem = path
            .file_stem()
            .map(|s| s.to_string_lossy().to_string())
            .unwrap_or_default();
        // The slug that identifies the game is the `game_slug` field Lutris
        // writes (and what the tool's own definitions emit), not the filename,
        // which carries a "-librarybridge-<timestamp>" suffix. A filename-only
        // read would never match a candidate's slug.
        let slug = fields
            .get("game_slug")
            .cloned()
            .unwrap_or_else(|| strip_trailing_id(&stem));
        entries.push(Entry {
            name: fields
                .get("name")
                .cloned()
                .unwrap_or_else(|| titleize(&slug)),
            runner: fields
                .get("runner")
                .cloned()
                .unwrap_or_else(|| "unknown".into()),
            exe: fields.get("exe").map(PathBuf::from),
            appid: fields.get("appid").cloned(),
            prefix: fields.get("prefix").map(PathBuf::from),
            config: path,
            slug,
        });
    }

    if let Some(named) = names_from_cli(installation) {
        for entry in &mut entries {
            if let Some(name) = named.get(&entry.slug) {
                entry.name = name.clone();
            }
        }
    }
    entries.sort_by(|a, b| a.slug.cmp(&b.slug));
    entries
}

fn names_from_cli(installation: &Installation) -> Option<BTreeMap<String, String>> {
    let output = installation.invoke(&["--list-games", "--json"])?;
    if !output.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&output.stdout);
    let start = text.find(['[', '{'])?;
    let parsed = json::parse(text[start..].trim()).ok()?;
    let mut names = BTreeMap::new();
    for game in parsed.as_array() {
        if let (Some(slug), Some(name)) = (game.string("slug"), game.string("name")) {
            names.insert(slug, name);
        }
    }
    Some(names)
}

/// Lutris names its config files `<slug>-<id>.yml`. Drop the trailing id.
fn strip_trailing_id(stem: &str) -> String {
    match stem.rsplit_once('-') {
        Some((head, tail)) if !tail.is_empty() && tail.chars().all(|c| c.is_ascii_digit()) => {
            head.to_string()
        }
        _ => stem.to_string(),
    }
}

/// Collect every `key: value` pair in a YAML file, ignoring structure.
///
/// Lutris config files are shallow maps of scalars, and the only fields that
/// matter here are exe, appid, prefix and runner. Reading them flatly avoids
/// carrying a YAML parser for a job that does not need one.
fn scalar_fields(text: &str) -> BTreeMap<String, String> {
    let mut fields = BTreeMap::new();
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') || line.starts_with('-') {
            continue;
        }
        let Some((key, value)) = line.split_once(':') else {
            continue;
        };
        let value = value.trim();
        if value.is_empty() {
            continue;
        }
        let value = value.trim_matches(|c| c == '\'' || c == '"').to_string();
        fields.entry(key.trim().to_string()).or_insert(value);
    }
    fields
}

pub fn slugify(name: &str) -> String {
    let mut slug = String::new();
    let mut last_dash = true;
    for c in name.chars() {
        if c.is_ascii_alphanumeric() {
            slug.push(c.to_ascii_lowercase());
            last_dash = false;
        } else if !last_dash {
            slug.push('-');
            last_dash = true;
        }
    }
    let slug = slug.trim_matches('-').to_string();
    if slug.is_empty() {
        "game".to_string()
    } else {
        slug.chars().take(60).collect()
    }
}

fn titleize(slug: &str) -> String {
    slug.split('-')
        .filter(|part| !part.is_empty())
        .map(|part| {
            let mut chars = part.chars();
            match chars.next() {
                Some(first) => first.to_uppercase().collect::<String>() + chars.as_str(),
                None => String::new(),
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// YAML single-quoted scalar. Everything we emit is a path or a name that
/// came off the filesystem, so it is quoted unconditionally.
fn quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "''"))
}

#[derive(Debug, Clone)]
pub struct Definition {
    pub name: String,
    pub slug: String,
    pub runner: String,
    pub exe: Option<PathBuf>,
    pub appid: Option<String>,
    pub prefix: Option<PathBuf>,
    pub working_dir: Option<PathBuf>,
}

impl Definition {
    /// A Lutris install script. The shape follows Lutris's installer
    /// documentation; it is validated against the installed Lutris version at
    /// import time rather than assumed to be accepted.
    pub fn to_yaml(&self) -> String {
        let mut out = String::new();
        out.push_str(&format!("name: {}\n", quote(&self.name)));
        out.push_str(&format!("game_slug: {}\n", quote(&self.slug)));
        out.push_str("version: librarybridge-import\n");
        out.push_str(&format!(
            "slug: {}\n",
            quote(&format!("{}-librarybridge", self.slug))
        ));
        out.push_str(&format!("runner: {}\n", quote(&self.runner)));
        out.push_str("script:\n  game:\n");
        if let Some(appid) = &self.appid {
            out.push_str(&format!("    appid: {}\n", quote(appid)));
        }
        if let Some(exe) = &self.exe {
            out.push_str(&format!("    exe: {}\n", quote(&exe.to_string_lossy())));
        }
        if let Some(prefix) = &self.prefix {
            out.push_str(&format!(
                "    prefix: {}\n",
                quote(&prefix.to_string_lossy())
            ));
        }
        if let Some(dir) = &self.working_dir {
            out.push_str(&format!(
                "    working_dir: {}\n",
                quote(&dir.to_string_lossy())
            ));
        }
        out
    }
}

/// Hand one definition to Lutris. Lutris shows its own installer dialog, so
/// this returns once the user has finished with it.
///
/// Lutris's exit code is not a reliable success signal: `--install` can exit
/// non-zero even when the entry landed (recent Lutris versions do), and the
/// user can cancel the dialog. So a non-zero exit is reported, but the caller
/// decides what it means by re-reading Lutris's config afterwards. The caller
/// that checks for the entry decides success, not this function.
pub fn install(installation: &Installation, yaml_path: &Path) -> Result<i32, String> {
    let (program, leading) = installation
        .command
        .split_first()
        .ok_or("no Lutris command available")?;
    let status = Command::new(program)
        .args(leading)
        .arg("--install")
        .arg(yaml_path)
        .status()
        .map_err(|e| format!("could not run Lutris: {e}"))?;
    if status.success() {
        Ok(0)
    } else {
        // Report the code but do not fail the import on it. The dialog could
        // have been cancelled, or Lutris could have added the game anyway.
        // The caller verifies empirically by re-reading Lutris's config.
        Ok(status.code().unwrap_or(-1))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_the_fields_that_matter_from_a_config() {
        let text = "game:\n  exe: /home/a/Games/x/Game.exe\n  prefix: /home/a/Games/x-prefix\n\
                    \nrunner: wine\nsystem:\n  env:\n    DXVK: '1'\n";
        let fields = scalar_fields(text);
        assert_eq!(fields.get("exe").unwrap(), "/home/a/Games/x/Game.exe");
        assert_eq!(fields.get("prefix").unwrap(), "/home/a/Games/x-prefix");
        assert_eq!(fields.get("runner").unwrap(), "wine");
    }

    #[test]
    fn strips_the_id_lutris_appends_to_config_names() {
        assert_eq!(strip_trailing_id("half-life-2-1699999999"), "half-life-2");
        assert_eq!(strip_trailing_id("portal"), "portal");
        assert_eq!(strip_trailing_id("game-2"), "game");
    }

    /// The running check must not mistake `librarybridge lutris import` (or a
    /// background `lutris scan`) for the Lutris application itself.
    #[test]
    fn our_own_lutris_arguments_are_never_a_running_lutris() {
        assert!(command_line_is_lutris("/usr/bin/lutris"));
        assert!(command_line_is_lutris("python3 /usr/bin/lutris"));
        assert!(command_line_is_lutris("flatpak run net.lutris.Lutris"));
        // A LibraryBridge invocation that merely names `lutris` as a
        // subcommand is separated at the running-process check via
        // `command_line_is_self`, not in this classifier.
        assert!(command_line_is_self("librarybridge lutris import --plan /tmp/plan.json"));
        assert!(command_line_is_self("/opt/librarybridge/bin/librarybridge lutris scan --root /games"));
        assert!(!command_line_is_self("/usr/bin/lutris"));
        assert!(!command_line_is_self("flatpak run net.lutris.Lutris"));
    }

    #[test]
    fn slugs_are_url_shaped() {
        assert_eq!(
            slugify("Half-Life 2: Episode One"),
            "half-life-2-episode-one"
        );
        assert_eq!(slugify("  S.T.A.L.K.E.R.  "), "s-t-a-l-k-e-r");
        assert_eq!(slugify("!!!"), "game");
    }

    #[test]
    fn yaml_quotes_awkward_paths() {
        let definition = Definition {
            name: "Tom's Game".into(),
            slug: "toms-game".into(),
            runner: "wine".into(),
            exe: Some(PathBuf::from("/mnt/My Games/Tom's Game/game.exe")),
            appid: None,
            prefix: Some(PathBuf::from("/home/a/prefix")),
            working_dir: None,
        };
        let yaml = definition.to_yaml();
        assert!(yaml.contains("name: 'Tom''s Game'"), "{yaml}");
        assert!(
            yaml.contains("exe: '/mnt/My Games/Tom''s Game/game.exe'"),
            "{yaml}"
        );
        assert!(!yaml.contains("appid"));
    }

    #[test]
    fn steam_definitions_carry_an_appid_and_no_executable() {
        let definition = Definition {
            name: "Half-Life 2".into(),
            slug: "half-life-2".into(),
            runner: "steam".into(),
            exe: None,
            appid: Some("220".into()),
            prefix: None,
            working_dir: None,
        };
        let yaml = definition.to_yaml();
        assert!(yaml.contains("appid: '220'"), "{yaml}");
        assert!(!yaml.contains("exe:"), "{yaml}");
    }
}
