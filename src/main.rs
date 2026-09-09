//! LibraryBridge: move Proton compatdata off a game drive that cannot hold it,
//! onto a filesystem that can, without ever deleting the original.

mod commands;
mod discover;
mod evidence;
mod fsops;
mod json;
mod lock;
mod lutris;
mod lutris_cmd;
mod record;
mod safefs;
mod sha256;
mod state;
mod steam;
mod system;
mod vdf;

use std::path::PathBuf;
use std::process::ExitCode;

const VERSION: &str = env!("CARGO_PKG_VERSION");

const USAGE: &str = "\
librarybridge - move Proton compatdata onto a filesystem that can hold it

USAGE
    librarybridge [options] <command>

COMMANDS
    scan                 List Steam libraries and what each one needs
    fix <library>        Copy this library's compatdata to your Linux drive,
                         move the original aside, and link to the copy
    undo <library>       Copy the data back and remove the link
    storage              Show what is stored and how much space it uses
    evidence <library>   Show what has been established, and record an answer

    <library> is an id from `scan`, or a library path.

    lutris detect        Report whether Lutris is installed and what it knows
    lutris scan          Find installed games that Lutris does not have
    lutris plan          Write a reviewable import file for chosen games
    lutris import        Hand that file to Lutris, one dialog per game
    lutris forget        Drop LibraryBridge's record of an import

OPTIONS
    --steam-root PATH    Use this Steam installation instead of searching
    --root PATH          Folder to scan for games; repeat for more
    --candidate ID       Game to include in a plan; repeat for more
    --output PATH        Where `lutris plan` writes its file
    --plan PATH          Plan file for `lutris import`
    --entry SLUG         Import record for `lutris forget`
    --all                Include games Lutris already has
    --json               Machine-readable output
    -n, --dry-run        Show what would happen and stop
    -y, --yes            Do not ask for confirmation
    --force              Proceed past a filesystem that needs no repair
    --expect PLAN        Apply only if the plan is still the one reviewed
    --record FIELD=ANSWER  Record evidence, for example launch=yes
    --keep-destination   On a destination conflict, keep the data already at
                         the destination and set the game drive copy aside
    --replace-destination  The other way round. Both copies are always kept.
    -h, --help           This text
    -V, --version        Version

NOTES
    Nothing is ever deleted. `fix` renames your original compatdata to
    compatdata.backup beside itself and leaves it there.
    Close Steam before repairing.
";

