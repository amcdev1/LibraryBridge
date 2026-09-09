//! What has actually been established about a repaired library.
//!
//! A repair proves one thing: the files were copied and checked. It proves
//! nothing about whether a game starts, whether a save loads, or whether Steam
//! Cloud is still working. Those are separate questions with separate answers,
//! and the answers come from a person playing a game, not from this tool.
//!
//! So each is recorded separately, with who established it and when, and
//! anything not established stays "not checked" rather than being implied by a
//! green tick somewhere else.

use std::fs;
use std::path::PathBuf;

use crate::json::{self, Json};
use crate::state::app_data_dir;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Result_ {
    NotChecked,
    Worked,
    Failed,
    NotApplicable,
}

impl Result_ {
    pub fn label(self) -> &'static str {
        match self {
            Result_::NotChecked => "not checked",
            Result_::Worked => "worked",
            Result_::Failed => "failed",
            Result_::NotApplicable => "does not apply",
        }
    }

    fn code(self) -> &'static str {
        match self {
            Result_::NotChecked => "not_checked",
            Result_::Worked => "worked",
            Result_::Failed => "failed",
            Result_::NotApplicable => "not_applicable",
        }
    }

    fn parse(text: &str) -> Option<Result_> {
        match text {
            "not_checked" => Some(Result_::NotChecked),
            "worked" | "yes" => Some(Result_::Worked),
            "failed" | "no" => Some(Result_::Failed),
            "not_applicable" | "na" => Some(Result_::NotApplicable),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Row {
    pub result: Result_,
    pub when: u64,
    /// True when this tool established it, false when a person reported it.
    pub by_tool: bool,
}

impl Default for Row {
    fn default() -> Self {
        Row {
            result: Result_::NotChecked,
            when: 0,
            by_tool: false,
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct Evidence {
    pub files: Row,
    pub launch: Row,
    pub save: Row,
    pub cloud: Row,
}

pub const FIELDS: [&str; 4] = ["files", "launch", "save", "cloud"];

pub fn describe(field: &str) -> &'static str {
    match field {
        "files" => "Files copied and checked",
        "launch" => "Game starts",
        "save" => "An existing save loads, and a new one is kept",
        _ => "Steam Cloud still syncs",
    }
}

fn path_for(library_id: &str) -> PathBuf {
    app_data_dir().join("evidence").join(format!("{library_id}.json"))
}

fn now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

impl Evidence {
    pub fn load(library_id: &str) -> Evidence {
        let Ok(text) = fs::read_to_string(path_for(library_id)) else {
            return Evidence::default();
        };
        let Ok(parsed) = json::parse(&text) else {
            return Evidence::default();
        };
        let row = |name: &str| -> Row {
            let Some(value) = parsed.get(name) else {
                return Row::default();
            };
            Row {
                result: value
                    .string("result")
                    .and_then(|r| Result_::parse(&r))
                    .unwrap_or(Result_::NotChecked),
                when: match value.get("when") {
                    Some(Json::Number(n)) => *n as u64,
                    _ => 0,
                },
                by_tool: value.get("by_tool").and_then(Json::as_bool).unwrap_or(false),
            }
        };
        Evidence {
            files: row("files"),
            launch: row("launch"),
            save: row("save"),
            cloud: row("cloud"),
        }
    }

    pub fn get(&self, field: &str) -> Row {
        match field {
            "files" => self.files,
            "launch" => self.launch,
            "save" => self.save,
            _ => self.cloud,
        }
    }

    fn set(&mut self, field: &str, row: Row) {
        match field {
            "files" => self.files = row,
            "launch" => self.launch = row,
            "save" => self.save = row,
            _ => self.cloud = row,
        }
    }

    /// Record an answer. `by_tool` separates what was measured from what a
    /// person reported, because they are not the same kind of claim.
    pub fn record(
        library_id: &str,
        field: &str,
        result: Result_,
        by_tool: bool,
    ) -> Result<(), String> {
        if !FIELDS.contains(&field) {
            return Err(format!(
                "'{field}' is not one of the things recorded here: {}",
                FIELDS.join(", ")
            ));
        }
        let mut evidence = Evidence::load(library_id);
        evidence.set(
            field,
            Row {
                result,
                when: now(),
                by_tool,
            },
        );

        let path = path_for(library_id);
        let directory = path.parent().unwrap().to_path_buf();
        fs::create_dir_all(&directory).map_err(|e| format!("{}: {e}", directory.display()))?;

        let block = |name: &str, row: Row| {
            format!(
                "  \"{name}\": {{\"result\": \"{}\", \"when\": {}, \"by_tool\": {}}}",
                row.result.code(),
                row.when,
                row.by_tool
            )
        };
        let document = format!(
            "{{\n  \"schema\": 1,\n{},\n{},\n{},\n{}\n}}\n",
            block("files", evidence.files),
            block("launch", evidence.launch),
            block("save", evidence.save),
            block("cloud", evidence.cloud),
        );
        fs::write(&path, document).map_err(|e| format!("{}: {e}", path.display()))
    }

    /// Anything that has to be established again after the data moved.
    pub fn invalidate(library_id: &str) {
        for field in ["launch", "save", "cloud"] {
            let _ = Evidence::record(library_id, field, Result_::NotChecked, false);
        }
    }
}

pub fn parse_result(text: &str) -> Option<Result_> {
    Result_::parse(text)
}
