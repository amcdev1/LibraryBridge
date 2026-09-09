//! LibraryBridge: move Proton compatdata off a game drive that cannot hold it,
//! onto a filesystem that can, without ever deleting the original.

mod commands;
mod discover;
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
            | "--expect" => {
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

    if options.keep_destination && options.replace_destination {
        return usage_error(
            "--keep-destination and --replace-destination cannot both be given: they are              opposite answers to the same question.",
        );
    }

    let Some(command) = positional.first().map(String::as_str) else {
        print!("{USAGE}");
        return ExitCode::from(2);
    };

    let result = match command {
        "scan" => commands::scan(&options),
        "storage" => commands::storage(&options),
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

fn usage_error(message: &str) -> ExitCode {
    eprintln!("librarybridge: {message}");
    eprintln!("Run `librarybridge --help` for usage.");
    ExitCode::from(2)
}