fn main() -> ExitCode {
    // `args` panics on an argument that is not valid UTF-8, and a Linux path
    // is bytes, not text. Options that name a path keep their original bytes;
    // everything else is compared as text.
    let raw: Vec<std::ffi::OsString> = std::env::args_os().skip(1).collect();
    let arguments: Vec<String> = raw
        .iter()
        .map(|argument| argument.to_string_lossy().into_owned())
        .collect();

    let mut options = commands::Options {
        steam_root: None,
        json: false,
        dry_run: false,
        force: false,
        assume_yes: false,
        roots: Vec::new(),
        candidates: Vec::new(),
        output: None,
        plan: None,
        entry: None,
        all: false,
        expect: None,
        record: None,
        keep_destination: false,
        replace_destination: false,
    };
    let mut positional: Vec<String> = Vec::new();
    let mut index = 0;

    while index < arguments.len() {
        let argument = arguments[index].as_str();
        match argument {
            "-h" | "--help" => {
                print!("{USAGE}");
                return ExitCode::SUCCESS;
            }
            "-V" | "--version" => {
                println!("librarybridge {VERSION}");
                return ExitCode::SUCCESS;
            }
            "--json" => options.json = true,
            "-n" | "--dry-run" => options.dry_run = true,
            "-y" | "--yes" => options.assume_yes = true,
            "--force" => options.force = true,
            "--all" => options.all = true,
            "--keep-destination" => options.keep_destination = true,
            "--replace-destination" => options.replace_destination = true,
            "--steam-root" | "--root" | "--candidate" | "--output" | "--plan" | "--entry"
            | "--expect" | "--record" => {
                index += 1;
                let Some(value) = arguments.get(index) else {
                    return usage_error(&format!("{argument} needs a value"));
                };
                // Paths come from the untouched argument, not the lossy copy.
                let original = raw[index].clone();
                match argument {
                    "--steam-root" => options.steam_root = Some(PathBuf::from(original)),
                    "--root" => options.roots.push(PathBuf::from(original)),
                    "--candidate" => options.candidates.push(value.clone()),
                    "--output" => options.output = Some(PathBuf::from(original)),
                    "--plan" => options.plan = Some(PathBuf::from(original)),
                    "--expect" => options.expect = Some(value.clone()),
                    "--record" => options.record = Some(value.clone()),
                    _ => options.entry = Some(value.clone()),
                }
            }
            other if other.starts_with('-') => {
                return usage_error(&format!("unknown option '{other}'"))
            }
            other => positional.push(other.to_string()),
        }
        index += 1;
    }

    // A flag that means nothing to the command asked for is a mistake worth
    // reporting, not something to ignore quietly.
    if let Some(problem) = misplaced_flags(&positional, &arguments) {
        return usage_error(&problem);
    }

    if options.keep_destination && options.replace_destination {
        return usage_error(
            "--keep-destination and --replace-destination cannot both be given: they are opposite answers to the same question.",
        );
    }

    let Some(command) = positional.first().map(String::as_str) else {
        print!("{USAGE}");
        return ExitCode::from(2);
    };

    let result = match command {
        "scan" => commands::scan(&options),
        "storage" => commands::storage(&options),
        "evidence" => match positional.get(1) {
            Some(reference) => commands::evidence(&options, reference),
            None => {
                return usage_error(
                    "`evidence` needs a library. Run `librarybridge scan` to see the list.",
                )
            }
        },
        "lutris" => lutris_cmd::dispatch(&options, &positional[1..]),
        "fix" | "undo" => match positional.get(1) {
            Some(reference) => {
                if command == "fix" {
                    commands::fix(&options, reference)
                } else {
                    commands::undo(&options, reference)
                }
            }
            None => {
                return usage_error(&format!(
                    "`{command}` needs a library. Run `librarybridge scan` to see the list."
                ))
            }
        },
        "help" => {
            print!("{USAGE}");
            return ExitCode::SUCCESS;
        }
        "version" => {
            println!("librarybridge {VERSION}");
            return ExitCode::SUCCESS;
        }
        other => return usage_error(&format!("unknown command '{other}'")),
    };

    match result {
        Ok(code) => ExitCode::from(code as u8),
        Err(message) => {
            eprintln!("librarybridge: {message}");
            ExitCode::FAILURE
        }
    }
}

/// Flags each command accepts, beyond the ones every command takes.
fn misplaced_flags(positional: &[String], arguments: &[String]) -> Option<String> {
    const EVERYWHERE: [&str; 4] = ["--json", "-n", "--dry-run", "--steam-root"];
    let command = positional.first().map(String::as_str)?;
    let sub = positional.get(1).map(String::as_str).unwrap_or("");

    let allowed: &[&str] = match (command, sub) {
        ("scan", _) | ("storage", _) => &[],
        ("fix", _) => &[
            "-y",
            "--yes",
            "--force",
            "--expect",
            "--keep-destination",
            "--replace-destination",
        ],
        ("undo", _) => &["-y", "--yes"],
        ("evidence", _) => &["--record"],
        ("lutris", "scan") => &["--root", "--all"],
        ("lutris", "detect") => &[],
        ("lutris", "plan") => &["--root", "--candidate", "--output"],
        ("lutris", "import") => &["--plan", "-y", "--yes"],
        ("lutris", "forget") => &["--entry"],
        _ => return None,
    };

    let offender = arguments
        .iter()
        .filter(|argument| argument.starts_with('-'))
        .find(|argument| {
            !EVERYWHERE.contains(&argument.as_str()) && !allowed.contains(&argument.as_str())
        })?;

    let name = if sub.is_empty() {
        command.to_string()
    } else {
        format!("{command} {sub}")
    };
    Some(format!(
        "`{name}` does not take {offender}. Run `librarybridge --help` to see what it does take."
    ))
}

fn usage_error(message: &str) -> ExitCode {
    eprintln!("librarybridge: {message}");
    eprintln!("Run `librarybridge --help` for usage.");
    ExitCode::from(2)
}
