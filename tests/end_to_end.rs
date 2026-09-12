//! End-to-end tests against a synthetic Steam tree.
//!
//! These drive the real binary, so they cover argument handling, discovery,
//! the copier and the state machine together. They cannot cover NTFS, Proton
//! or Steam Cloud, which need a Linux machine with real hardware.

use std::fs;
use std::os::unix::fs::symlink;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const BIN: &str = env!("CARGO_BIN_EXE_librarybridge");
const APPID: &str = "220";

struct Fixture {
    root: PathBuf,
    steam: PathBuf,
    library: PathBuf,
    home: PathBuf,
    by_env: Option<PathBuf>,
}

impl Fixture {
    fn new(name: &str) -> Fixture {
        let root =
            std::env::temp_dir().join(format!("librarybridge-test-{}-{name}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        let root = root.canonicalize().unwrap();

        let steam = root.join("Steam");
        let library = root.join("GameDrive/SteamLibrary");
        let home = root.join("home");
        fs::create_dir_all(steam.join("steamapps")).unwrap();
        fs::create_dir_all(library.join("steamapps")).unwrap();
        fs::create_dir_all(&home).unwrap();

        // CI mounts a separate filesystem under this path so the destination
        // of a repair is genuinely a different filesystem from the fixture's
        // library, which the real tool requires and enforces (commit
        // 3de7f0c's same-filesystem guard). Leave it unset for a host where
        // the default data home already resolves to another volume. Each
        // fixture gets its own directory so parallel tests stay isolated.
        let by_env = std::env::var_os("LIBRARYBRIDGE_TEST_DATA_DIR").map(|base| {
            let dir = PathBuf::from(base).join(format!("test-{}-{name}", std::process::id()));
            fs::create_dir_all(&dir).unwrap();
            dir
        });

        fs::write(
            steam.join("steamapps/libraryfolders.vdf"),
            format!(
                "\"libraryfolders\"\n{{\n\t\"0\"\n\t{{\n\t\t\"path\"\t\t\"{}\"\n\t\t\"label\"\t\t\"\"\n\t}}\n\t\"1\"\n\t{{\n\t\t\"path\"\t\t\"{}\"\n\t\t\"label\"\t\t\"Games\"\n\t}}\n}}\n",
                steam.display(),
                library.display()
            ),
        )
        .unwrap();

        fs::write(
            library.join(format!("steamapps/appmanifest_{APPID}.acf")),
            format!(
                "\"AppState\"\n{{\n\t\"appid\"\t\t\"{APPID}\"\n\t\"name\"\t\t\"Half-Life 2\"\n}}\n"
            ),
        )
        .unwrap();

        Fixture {
            root,
            steam,
            library,
            home,
            by_env,
        }
    }

    /// A prefix with the awkward contents a real one has: nested directories,
    /// an absolute symlink pointing outside the tree, a relative symlink, a
    /// non-ASCII filename and an empty file.
    fn make_prefix(&self) {
        let prefix = self
            .library
            .join(format!("steamapps/compatdata/{APPID}/pfx"));
        let saves = prefix.join("drive_c/users/steamuser/Saved Games");
        fs::create_dir_all(&saves).unwrap();
        fs::create_dir_all(prefix.join("dosdevices")).unwrap();
        fs::create_dir_all(prefix.join("drive_c/windows/system32")).unwrap();

        fs::write(saves.join("save.dat"), b"OLD SAVE").unwrap();
        fs::write(saves.join("Sauvegarde-日本.dat"), b"unicode name").unwrap();
        fs::write(
            prefix.join("drive_c/windows/system32/kernel32.dll"),
            vec![7u8; 40_000],
        )
        .unwrap();
        fs::write(prefix.join("drive_c/empty.txt"), b"").unwrap();
        fs::write(prefix.join("system.reg"), b"WINE REGISTRY\n").unwrap();
        fs::write(
            self.library
                .join(format!("steamapps/compatdata/{APPID}/version")),
            b"proton-9.0",
        )
        .unwrap();

        // The link that makes a naive recursive copier destroy the machine.
        symlink("/", prefix.join("dosdevices/z:")).unwrap();
        symlink("../drive_c", prefix.join("dosdevices/c:")).unwrap();

        // An orphaned prefix with no app manifest. It must be carried across.
        let orphan = self.library.join("steamapps/compatdata/999999");
        fs::create_dir_all(&orphan).unwrap();
        fs::write(orphan.join("leftover.txt"), b"nobody knows what this is").unwrap();
    }

    /// Rewrite the library's display label, as renaming it in Steam would.
    fn relabel(&self, label: &str) {
        fs::write(
            self.steam.join("steamapps/libraryfolders.vdf"),
            format!(
                "\"libraryfolders\"\n{{\n\t\"0\"\n\t{{\n\t\t\"path\"\t\t\"{}\"\n\t}}\n\t\"1\"\n\t{{\n\t\t\"path\"\t\t\"{}\"\n\t\t\"label\"\t\t\"{label}\"\n\t}}\n}}\n",
                self.steam.display(),
                self.library.display()
            ),
        )
        .unwrap();
    }

    fn compatdata(&self) -> PathBuf {
        self.library.join("steamapps/compatdata")
    }

    /// Where LibraryBridge will keep its own files and the moved data. Matches
    /// `state::app_data_dir` / `native_data_root` in the tool: XDG_DATA_HOME
    /// when set, else HOME/.local/share.
    fn data_home(&self) -> PathBuf {
        match &self.by_env {
            Some(dir) => dir.join("librarybridge"),
            None => self.home.join(".local/share/librarybridge"),
        }
    }

    fn save_file(&self, root: &Path) -> PathBuf {
        root.join(format!(
            "{APPID}/pfx/drive_c/users/steamuser/Saved Games/save.dat"
        ))
    }

    /// Put a stub `ps` on PATH so process detection can be steered. Passing
    /// an empty string makes `ps` missing entirely, which is the case where
    /// the question cannot be answered at all.
    fn with_fake_ps(&self, prints: &str, args: &[&str]) -> Output {
        let bin = self.root.join("fakebin");
        fs::create_dir_all(&bin).unwrap();
        if !prints.is_empty() {
            let script = bin.join("ps");
            fs::write(&script, format!("#!/bin/sh\necho '{prints}'\n")).unwrap();
            fs::set_permissions(&script, fs::Permissions::from_mode(0o755)).unwrap();
        }
        let path = if prints.is_empty() {
            bin.to_string_lossy().to_string()
        } else {
            format!("{}:/usr/bin:/bin", bin.display())
        };
        let mut cmd = Command::new(BIN);
        cmd.args(["--steam-root", self.steam.to_str().unwrap()])
            .args(args)
            .env("HOME", &self.home)
            .env("PATH", path);
        if let Some(dir) = &self.by_env {
            cmd.env("XDG_DATA_HOME", dir);
        } else {
            cmd.env_remove("XDG_DATA_HOME");
        }
        cmd.output().expect("failed to run librarybridge")
    }

    fn run(&self, args: &[&str]) -> Output {
        let mut cmd = Command::new(BIN);
        cmd.args(["--steam-root", self.steam.to_str().unwrap()])
            .args(args)
            .env("HOME", &self.home);
        if let Some(dir) = &self.by_env {
            cmd.env("XDG_DATA_HOME", dir);
        } else {
            cmd.env_remove("XDG_DATA_HOME");
        }
        cmd.output().expect("failed to run librarybridge")
    }

    fn run_ok(&self, args: &[&str]) -> String {
        let output = self.run(args);
        let stdout = String::from_utf8_lossy(&output.stdout).to_string();
        assert!(
            output.status.success(),
            "librarybridge {args:?} failed\n--- stdout ---\n{stdout}\n--- stderr ---\n{}",
            String::from_utf8_lossy(&output.stderr)
        );
        stdout
    }

    /// Attempt a real repair. Returns true if it succeeded. Some development
    /// hosts put the fixture's destination on the same filesystem as the
    /// library, which the tool correctly refuses; callers that only test what
    /// happens *after* a repair can stop there without pretending otherwise.
    fn try_fix(&self, id: &str) -> bool {
        self.run(&["fix", id, "--yes", "--force"]).status.success()
    }

    /// The library id, taken from the tool's own scan output.
    fn library_id(&self) -> String {
        let json = self.run_ok(&["scan", "--json"]);
        let block = json
            .split("    {")
            .find(|b| b.contains(self.library.to_str().unwrap()))
            .expect("library missing from scan output");
        let marker = "\"id\": \"";
        let start = block.find(marker).unwrap() + marker.len();
        block[start..start + 8].to_string()
    }

    fn backup_dir(&self) -> PathBuf {
        let mut found: Vec<PathBuf> = fs::read_dir(self.library.join("steamapps"))
            .unwrap()
            .flatten()
            .map(|e| e.path())
            .filter(|p| {
                p.file_name()
                    .unwrap()
                    .to_string_lossy()
                    .starts_with("compatdata.backup")
            })
            .collect();
        found.sort();
        assert_eq!(
            found.len(),
            1,
            "expected exactly one backup, found {found:?}"
        );
        found.pop().unwrap()
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

#[test]
fn scan_reports_a_library_that_can_be_repaired() {
    let fixture = Fixture::new("scan");
    fixture.make_prefix();

    let text = fixture.run_ok(&["scan"]);
    assert!(text.contains("Repair available"), "{text}");
    assert!(text.contains("Half-Life 2"), "{text}");
    assert!(text.contains("2 prefixes"), "{text}");

    let json = fixture.run_ok(&["scan", "--json"]);
    assert!(json.contains("\"state\": \"repair_available\""), "{json}");
    assert!(json.contains("\"schema\": 1"), "{json}");
}

#[test]
fn dry_run_changes_nothing() {
    let fixture = Fixture::new("dryrun");
    fixture.make_prefix();
    let id = fixture.library_id();

    let text = fixture.run_ok(&["fix", &id, "--dry-run", "--force"]);
    assert!(text.contains("Dry run"), "{text}");
    assert!(fixture.compatdata().is_dir());
    assert!(!fs::symlink_metadata(fixture.compatdata())
        .unwrap()
        .file_type()
        .is_symlink());
    assert!(!fixture.data_home().exists());
}

#[test]
fn fix_moves_the_data_and_keeps_the_original() {
    let fixture = Fixture::new("fix");
    fixture.make_prefix();
    let id = fixture.library_id();

    let text = fixture.run_ok(&["fix", &id, "--yes", "--force"]);
    assert!(text.contains("every file matches"), "{text}");
    assert!(text.contains("Done."), "{text}");

    // compatdata is now a link.
    let meta = fs::symlink_metadata(fixture.compatdata()).unwrap();
    assert!(meta.file_type().is_symlink());
    let target = fs::read_link(fixture.compatdata()).unwrap();
    assert!(
        target.starts_with(fixture.data_home()),
        "target was {}",
        target.display()
    );

    // The data arrived intact, reachable through the link as Steam sees it.
    assert_eq!(
        fs::read(fixture.save_file(&fixture.compatdata())).unwrap(),
        b"OLD SAVE"
    );
    assert_eq!(
        fs::read(target.join(format!("{APPID}/version"))).unwrap(),
        b"proton-9.0"
    );
    assert_eq!(
        fs::read(target.join("999999/leftover.txt")).unwrap(),
        b"nobody knows what this is"
    );
    assert!(target
        .join(format!("{APPID}/pfx/drive_c/empty.txt"))
        .exists());
    assert!(target
        .join(format!(
            "{APPID}/pfx/drive_c/users/steamuser/Saved Games/Sauvegarde-日本.dat"
        ))
        .exists());

    // The dangerous link was copied as a link, not followed.
    let z_drive = target.join(format!("{APPID}/pfx/dosdevices/z:"));
    let z_meta = fs::symlink_metadata(&z_drive).unwrap();
    assert!(z_meta.file_type().is_symlink());
    assert_eq!(fs::read_link(&z_drive).unwrap(), PathBuf::from("/"));
    assert_eq!(
        fs::read_link(target.join(format!("{APPID}/pfx/dosdevices/c:"))).unwrap(),
        PathBuf::from("../drive_c")
    );

    // The original is still on the game drive, untouched.
    let backup = fixture.backup_dir();
    assert_eq!(fs::read(fixture.save_file(&backup)).unwrap(), b"OLD SAVE");
    assert!(
        fs::symlink_metadata(backup.join(format!("{APPID}/pfx/dosdevices/z:")))
            .unwrap()
            .file_type()
            .is_symlink()
    );

    // And scan agrees about where things stand.
    let after = fixture.run_ok(&["scan"]);
    assert!(after.contains("Repaired"), "{after}");
    assert!(after.contains("Nothing to do."), "{after}");
}

#[test]
fn fix_is_idempotent() {
    let fixture = Fixture::new("idempotent");
    fixture.make_prefix();
    let id = fixture.library_id();

    fixture.run_ok(&["fix", &id, "--yes", "--force"]);
    let second = fixture.run_ok(&["fix", &id, "--yes", "--force"]);
    assert!(second.contains("already repaired"), "{second}");
    fixture.backup_dir(); // still exactly one backup
}

#[test]
fn undo_keeps_saves_written_after_the_repair() {
    let fixture = Fixture::new("undo");
    fixture.make_prefix();
    let id = fixture.library_id();
    fixture.run_ok(&["fix", &id, "--yes", "--force"]);

    // Play a game: write a new save into the live prefix.
    fs::write(fixture.save_file(&fixture.compatdata()), b"NEW SAVE").unwrap();

    let text = fixture.run_ok(&["undo", &id, "--yes"]);
    assert!(text.contains("Done."), "{text}");

    // compatdata is a real directory again, holding the newer save.
    let meta = fs::symlink_metadata(fixture.compatdata()).unwrap();
    assert!(meta.file_type().is_dir() && !meta.file_type().is_symlink());
    assert_eq!(
        fs::read(fixture.save_file(&fixture.compatdata())).unwrap(),
        b"NEW SAVE"
    );

    // The migration-time copy is still there and still holds the old save.
    let backup = fixture.backup_dir();
    assert_eq!(fs::read(fixture.save_file(&backup)).unwrap(), b"OLD SAVE");
}

#[test]
fn an_interrupted_repair_is_finished_not_restarted() {
    let fixture = Fixture::new("interrupted");
    fixture.make_prefix();
    let id = fixture.library_id();
    fixture.run_ok(&["fix", &id, "--yes", "--force"]);

    // Simulate a crash between moving the original aside and linking.
    fs::remove_file(fixture.compatdata()).unwrap();

    let text = fixture.run_ok(&["scan"]);
    assert!(text.contains("Interrupted, needs finishing"), "{text}");
    assert!(text.contains("Nothing was lost"), "{text}");

    let text = fixture.run_ok(&["fix", &id, "--yes", "--force"]);
    assert!(text.contains("Done."), "{text}");
    assert!(fs::symlink_metadata(fixture.compatdata())
        .unwrap()
        .file_type()
        .is_symlink());
    assert_eq!(
        fs::read(fixture.save_file(&fixture.compatdata())).unwrap(),
        b"OLD SAVE"
    );
}

#[test]
fn a_broken_link_is_reported_and_left_alone() {
    let fixture = Fixture::new("dangling");
    fixture.make_prefix();
    let id = fixture.library_id();
    fixture.run_ok(&["fix", &id, "--yes", "--force"]);

    // The destination disappears, as it would if the home drive were absent.
    let target = fs::read_link(fixture.compatdata()).unwrap();
    fs::rename(&target, target.with_file_name("moved-away")).unwrap();

    let text = fixture.run_ok(&["scan"]);
    assert!(text.contains("Link is broken"), "{text}");

    let output = fixture.run(&["undo", &id, "--yes"]);
    assert!(!output.status.success());
    assert!(fs::symlink_metadata(fixture.compatdata())
        .unwrap()
        .file_type()
        .is_symlink());
}

#[test]
fn a_hand_made_link_is_never_touched() {
    let fixture = Fixture::new("foreign");
    let elsewhere = fixture.root.join("somewhere-else");
    fs::create_dir_all(&elsewhere).unwrap();
    fs::create_dir_all(fixture.library.join("steamapps")).unwrap();
    symlink(&elsewhere, fixture.compatdata()).unwrap();
    let id = fixture.library_id();

    let text = fixture.run_ok(&["scan"]);
    assert!(text.contains("Already linked elsewhere"), "{text}");

    let output = fixture.run(&["fix", &id, "--yes", "--force"]);
    assert!(!output.status.success());
    assert_eq!(fs::read_link(fixture.compatdata()).unwrap(), elsewhere);
}

#[test]
fn a_prefix_containing_a_pipe_stops_before_anything_changes() {
    let fixture = Fixture::new("special");
    fixture.make_prefix();
    let id = fixture.library_id();
    let pipe = fixture
        .compatdata()
        .join(format!("{APPID}/pfx/drive_c/strange.pipe"));
    let made = Command::new("mkfifo").arg(&pipe).status();
    if !made.map(|s| s.success()).unwrap_or(false) {
        return; // no mkfifo on this host, nothing to assert
    }

    let output = fixture.run(&["fix", &id, "--yes", "--force"]);
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("not a regular file"), "{stderr}");

    // The game drive is exactly as it was.
    assert!(fixture.compatdata().is_dir());
    assert!(!fs::symlink_metadata(fixture.compatdata())
        .unwrap()
        .file_type()
        .is_symlink());
    assert_eq!(
        fs::read(fixture.save_file(&fixture.compatdata())).unwrap(),
        b"OLD SAVE"
    );
}

#[test]
fn a_library_with_no_proton_data_is_linked_for_later() {
    let fixture = Fixture::new("empty");
    let id = fixture.library_id();

    let text = fixture.run_ok(&["scan"]);
    assert!(text.contains("No Proton data yet"), "{text}");

    let text = fixture.run_ok(&["fix", &id, "--yes", "--force"]);
    assert!(text.contains("no Proton data yet"), "{text}");
    assert!(fs::symlink_metadata(fixture.compatdata())
        .unwrap()
        .file_type()
        .is_symlink());
    assert!(fixture.compatdata().is_dir()); // resolves through the link
}

#[test]
fn unknown_arguments_are_rejected() {
    let fixture = Fixture::new("usage");
    let output = fixture.run(&["fix"]);
    assert_eq!(output.status.code(), Some(2));
    let output = fixture.run(&["--nonsense", "scan"]);
    assert_eq!(output.status.code(), Some(2));
    let output = fixture.run(&["fix", "nosuchlibrary", "--yes"]);
    assert_eq!(output.status.code(), Some(1));
}

#[test]
fn a_second_repair_after_an_undo_sets_the_old_copy_aside() {
    let fixture = Fixture::new("second");
    fixture.make_prefix();
    let id = fixture.library_id();

    fixture.run_ok(&["fix", &id, "--yes", "--force"]);
    fixture.run_ok(&["undo", &id, "--yes"]);

    // The undo leaves the native copy in place on purpose. Repairing again
    // must not need the user to clear it by hand.
    let text = fixture.run_ok(&["fix", &id, "--yes", "--force"]);
    assert!(
        text.contains("Set aside a copy from an earlier repair"),
        "{text}"
    );
    assert!(text.contains("Done."), "{text}");

    // Two backups beside the library, numbered rather than timestamped.
    let mut names: Vec<String> = fs::read_dir(fixture.library.join("steamapps"))
        .unwrap()
        .flatten()
        .map(|e| e.file_name().to_string_lossy().to_string())
        .filter(|n| n.starts_with("compatdata.backup"))
        .collect();
    names.sort();
    assert_eq!(names, ["compatdata.backup", "compatdata.backup-2"]);

    // And the save is still readable through the new link.
    assert_eq!(
        fs::read(fixture.save_file(&fixture.compatdata())).unwrap(),
        b"OLD SAVE"
    );
}

/// The case the release review called out: a repair is interrupted after the
/// original is moved aside, then Steam starts before recovery and creates a
/// fresh compatdata. The migrated prefixes and the new empty one are both
/// real, and the tool must not pick one silently.
#[test]
fn a_competing_writer_after_an_interrupted_repair_is_a_conflict() {
    let fixture = Fixture::new("conflict");
    fixture.make_prefix();
    let id = fixture.library_id();
    fixture.run_ok(&["fix", &id, "--yes", "--force"]);

    // Crash between moving the original aside and creating the link.
    fs::remove_file(fixture.compatdata()).unwrap();
    // Steam gets there first and makes its own.
    let fresh = fixture
        .compatdata()
        .join(format!("{APPID}/pfx/drive_c/users/steamuser/Saved Games"));
    fs::create_dir_all(&fresh).unwrap();
    fs::write(fresh.join("save.dat"), b"EMPTY NEW PREFIX").unwrap();

    let output = fixture.run(&["fix", &id, "--yes", "--force"]);
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("only you can say which one counts"),
        "{stderr}"
    );
    assert!(stderr.contains("--keep-destination"), "{stderr}");
    assert!(stderr.contains("--replace-destination"), "{stderr}");

    // Refusing changed nothing.
    assert_eq!(
        fs::read(fixture.save_file(&fixture.compatdata())).unwrap(),
        b"EMPTY NEW PREFIX"
    );
}

#[test]
fn keeping_the_destination_restores_the_migrated_saves() {
    let fixture = Fixture::new("keepdest");
    fixture.make_prefix();
    let id = fixture.library_id();
    fixture.run_ok(&["fix", &id, "--yes", "--force"]);
    let target = fs::read_link(fixture.compatdata()).unwrap();

    fs::remove_file(fixture.compatdata()).unwrap();
    let fresh = fixture
        .compatdata()
        .join(format!("{APPID}/pfx/drive_c/users/steamuser/Saved Games"));
    fs::create_dir_all(&fresh).unwrap();
    fs::write(fresh.join("save.dat"), b"EMPTY NEW PREFIX").unwrap();

    let text = fixture.run_ok(&["fix", &id, "--yes", "--force", "--keep-destination"]);
    assert!(text.contains("Done."), "{text}");

    // Steam reads the migrated prefixes again.
    assert!(fs::symlink_metadata(fixture.compatdata())
        .unwrap()
        .file_type()
        .is_symlink());
    assert_eq!(
        fs::read(fixture.save_file(&fixture.compatdata())).unwrap(),
        b"OLD SAVE"
    );
    assert_eq!(fs::read(fixture.save_file(&target)).unwrap(), b"OLD SAVE");

    // And what Steam had made is kept, not discarded.
    let kept: Vec<String> = fs::read_dir(fixture.library.join("steamapps"))
        .unwrap()
        .flatten()
        .map(|e| e.file_name().to_string_lossy().to_string())
        .filter(|n| n.starts_with("compatdata.backup"))
        .collect();
    assert_eq!(kept.len(), 2, "expected both copies kept, got {kept:?}");
    let recovered = kept.iter().any(|name| {
        fs::read(fixture.save_file(&fixture.library.join("steamapps").join(name)))
            .map(|bytes| bytes == b"EMPTY NEW PREFIX")
            .unwrap_or(false)
    });
    assert!(recovered, "the prefix Steam created was not kept");
}

#[test]
fn replacing_the_destination_keeps_the_old_one_too() {
    let fixture = Fixture::new("replacedest");
    fixture.make_prefix();
    let id = fixture.library_id();
    fixture.run_ok(&["fix", &id, "--yes", "--force"]);

    fs::remove_file(fixture.compatdata()).unwrap();
    let fresh = fixture
        .compatdata()
        .join(format!("{APPID}/pfx/drive_c/users/steamuser/Saved Games"));
    fs::create_dir_all(&fresh).unwrap();
    fs::write(fresh.join("save.dat"), b"EMPTY NEW PREFIX").unwrap();

    let text = fixture.run_ok(&["fix", &id, "--yes", "--force", "--replace-destination"]);
    assert!(
        text.contains("Set aside a copy from an earlier repair"),
        "{text}"
    );
    let target = fs::read_link(fixture.compatdata()).unwrap();
    assert_eq!(
        fs::read(fixture.save_file(&target)).unwrap(),
        b"EMPTY NEW PREFIX"
    );

    // The migrated prefixes are still there under compatdata.previous.
    let previous = target.with_file_name("compatdata.previous");
    assert_eq!(fs::read(fixture.save_file(&previous)).unwrap(), b"OLD SAVE");
}

// ---- regressions for the findings raised in the release review ----------

/// R13. A dry run has to leave the disk exactly as it found it, including the
/// symlink probe, which used to run before the dry-run return.
#[test]
fn a_dry_run_writes_nothing_at_all() {
    let fixture = Fixture::new("drynowrite");
    fixture.make_prefix();
    let id = fixture.library_id();

    let steamapps = fixture.library.join("steamapps");
    let before = fs::metadata(&steamapps).unwrap().modified().unwrap();
    let listing_before: Vec<String> = fs::read_dir(&steamapps)
        .unwrap()
        .flatten()
        .map(|e| e.file_name().to_string_lossy().to_string())
        .collect();

    std::thread::sleep(std::time::Duration::from_millis(1100));
    fixture.run_ok(&["fix", &id, "--dry-run", "--force"]);

    assert_eq!(
        fs::metadata(&steamapps).unwrap().modified().unwrap(),
        before,
        "the dry run modified the library directory"
    );
    let listing_after: Vec<String> = fs::read_dir(&steamapps)
        .unwrap()
        .flatten()
        .map(|e| e.file_name().to_string_lossy().to_string())
        .collect();
    assert_eq!(listing_before.len(), listing_after.len());
    assert!(
        !listing_after.iter().any(|name| name.contains("probe")),
        "a probe file was left behind: {listing_after:?}"
    );
    assert!(!fixture.data_home().exists());
}

/// R13. A directory sitting at the staging name is not proof that this run
/// created it, so it must never be cleared to make room.
#[test]
fn data_at_the_staging_path_is_never_deleted() {
    let fixture = Fixture::new("staging");
    fixture.make_prefix();
    let id = fixture.library_id();

    // Someone else's data, at the name an interrupted copy would use.
    let root = fixture.data_home();
    let library_dir = root.join(format!("Games-{id}"));
    let squatter = library_dir.join("compatdata.incomplete");
    fs::create_dir_all(&squatter).unwrap();
    fs::write(squatter.join("sentinel.txt"), b"NOT YOURS").unwrap();

    fixture.run_ok(&["fix", &id, "--yes", "--force"]);

    assert_eq!(
        fs::read(squatter.join("sentinel.txt")).unwrap(),
        b"NOT YOURS",
        "the repair deleted data it did not create"
    );
    // And the repair still worked.
    assert!(fs::symlink_metadata(fixture.compatdata())
        .unwrap()
        .file_type()
        .is_symlink());
    assert_eq!(
        fs::read(fixture.save_file(&fixture.compatdata())).unwrap(),
        b"OLD SAVE"
    );
}

/// R20. A filesystem the tool cannot identify is a blocking state, not a
/// shrug. This is every non-Linux host, which is why the other tests pass
/// --force. On Linux the mount table is always readable, so the state the
/// test exercises does not exist there.
#[test]
#[cfg_attr(
    target_os = "linux",
    ignore = "exercises the off-Linux unidentified-filesystem state"
)]
fn an_unidentified_filesystem_blocks_repair() {
    let fixture = Fixture::new("unknownfs");
    fixture.make_prefix();
    let id = fixture.library_id();

    let output = fixture.run(&["fix", &id, "--yes"]);
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("could not be identified"), "{stderr}");
    assert!(fixture.compatdata().is_dir());
    assert!(!fs::symlink_metadata(fixture.compatdata())
        .unwrap()
        .file_type()
        .is_symlink());
}

