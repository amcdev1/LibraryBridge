//! Running the `librarybridge` binary and reading what it says.
//!
//! The window deliberately holds no repair or discovery logic of its own. It
//! shells out to the tested command line tool and renders the result, so the
//! two can never disagree about what is about to happen.

use std::io::{BufRead, BufReader};
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::mpsc::Sender;

use serde_json::Value;

/// Where the command line tool is. It ships beside the GUI, so look there
/// first and fall back to whatever is on PATH.
pub fn binary() -> PathBuf {
    if let Ok(own) = std::env::current_exe() {
        if let Some(dir) = own.parent() {
            let sibling = dir.join("librarybridge");
            if sibling.is_file() {
                return sibling;
            }
        }
    }
    PathBuf::from("librarybridge")
}

#[derive(Debug, Clone, Default)]
pub struct Library {
    pub id: String,
    pub path: String,
    pub name: String,
    pub steam: String,
    pub filesystem: String,
    pub state: String,
    pub target: String,
    pub connected: bool,
    pub backups: Vec<String>,
    /// Whether a repair applies, decided by the tool rather than guessed here
    /// from the shape of the state.
    pub eligible: bool,
    pub blocking_reason: String,
    /// Data already at the destination while this library is not repaired,
    /// which is a choice for the user, not something to resolve silently.
    pub destination_occupied: String,
}

impl Library {
    /// The plain-language version of the state code, matching the words the
    /// command line tool prints.
    pub fn headline(&self) -> &'static str {
        match self.state.as_str() {
            "repair_available" => "Repair available",
            "repaired" => "Repaired",
            "no_compatdata" => "No Proton data yet",
            "disconnected" => "Drive not connected",
            "interrupted" => "Interrupted, needs finishing",
            "dangling_link" => "Link is broken",
            "linked_elsewhere" => "Already linked elsewhere",
            _ => "Needs attention",
        }
    }

}

#[derive(Debug, Clone, Default)]
pub struct Candidate {
    pub id: String,
    pub name: String,
    pub runner: String,
    pub source: String,
    pub exe: String,
    pub appid: String,
    pub prefix: String,
    pub working_dir: String,
    pub confidence: String,
    pub in_lutris: bool,
    /// Whether the tool would actually import this. Detection and eligibility
    /// are different questions and the window has to show both.
    pub eligible: bool,
    pub blocking_reason: String,
    pub filesystem_warning: String,
    pub alternatives: Vec<String>,
    pub reasons: Vec<String>,
}

fn text(value: &Value, key: &str) -> String {
    value
        .get(key)
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string()
}

/// Run the tool and capture its output. Errors carry the tool's own stderr,
/// which is already written for a person to read.
pub fn run(arguments: &[String]) -> Result<String, String> {
    let output = Command::new(binary())
        .args(arguments)
        .output()
        .map_err(|e| format!("could not run {}: {e}", binary().display()))?;
    let stdout = String::from_utf8_lossy(&output.stdout).to_string();
    if output.status.success() {
        Ok(stdout)
    } else {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
        if stderr.is_empty() {
            Err(stdout)
        } else {
            Err(stderr)
        }
    }
}

/// A scan result, warnings included. Dropping them meant a library with
/// unreadable metadata produced a note on the command line and silence here.
#[derive(Debug, Clone, Default)]
pub struct Scan {
    pub libraries: Vec<Library>,
    pub warnings: Vec<String>,
    /// False when the scan could not read everything. An empty list and an
    /// unreadable one must not look the same.
    pub complete: bool,
}

/// The output schema this window knows how to read.
const SCHEMA: u64 = 1;

