//! A durable note of an operation in progress.
//!
//! Disk evidence stays authoritative: a record never permits an overwrite and
//! never proves on its own that a copy is good. What it does is let recovery
//! skip re-reading two whole trees when it can show the copy it is about to
//! adopt is the one this tool verified.
//!
//! Written before the first mutation, updated at each durable transition, and
//! removed when the operation completes. A record left behind is a report that
//! something stopped, not an instruction.

use std::fs;
use std::path::{Path, PathBuf};

use crate::json::{self, Json};
use crate::state::app_data_dir;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Stage {
    /// The copy exists and every file was checked against the source.
    Verified,
    /// The copy has been renamed onto its final destination.
    Published,
    /// The original has been renamed aside to the backup.
    BackedUp,
}

impl Stage {
    fn label(self) -> &'static str {
        match self {
            Stage::Verified => "verified",
            Stage::Published => "published",
            Stage::BackedUp => "backed_up",
        }
    }

    fn parse(text: &str) -> Option<Stage> {
        match text {
            "verified" => Some(Stage::Verified),
            "published" => Some(Stage::Published),
            "backed_up" => Some(Stage::BackedUp),
            _ => None,
        }
    }
}

#[derive(Debug, Clone)]
pub struct Record {
    pub library_id: String,
    pub source: PathBuf,
    pub destination: PathBuf,
    pub backup: PathBuf,
    /// Digest of the manifest that was verified, so the destination can be
    /// recognised later without reading the source again.
    pub digest: String,
    pub stage: Stage,
}

fn directory() -> PathBuf {
    app_data_dir().join("operations")
}

fn path_for(library_id: &str) -> PathBuf {
    directory().join(format!("{library_id}.json"))
}

fn quote(value: &str) -> String {
    let mut out = String::with_capacity(value.len() + 2);
    out.push('"');
    for c in value.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

impl Record {
    /// Write the record and flush it, along with the directory holding it, so
    /// it survives the power going out immediately afterwards.
    pub fn write(&self) -> Result<(), String> {
        let directory = directory();
        fs::create_dir_all(&directory).map_err(|e| format!("{}: {e}", directory.display()))?;
        let path = path_for(&self.library_id);

        let document = format!(
            "{{\n  \"schema\": 1,\n  \"library_id\": {},\n  \"source\": {},\n  \
             \"destination\": {},\n  \"backup\": {},\n  \"digest\": {},\n  \"stage\": {}\n}}\n",
            quote(&self.library_id),
            quote(&self.source.to_string_lossy()),
            quote(&self.destination.to_string_lossy()),
            quote(&self.backup.to_string_lossy()),
            quote(&self.digest),
            quote(self.stage.label())
        );
        fs::write(&path, document).map_err(|e| format!("{}: {e}", path.display()))?;
        sync(&path)?;
        sync(&directory)
    }

    pub fn advance(&mut self, stage: Stage) -> Result<(), String> {
        self.stage = stage;
        self.write()
    }

    pub fn load(library_id: &str) -> Option<Record> {
        let path = path_for(library_id);
        let text = fs::read_to_string(path).ok()?;
        let parsed = json::parse(&text).ok()?;
        if parsed.get("schema") != Some(&Json::Number(1.0)) {
            return None;
        }
        Some(Record {
            library_id: parsed.string("library_id")?,
            source: PathBuf::from(parsed.string("source")?),
            destination: PathBuf::from(parsed.string("destination")?),
            backup: PathBuf::from(parsed.string("backup")?),
            digest: parsed.string("digest")?,
            stage: Stage::parse(&parsed.string("stage")?)?,
        })
    }

    pub fn clear(library_id: &str) {
        let _ = fs::remove_file(path_for(library_id));
    }
}

/// Flush a file or directory. Failures are returned rather than ignored: an
/// unflushed record is exactly the one that will not be there after a crash.
pub fn sync(path: &Path) -> Result<(), String> {
    let handle = fs::File::open(path).map_err(|e| format!("{}: {e}", path.display()))?;
    handle
        .sync_all()
        .map_err(|e| format!("{}: could not flush to disk: {e}", path.display()))
}