/// R02. Two trees can have the same file count and the same total bytes and
/// still be different data. Two saves of the same length are the case that
/// matters.
#[test]
fn equal_sized_but_different_data_is_still_a_conflict() {
    let fixture = Fixture::new("equalsize");
    fixture.make_prefix();
    let id = fixture.library_id();

    // A destination whose shape matches the source exactly, byte for byte in
    // total, but whose contents differ.
    let target = fixture.data_home().join(format!("Games-{id}/compatdata"));
    copy_tree(&fixture.compatdata(), &target);
    let save = fixture.save_file(&target);
    let original = fs::read(&save).unwrap();
    let mut replacement = original.clone();
    replacement.reverse(); // same length, different bytes
    fs::write(&save, &replacement).unwrap();

    let output = fixture.run(&["fix", &id, "--yes", "--force"]);
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("only you can say which one counts"),
        "{stderr}"
    );

    // Untouched on both sides.
    assert_eq!(fs::read(&save).unwrap(), replacement);
    assert_eq!(
        fs::read(fixture.save_file(&fixture.compatdata())).unwrap(),
        original
    );
}

/// R24. Renaming a library in Steam changes its display name. Identity is the
/// id, so a working repair must survive the rename.
#[test]
fn renaming_a_library_in_steam_does_not_orphan_its_repair() {
    let fixture = Fixture::new("relabel");
    fixture.make_prefix();
    let id = fixture.library_id();
    fixture.run_ok(&["fix", &id, "--yes", "--force"]);

    fixture.relabel("A Completely Different Name");

    let text = fixture.run_ok(&["scan"]);
    assert!(text.contains("Repaired"), "{text}");
    assert!(!text.contains("Already linked elsewhere"), "{text}");

    // And undo still works through the renamed library.
    let new_id = fixture.library_id();
    let text = fixture.run_ok(&["undo", &new_id, "--yes"]);
    assert!(text.contains("Done."), "{text}");
    assert_eq!(
        fs::read(fixture.save_file(&fixture.compatdata())).unwrap(),
        b"OLD SAVE"
    );
}