pub fn libraries() -> Result<Scan, String> {
    let text_out = run(&["scan".into(), "--json".into()])?;
    let parsed: Value = serde_json::from_str(&text_out).map_err(|e| e.to_string())?;

    // Two binaries that ship together can still be mismatched by a partial
    // install. Say so plainly rather than failing somewhere further in.
    match parsed.get("schema").and_then(Value::as_u64) {
        Some(SCHEMA) => {}
        other => {
            return Err(format!(
                "this window reads version {SCHEMA} of the command line tool's output, and \
                 {} reports version {}. They are from different builds; install them together.",
                binary().display(),
                other.map(|v| v.to_string()).unwrap_or_else(|| "an unknown".to_string())
            ))
        }
    }

    let rows = parsed
        .get("libraries")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let warnings = parsed
        .get("warnings")
        .and_then(Value::as_array)
        .map(|list| list.iter().filter_map(Value::as_str).map(str::to_string).collect())
        .unwrap_or_default();

    let libraries = rows
        .iter()
        .map(|row| Library {
            id: text(row, "id"),
            path: text(row, "path"),
            name: text(row, "name"),
            steam: text(row, "steam"),
            filesystem: text(row, "filesystem"),
            state: text(row, "state"),
            target: text(row, "target"),
            connected: row
                .get("connected")
                .and_then(Value::as_bool)
                .unwrap_or(false),
            eligible: row.get("eligible").and_then(Value::as_bool).unwrap_or(false),
            blocking_reason: text(row, "blocking_reason"),
            destination_occupied: text(row, "destination_occupied"),
            backups: row
                .get("backups")
                .and_then(Value::as_array)
                .map(|list| {
                    list.iter()
                        .filter_map(Value::as_str)
                        .map(str::to_string)
                        .collect()
                })
                .unwrap_or_default(),
        })
        .collect();
    let complete = parsed
        .get("complete")
        .and_then(Value::as_bool)
        .unwrap_or(true);
    Ok(Scan {
        libraries,
        warnings,
        complete,
    })
}

pub fn candidates(roots: &[String], include_known: bool) -> Result<Vec<Candidate>, String> {
    let mut arguments = vec![
        "lutris".to_string(),
        "scan".to_string(),
        "--json".to_string(),
    ];
    if include_known {
        arguments.push("--all".to_string());
    }
    for root in roots {
        arguments.push("--root".to_string());
        arguments.push(root.clone());
    }
    let text_out = run(&arguments)?;
    let parsed: Value = serde_json::from_str(&text_out).map_err(|e| e.to_string())?;
    let rows = parsed
        .get("candidates")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    Ok(rows
        .iter()
        .map(|row| Candidate {
            id: text(row, "id"),
            name: text(row, "name"),
            runner: text(row, "runner"),
            source: text(row, "source"),
            exe: text(row, "exe"),
            appid: text(row, "appid"),
            prefix: text(row, "prefix"),
            working_dir: text(row, "working_dir"),
            confidence: text(row, "confidence"),
            in_lutris: row
                .get("in_lutris")
                .and_then(Value::as_bool)
                .unwrap_or(false),
            eligible: row.get("eligible").and_then(Value::as_bool).unwrap_or(true),
            blocking_reason: text(row, "blocking_reason"),
            filesystem_warning: text(row, "filesystem_warning"),
            alternatives: row
                .get("alternatives")
                .and_then(Value::as_array)
                .map(|list| {
                    list.iter()
                        .filter_map(Value::as_str)
                        .map(str::to_string)
                        .collect()
                })
                .unwrap_or_default(),
            reasons: row
                .get("reasons")
                .and_then(Value::as_array)
                .map(|list| {
                    list.iter()
                        .filter_map(Value::as_str)
                        .map(str::to_string)
                        .collect()
                })
                .unwrap_or_default(),
        })
        .collect())
}

/// Whether Lutris is installed, and the one-line summary to show if it is.
pub fn lutris_status() -> Result<String, String> {
    run(&["lutris".into(), "detect".into()])
}

// ---------------------------------------------------------------- background

/// A phase of a running operation, as the tool reports it.
///
/// Which phase the work is in decides whether stopping is safe, so it arrives
/// as data rather than being inferred from printed text.
#[derive(Debug, Clone, PartialEq)]
pub enum Phase {
    Preparing,
    Copying { total_bytes: u64 },
    Progress { files: u64, bytes: u64 },
    Verifying,
    Committing,
    Applied,
}

impl Phase {
    /// Everything before the first change to the game drive can be stopped
    /// freely: the original has not been touched.
    pub fn can_stop(&self) -> bool {
        matches!(
            self,
            Phase::Preparing | Phase::Copying { .. } | Phase::Progress { .. } | Phase::Verifying
        )
    }

    pub fn label(&self) -> &'static str {
        match self {
            Phase::Preparing => "Checking the drive",
            Phase::Copying { .. } | Phase::Progress { .. } => "Copying",
            Phase::Verifying => "Checking every file",
            Phase::Committing => "Switching over",
            Phase::Applied => "Finished",
        }
    }
}

fn parse_event(line: &str) -> Option<Phase> {
    if !line.starts_with("{\"event\"") {
        return None;
    }
    let value: Value = serde_json::from_str(line).ok()?;
    let count = |key: &str| value.get(key).and_then(Value::as_u64).unwrap_or(0);
    Some(match value.get("event")?.as_str()? {
        "preparing" => Phase::Preparing,
        "copying" => Phase::Copying { total_bytes: count("total_bytes") },
        "progress" => Phase::Progress { files: count("files"), bytes: count("bytes") },
        "verifying" => Phase::Verifying,
        "committing" => Phase::Committing,
        "applied" => Phase::Applied,
        _ => return None,
    })
}

/// A running child, held so the window can stop it.
pub type Running = std::sync::Arc<std::sync::Mutex<Option<std::process::Child>>>;

/// Messages from a worker thread back to the window.
pub enum Update {
    Libraries(Result<Scan, String>),
    Candidates(Result<Vec<Candidate>, String>),
    Plan(Result<Plan, String>),
    Storage(Result<Vec<Stored>, String>),
    Evidence(Result<Vec<EvidenceRow>, String>),
    Lutris(Result<String, String>),
    /// One line of output from a running command.
    Line(String),
    /// A phase change or progress report from a running command.
    Event(Phase),
    /// A command finished. The bool says whether it succeeded.
    Done(bool),
}

pub fn spawn<F>(sender: Sender<Update>, work: F)
where
    F: FnOnce(Sender<Update>) + Send + 'static,
{
    std::thread::spawn(move || work(sender));
}

/// Run a command, sending each line of output as it appears so the window can
/// show progress rather than freezing until the copy finishes.
pub fn stream(sender: &Sender<Update>, arguments: &[String], running: &Running) {
    let mut child = match Command::new(binary())
        .args(arguments)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
    {
        Ok(child) => child,
        Err(error) => {
            let _ = sender.send(Update::Line(format!(
                "could not run {}: {error}",
                binary().display()
            )));
            let _ = sender.send(Update::Done(false));
            return;
        }
    };

    // Both pipes are drained at the same time. Reading one to the end first
    // deadlocks as soon as the other fills its buffer: the child blocks on a
    // write nobody is reading, and this side waits for an end that will never
    // come. The tool writes progress to stderr while copying, so that is not
    // hypothetical.
    let errors = child.stderr.take().map(|stderr| {
        let sender = sender.clone();
        std::thread::spawn(move || {
            for line in BufReader::new(stderr).lines().map_while(Result::ok) {
                let _ = sender.send(Update::Line(line));
            }
        })
    });

    let stdout = child.stdout.take();
    // Hand the child over so the window can stop it. Taking the pipes first
    // means the reading loops never need the lock.
    if let Ok(mut slot) = running.lock() {
        *slot = Some(child);
    }

    if let Some(stdout) = stdout {
        for line in BufReader::new(stdout).lines().map_while(Result::ok) {
            match parse_event(&line) {
                Some(phase) => {
                    let _ = sender.send(Update::Event(phase));
                }
                None => {
                    let _ = sender.send(Update::Line(line));
                }
            }
        }
    }
    if let Some(errors) = errors {
        let _ = errors.join();
    }

    let ok = running
        .lock()
        .ok()
        .and_then(|mut slot| slot.take())
        .and_then(|mut child| child.wait().ok())
        .map(|status| status.success())
        .unwrap_or(false);
    let _ = sender.send(Update::Done(ok));
}

/// The reviewed plan, as the tool reports it. The window renders these fields
/// rather than reading them back out of prose.
#[derive(Debug, Clone, Default)]
pub struct Plan {
    pub fingerprint: String,
    pub source: String,
    pub destination: String,
    pub backup: String,
    pub copy_bytes: u64,
    pub reserve_bytes: u64,
    pub available_bytes: Option<u64>,
    pub remaining_bytes: Option<u64>,
    pub installed_games: u64,
    pub prefix_folders: u64,
    pub unknown_folders: Vec<String>,
    pub consequences: Vec<String>,
}