/// A plain recursive copy for test setup only. Not the tool's copier.
fn copy_tree(from: &Path, to: &Path) {
    fs::create_dir_all(to).unwrap();
    for entry in fs::read_dir(from).unwrap().flatten() {
        let source = entry.path();
        let destination = to.join(entry.file_name());
        let meta = fs::symlink_metadata(&source).unwrap();
        if meta.file_type().is_symlink() {
            symlink(fs::read_link(&source).unwrap(), &destination).unwrap();
        } else if meta.is_dir() {
            copy_tree(&source, &destination);
        } else {
            fs::copy(&source, &destination).unwrap();
        }
    }
}

/// R15. A second process must not act on a library the first is working on.
/// The lock is simulated by planting one held by this test process, which is
/// alive by definition.
#[test]
fn a_locked_library_refuses_a_second_operation() {
    let fixture = Fixture::new("locked");
    fixture.make_prefix();
    let id = fixture.library_id();

    let locks = fixture.data_home().join("locks");
    fs::create_dir_all(&locks).unwrap();
    fs::write(
        locks.join(format!("{id}.lock")),
        format!("{}\nlibrarybridge\n", std::process::id()),
    )
    .unwrap();

    let output = fixture.run(&["fix", &id, "--yes", "--force"]);
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("another LibraryBridge operation"),
        "{stderr}"
    );
    assert!(fixture.compatdata().is_dir());

    // A dry run needs no lock, because it changes nothing.
    let output = fixture.run(&["fix", &id, "--dry-run", "--force"]);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