fn strings(value: &Value, key: &str) -> Vec<String> {
    value
        .get(key)
        .and_then(Value::as_array)
        .map(|list| list.iter().filter_map(Value::as_str).map(str::to_string).collect())
        .unwrap_or_default()
}

pub fn plan(library_id: &str) -> Result<Plan, String> {
    let out = run(&[
        "fix".into(),
        library_id.into(),
        "--dry-run".into(),
        "--json".into(),
    ])?;
    let parsed: Value = serde_json::from_str(&out).map_err(|e| e.to_string())?;
    let count = |key: &str| parsed.get(key).and_then(Value::as_u64);
    Ok(Plan {
        fingerprint: text(&parsed, "fingerprint"),
        source: text(&parsed, "source"),
        destination: text(&parsed, "destination"),
        backup: text(&parsed, "backup"),
        copy_bytes: count("copy_bytes").unwrap_or(0),
        reserve_bytes: count("reserve_bytes").unwrap_or(0),
        available_bytes: count("available_bytes"),
        remaining_bytes: count("expected_remaining_bytes"),
        installed_games: count("installed_games").unwrap_or(0),
        prefix_folders: count("prefix_folders").unwrap_or(0),
        unknown_folders: strings(&parsed, "unknown_folders"),
        consequences: strings(&parsed, "consequences"),
    })
}

/// What is kept on disk for one library.
#[derive(Debug, Clone, Default)]
pub struct Stored {
    pub id: String,
    pub name: String,
    pub live: Option<(String, u64)>,
    pub backups: Vec<(String, u64)>,
    pub leftover: Option<(String, u64)>,
}

pub fn storage() -> Result<Vec<Stored>, String> {
    let out = run(&["storage".into(), "--json".into()])?;
    let parsed: Value = serde_json::from_str(&out).map_err(|e| e.to_string())?;
    let entry = |value: Option<&Value>| {
        value.and_then(|item| {
            Some((
                item.get("path")?.as_str()?.to_string(),
                item.get("bytes")?.as_u64()?,
            ))
        })
    };
    Ok(parsed
        .get("libraries")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default()
        .iter()
        .map(|row| Stored {
            id: text(row, "id"),
            name: text(row, "name"),
            live: entry(row.get("live")),
            backups: row
                .get("backups")
                .and_then(Value::as_array)
                .map(|list| list.iter().filter_map(|item| entry(Some(item))).collect())
                .unwrap_or_default(),
            leftover: entry(row.get("leftover")),
        })
        .collect())
}

pub fn human_bytes(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KiB", "MiB", "GiB", "TiB"];
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit < UNITS.len() - 1 {
        value /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{bytes} B")
    } else {
        format!("{value:.1} {}", UNITS[unit])
    }
}

/// One thing that has or has not been established about a library.
#[derive(Debug, Clone, Default)]
pub struct EvidenceRow {
    pub field: String,
    pub result: String,
    pub by_tool: bool,
}

pub fn evidence(library_id: &str) -> Result<Vec<EvidenceRow>, String> {
    let out = run(&["evidence".into(), library_id.into(), "--json".into()])?;
    let parsed: Value = serde_json::from_str(&out).map_err(|e| e.to_string())?;
    Ok(parsed
        .get("evidence")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default()
        .iter()
        .map(|row| EvidenceRow {
            field: text(row, "field"),
            result: text(row, "result"),
            by_tool: row.get("by_tool").and_then(Value::as_bool).unwrap_or(false),
        })
        .collect())
}

pub fn record_evidence(library_id: &str, field: &str, answer: &str) -> Result<(), String> {
    run(&[
        "evidence".into(),
        library_id.into(),
        "--record".into(),
        format!("{field}={answer}"),
    ])
    .map(|_| ())
}

pub fn describe_evidence(field: &str) -> &'static str {
    match field {
        "files" => "Files copied and checked",
        "launch" => "The game starts",
        "save" => "A save loads, and a new one is kept",
        _ => "Steam Cloud still syncs",
    }
}