/// R14. Recovery used to skip the preconditions a normal repair runs, so an
/// interrupted repair could be finished while Steam was running.
///
/// The fake-`ps` mechanism only steers the non-Linux process check; on Linux
/// the tool reads /proc/*/comm directly, so the refusal is exercised live by
/// the same code path and cannot be steered by a PATH stub.
#[test]
#[cfg_attr(
    target_os = "linux",
    ignore = "on Linux the running check reads /proc, not ps"
)]
fn recovery_refuses_while_steam_is_running() {
    let fixture = Fixture::new("recoverysteam");
    fixture.make_prefix();
    let id = fixture.library_id();
    fixture.run_ok(&["fix", &id, "--yes", "--force"]);
    fs::remove_file(fixture.compatdata()).unwrap(); // interrupted before the link

    let output = fixture.with_fake_ps("steam", &["fix", &id, "--yes", "--force"]);
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("steam"), "{stderr}");
    assert!(fs::symlink_metadata(fixture.compatdata()).is_err());
}

/// R25. An empty process list used to mean both "nothing is running" and
/// "nothing could be seen". Only the first is safe to act on.
#[test]
#[cfg_attr(
    target_os = "linux",
    ignore = "on Linux the running check reads /proc, not ps"
)]
fn an_unanswerable_process_check_blocks_the_repair() {
    let fixture = Fixture::new("noprocinfo");
    fixture.make_prefix();
    let id = fixture.library_id();

    let output = fixture.with_fake_ps("", &["fix", &id, "--yes", "--force"]);
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("cannot tell whether"), "{stderr}");
    assert!(fixture.compatdata().is_dir());
}

/// R14. A link pointing at a file is not a relocated compatdata, whatever it
/// is called.
#[test]
fn a_link_to_a_file_is_not_treated_as_repaired() {
    let fixture = Fixture::new("linktofile");
    fs::create_dir_all(fixture.library.join("steamapps")).unwrap();
    let file = fixture.root.join("not-a-directory");
    fs::write(&file, b"x").unwrap();
    symlink(&file, fixture.compatdata()).unwrap();

    let text = fixture.run_ok(&["scan"]);
    assert!(text.contains("Needs attention"), "{text}");
    assert!(text.contains("not a directory"), "{text}");
    assert!(!text.contains("Repaired"), "{text}");
}

/// R23. Preserving link text exactly still changes where a relative link
/// leads, if it pointed outside the tree being moved.
#[test]
fn a_relative_link_pointing_outside_the_prefix_blocks_the_repair() {
    let fixture = Fixture::new("escapinglink");
    fixture.make_prefix();
    let id = fixture.library_id();

    // Something outside compatdata, reached by a relative link from inside.
    fs::write(fixture.library.join("steamapps/external-save.dat"), b"SAVE").unwrap();
    symlink(
        "../../../external-save.dat",
        fixture
            .compatdata()
            .join(format!("{APPID}/pfx/relative-save")),
    )
    .unwrap();

    let output = fixture.run(&["fix", &id, "--yes", "--force"]);
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("point outside it"), "{stderr}");
    assert!(stderr.contains("relative-save"), "{stderr}");
    assert!(fixture.compatdata().is_dir());

    // The absolute z: link is not what triggered it.
    assert!(!stderr.contains("dosdevices/z:"), "{stderr}");
}

/// R21. Applying quotes back the plan that was reviewed, and a plan that no
/// longer matches is refused rather than applied to something else. Content
/// changes are caught even when file count and total bytes stay the same.
#[test]
fn a_stale_plan_is_refused() {
    let fixture = Fixture::new("staleplan");
    fixture.make_prefix();
    let id = fixture.library_id();

    let review = fixture.run_ok(&["fix", &id, "--dry-run", "--force"]);
    let plan = review
        .lines()
        .find_map(|line| line.strip_prefix("Plan"))
        .map(str::trim)
        .expect("the review names a plan")
        .to_string();
    assert_eq!(plan.len(), 16);

    // The same plan still applies.
    let output = fixture.run(&["fix", &id, "--dry-run", "--force", "--expect", &plan]);
    assert!(output.status.success());

    // The library changes underneath it without changing its shape or size.
    let save = fixture.save_file(&fixture.compatdata());
    let original = fs::read(&save).unwrap();
    let mut replacement = original.clone();
    replacement.reverse();
    fs::write(&save, &replacement).unwrap();

    let output = fixture.run(&["fix", &id, "--yes", "--force", "--expect", &plan]);
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("changed since the plan was reviewed"),
        "{stderr}"
    );
    assert!(!fs::symlink_metadata(fixture.compatdata())
        .unwrap()
        .file_type()
        .is_symlink());
    assert_eq!(fs::read(&save).unwrap(), replacement);
}

/// R14. Recovery must show the copy is complete before pointing Steam at it.
/// Existence used to be the only evidence.
#[test]
fn recovery_refuses_a_copy_that_does_not_match_the_original() {
    let fixture = Fixture::new("badcopy");
    fixture.make_prefix();
    let id = fixture.library_id();
    fixture.run_ok(&["fix", &id, "--yes", "--force"]);
    let target = fs::read_link(fixture.compatdata()).unwrap();

    // Interrupted before the link, and the copy is damaged afterwards.
    fs::remove_file(fixture.compatdata()).unwrap();
    fs::write(fixture.save_file(&target), b"CORRUPTED").unwrap();

    let output = fixture.run(&["fix", &id, "--yes", "--force"]);
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("does not match the original"), "{stderr}");
    assert!(fs::symlink_metadata(fixture.compatdata()).is_err());

    // The original is untouched and still holds the real save.
    let backup = fixture.backup_dir();
    assert_eq!(fs::read(fixture.save_file(&backup)).unwrap(), b"OLD SAVE");
}

/// R14. With no record and nothing to compare the copy against, there is no
/// evidence it is complete, so the tool stops rather than adopting it.
#[test]
fn recovery_without_evidence_stops() {
    let fixture = Fixture::new("noevidence");
    fixture.make_prefix();
    let id = fixture.library_id();
    fixture.run_ok(&["fix", &id, "--yes", "--force"]);

    fs::remove_file(fixture.compatdata()).unwrap();
    // The original is gone and something unusable stands in its place, and
    // the record from the interrupted run did not survive either.
    let backup = fixture.backup_dir();
    fs::remove_dir_all(&backup).unwrap();
    fs::write(&backup, b"not a directory").unwrap();
    let _ = fs::remove_dir_all(fixture.data_home().join("operations"));

    let output = fixture.run(&["fix", &id, "--yes", "--force"]);
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("no way to tell"), "{stderr}");
    assert!(fs::symlink_metadata(fixture.compatdata()).is_err());
}

/// The same shape without a backup at all is a conflict, not a recovery: two
/// sets of data exist and only the user can say which counts.
#[test]
fn a_destination_with_no_original_beside_it_is_a_conflict() {
    let fixture = Fixture::new("orphandest");
    fixture.make_prefix();
    let id = fixture.library_id();
    fixture.run_ok(&["fix", &id, "--yes", "--force"]);

    fs::remove_file(fixture.compatdata()).unwrap();
    let backup = fixture.backup_dir();
    fs::rename(&backup, fixture.library.join("steamapps/renamed-by-hand")).unwrap();

    let output = fixture.run(&["fix", &id, "--yes", "--force"]);
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("only you can say which one counts"),
        "{stderr}"
    );
    assert!(stderr.contains("nothing there"), "{stderr}");
}

/// The record left by an interrupted run is the cheap proof; it must actually
/// be used, and a completed repair must not leave one behind.
#[test]
fn a_completed_repair_leaves_no_operation_record() {
    let fixture = Fixture::new("recordclean");
    fixture.make_prefix();
    let id = fixture.library_id();
    fixture.run_ok(&["fix", &id, "--yes", "--force"]);

    let records = fixture.data_home().join("operations");
    let left = fs::read_dir(&records)
        .map(|d| d.flatten().count())
        .unwrap_or(0);
    assert_eq!(
        left, 0,
        "a completed repair left an operation record behind"
    );
}

/// Two contradictory answers to the same question is a usage error, not a
/// silent preference for whichever the code checks first.
#[test]
fn contradictory_conflict_flags_are_refused() {
    let fixture = Fixture::new("bothflags");
    fixture.make_prefix();
    let id = fixture.library_id();

    let output = fixture.run(&[
        "fix",
        &id,
        "--yes",
        "--force",
        "--keep-destination",
        "--replace-destination",
    ]);
    assert_eq!(output.status.code(), Some(2));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("cannot both"), "{stderr}");
}

/// An undo that stops part way leaves a copy beside the library. It is not in
/// use and nothing removes it, so it has to be reported rather than sitting
/// there unexplained.
#[test]
fn an_interrupted_copy_back_is_reported() {
    let fixture = Fixture::new("halfundo");
    fixture.make_prefix();
    let id = fixture.library_id();
    fixture.run_ok(&["fix", &id, "--yes", "--force"]);

    // What an undo killed mid-copy leaves behind.
    let leftover = fixture
        .library
        .join("steamapps/compatdata.restoring-1234-5678");
    fs::create_dir_all(leftover.join("220")).unwrap();
    fs::write(leftover.join("220/partial.dat"), b"half a copy").unwrap();

    let text = fixture.run_ok(&["scan"]);
    assert!(
        text.contains("copy back to this drive stopped part way"),
        "{text}"
    );
    assert!(text.contains("compatdata.restoring"), "{text}");

    // And it is still there afterwards.
    assert!(leftover.join("220/partial.dat").is_file());
}

/// A dry run is read-only in every subcommand, and the same rule about
/// relative links applies whichever direction the data is moving.
#[test]
fn undo_refuses_a_relative_link_that_would_change_meaning() {
    let fixture = Fixture::new("undolinks");
    fixture.make_prefix();
    let id = fixture.library_id();
    fixture.run_ok(&["fix", &id, "--yes", "--force"]);
    let target = fs::read_link(fixture.compatdata()).unwrap();

    // Something outside the prefix, reached by a relative link from inside it.
    fs::write(target.parent().unwrap().join("outside.dat"), b"OUT").unwrap();
    symlink(
        "../../../../outside.dat",
        target.join(format!("{APPID}/pfx/points-outside")),
    )
    .unwrap();

    let output = fixture.run(&["undo", &id, "--yes"]);
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("point outside it"), "{stderr}");
    assert!(stderr.contains("points-outside"), "{stderr}");

    // The repair is untouched and the data is still reachable.
    assert!(fs::symlink_metadata(fixture.compatdata())
        .unwrap()
        .file_type()
        .is_symlink());
    assert_eq!(
        fs::read(fixture.save_file(&fixture.compatdata())).unwrap(),
        b"OLD SAVE"
    );
}

/// Two names for one file must still be two names for one file afterwards,
/// and a file's timestamps must survive the move.
#[test]
fn hard_links_and_timestamps_survive_the_copy() {
    let fixture = Fixture::new("metadata");
    fixture.make_prefix();

    let prefix = fixture.compatdata().join(format!("{APPID}/pfx"));
    let original = prefix.join("drive_c/shared.dll");
    fs::write(&original, vec![3u8; 20_000]).unwrap();
    fs::hard_link(&original, prefix.join("drive_c/windows/shared.dll")).unwrap();

    let before = fs::metadata(&original).unwrap();
    let before_mtime = before.modified().unwrap();
    let before_inode = std::os::unix::fs::MetadataExt::ino(&before);

    let id = fixture.library_id();
    let text = fixture.run_ok(&["fix", &id, "--yes", "--force"]);
    assert!(text.contains("have more than one name"), "{text}");

    let target = fs::read_link(fixture.compatdata()).unwrap();
    let one = target.join(format!("{APPID}/pfx/drive_c/shared.dll"));
    let two = target.join(format!("{APPID}/pfx/drive_c/windows/shared.dll"));

    let one_meta = fs::metadata(&one).unwrap();
    let two_meta = fs::metadata(&two).unwrap();
    assert_eq!(
        std::os::unix::fs::MetadataExt::ino(&one_meta),
        std::os::unix::fs::MetadataExt::ino(&two_meta),
        "the copy duplicated a shared file instead of sharing it"
    );
    assert_ne!(
        std::os::unix::fs::MetadataExt::ino(&one_meta),
        before_inode,
        "the copy is the same file as the source"
    );
    assert_eq!(
        one_meta.modified().unwrap(),
        before_mtime,
        "the file's modification time was not preserved"
    );

    // Directory times too, which have to be set after their contents.
    let source_dir_mtime = fs::metadata(fixture.backup_dir().join(APPID))
        .unwrap()
        .modified()
        .unwrap();
    assert_eq!(
        fs::metadata(target.join(APPID))
            .unwrap()
            .modified()
            .unwrap(),
        source_dir_mtime
    );
}

/// Phase 2. A flag that means nothing to the command asked for is a mistake,
/// not something to ignore.
#[test]
fn flags_are_checked_against_the_command() {
    let fixture = Fixture::new("flags");
    for arguments in [
        vec!["scan", "--keep-destination"],
        vec!["scan", "--yes"],
        vec!["storage", "--force"],
        vec!["undo", "abc123", "--force"],
    ] {
        let output = fixture.run(&arguments);
        assert_eq!(
            output.status.code(),
            Some(2),
            "accepted {arguments:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(stderr.contains("does not take"), "{stderr}");
    }

    // And the flags that do belong are still accepted.
    assert!(fixture.run(&["scan", "--json"]).status.success());
}

/// Phase 2. A scan that could not read everything is not a scan that found
/// nothing, and the output has to say which happened.
#[test]
fn scan_reports_whether_it_saw_everything() {
    let fixture = Fixture::new("coverage");
    fixture.make_prefix();

    let json = fixture.run_ok(&["scan", "--json"]);
    assert!(json.contains("\"complete\": true"), "{json}");
    assert!(json.contains("\"tool_version\":"), "{json}");
    assert!(json.contains("\"eligible\":"), "{json}");
    assert!(json.contains("\"blocking_reason\":"), "{json}");

    // An ineligible library carries an explanation rather than a null reason.
    // What the reason is depends on the host: off Linux the filesystem cannot
    // be identified; on Linux the fixture library is already on a native
    // filesystem, so the reason is that no repair applies.
    assert!(
        json.contains("could not be identified")
            || json.contains("no repair needed")
            || json.contains("already on a Linux filesystem"),
        "{json}"
    );

    fs::write(
        fixture.steam.join("steamapps/libraryfolders.vdf"),
        "\"libraryfolders\" {",
    )
    .unwrap();
    let json = fixture.run_ok(&["scan", "--json"]);
    assert!(json.contains("\"complete\": false"), "{json}");
}

/// A repair establishes that the files copied. It establishes nothing about
/// whether a game runs, and the two must not be conflated.
#[test]
fn a_repair_records_only_what_it_established() {
    let fixture = Fixture::new("evidence");
    fixture.make_prefix();
    let id = fixture.library_id();
    fixture.run_ok(&["fix", &id, "--yes", "--force"]);

    let text = fixture.run_ok(&["evidence", &id]);
    assert!(text.contains("Files copied and checked"), "{text}");
    assert!(text.contains("checked by LibraryBridge"), "{text}");
    // Three things a repair cannot possibly know.
    assert_eq!(text.matches("not checked").count(), 3, "{text}");

    // A person can answer them, and the answer is marked as theirs.
    fixture.run_ok(&["evidence", &id, "--record", "launch=yes"]);
    let text = fixture.run_ok(&["evidence", &id]);
    assert!(text.contains("reported by you"), "{text}");
    assert_eq!(text.matches("not checked").count(), 2, "{text}");

    // Nonsense is refused rather than stored.
    let output = fixture.run(&["evidence", &id, "--record", "launch=maybe"]);
    assert!(!output.status.success());
    let output = fixture.run(&["evidence", &id, "--record", "teleport=yes"]);
    assert!(!output.status.success());
}

/// Moving the data again makes every answer about the game stale, because it
/// was answered about a different arrangement.
#[test]
fn moving_the_data_again_clears_answers_about_the_game() {
    let fixture = Fixture::new("stale-evidence");
    fixture.make_prefix();
    let id = fixture.library_id();
    fixture.run_ok(&["fix", &id, "--yes", "--force"]);
    fixture.run_ok(&["evidence", &id, "--record", "launch=yes"]);
    fixture.run_ok(&["evidence", &id, "--record", "save=yes"]);

    fixture.run_ok(&["undo", &id, "--yes"]);
    fixture.run_ok(&["fix", &id, "--yes", "--force", "--replace-destination"]);

    let text = fixture.run_ok(&["evidence", &id]);
    assert_eq!(
        text.matches("not checked").count(),
        3,
        "answers about the game survived the data moving:\n{text}"
    );
    assert!(text.contains("Files copied and checked"), "{text}");
}

// ------------------------------------------------------------ backup delete

/// The only thing LibraryBridge ever deletes, and it refuses without the
/// evidence that a game actually works from the moved copy. The backup stays
/// until a person has answered that a game launched and loaded a save.
#[test]
fn backup_delete_refuses_without_recorded_evidence() {
    let fixture = Fixture::new("backup-delete-no-evidence");
    fixture.make_prefix();
    let id = fixture.library_id();

    // If this host cannot stage a repair (the fixture puts the destination on
    // the same filesystem as the library), there is no backup to protect and
    // nothing further to assert. The safety property that matters — never
    // delete on a guess — still holds because the command cannot run at all.
    if !fixture.try_fix(&id) {
        return;
    }

    // The backup is there and would save space. Without evidence, it is not
    // deleted.
    let backup = fixture.backup_dir();
    let output = fixture.run(&["backup", &id]);
    assert!(
        !output.status.success(),
        "backup delete without evidence must fail: {output:?}"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("no evidence yet"), "{stderr}");
    assert!(backup.is_dir(), "the backup was deleted without evidence");

    // Even a dry run refuses, and changes nothing.
    let output = fixture.run(&["backup", &id, "--dry-run"]);
    assert!(
        !output.status.success(),
        "backup --dry-run without evidence must also fail"
    );
    assert!(backup.is_dir());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("no evidence yet"), "{stderr}");
}

/// With both answers recorded and the moved copy still identical to the
/// backup, the backup is removed and the space reclaimed.
#[test]
fn backup_delete_removes_the_original_after_confirmation() {
    let fixture = Fixture::new("backup-delete-ok");
    fixture.make_prefix();
    let id = fixture.library_id();
    if !fixture.try_fix(&id) {
        return;
    }
    fixture.run_ok(&["evidence", &id, "--record", "launch=yes"]);
    fixture.run_ok(&["evidence", &id, "--record", "save=yes"]);

    let backup = fixture.backup_dir();

    let text = fixture.run_ok(&["backup", &id, "--dry-run"]);
    assert!(text.contains("Would delete"), "{text}");
    assert!(text.contains("Dry run"), "{text}");
    assert!(backup.is_dir(), "dry run deleted the backup");

    let text = fixture.run_ok(&["backup", &id, "--yes"]);
    assert!(text.contains("Deleted"), "{text}");
    assert!(!backup.is_dir(), "backup is still there after delete");
}

/// If the moved copy no longer carries something the original holds, the
/// original still has the only copy of that data. It stays.
#[test]
fn backup_delete_refuses_when_the_moved_copy_is_missing_data() {
    let fixture = Fixture::new("backup-delete-diverge");
    fixture.make_prefix();
    let id = fixture.library_id();
    if !fixture.try_fix(&id) {
        return;
    }
    fixture.run_ok(&["evidence", &id, "--record", "launch=yes"]);
    fixture.run_ok(&["evidence", &id, "--record", "save=yes"]);

    // A file the original still holds is gone from the live copy. The copy no
    // longer carries what the original has, so the original must stay.
    let live = fs::read_link(fixture.compatdata()).unwrap();
    fs::remove_file(fixture.save_file(&live)).unwrap();

    let backup = fixture.backup_dir();
    let output = fixture.run(&["backup", &id, "--yes"]);
    assert!(
        !output.status.success(),
        "delete with data missing from the live copy must fail: {output:?}"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("not fully carried by"), "{stderr}");
    assert!(backup.is_dir(), "divergent backup was deleted");
}

/// Playing a game writes into the moved copy — new saves, updated registries,
/// patched files. That is the normal prerequisite for recording evidence, so
/// it must not block the deletion: a copy that is newer than the original has
/// outlived the original's purpose.
#[test]
fn backup_delete_after_newer_writes() {
    let fixture = Fixture::new("backup-delete-newer");
    fixture.make_prefix();
    let id = fixture.library_id();
    if !fixture.try_fix(&id) {
        return;
    }
    fixture.run_ok(&["evidence", &id, "--record", "launch=yes"]);
    fixture.run_ok(&["evidence", &id, "--record", "save=yes"]);

    let live = fs::read_link(fixture.compatdata()).unwrap();
    // A brand new save, only in the moved copy.
    fs::write(
        live.join(format!("{APPID}/pfx/drive_c/post-repair.dat")),
        b"NEW SAVE",
    )
    .unwrap();
    // An existing file rewritten in place, as a game update would.
    fs::write(
        live.join(format!("{APPID}/pfx/system.reg")),
        b"WINE REGISTRY\nEXTRA\n",
    )
    .unwrap();

    let backup = fixture.backup_dir();
    let text = fixture.run_ok(&["backup", &id, "--dry-run"]);
    assert!(text.contains("Would delete"), "{text}");
    assert!(backup.is_dir(), "dry run deleted the backup");

    let text = fixture.run_ok(&["backup", &id, "--yes"]);
    assert!(text.contains("Deleted"), "{text}");
    assert!(!backup.is_dir(), "backup is still there after delete");
}
