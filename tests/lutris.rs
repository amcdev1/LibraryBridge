//! Tests for the Lutris side: detection, discovery of games in folders, and
//! plan generation. Nothing here talks to a real Lutris.

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const BIN: &str = env!("CARGO_BIN_EXE_librarybridge");

struct Fixture {
    root: PathBuf,
    games: PathBuf,
    home: PathBuf,
}

impl Fixture {
    fn new(name: &str) -> Fixture {
        let root = std::env::temp_dir().join(format!(
            "librarybridge-lutris-{}-{name}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        let root = root.canonicalize().unwrap();
        let games = root.join("GameDrive");
        let home = root.join("home");
        fs::create_dir_all(&games).unwrap();
        fs::create_dir_all(&home).unwrap();
        Fixture { root, games, home }
    }

    fn exe(&self, relative: &str, megabytes: usize) {
        let path = self.games.join(relative);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, vec![0u8; megabytes * 1024 * 1024 + 1024]).unwrap();
    }

    fn file(&self, relative: &str, contents: &str) {
        let path = self.games.join(relative);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, contents).unwrap();
    }

    fn script(&self, relative: &str) {
        let path = self.games.join(relative);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, "#!/bin/sh\necho hi\n").unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
    }

    fn run(&self, args: &[&str]) -> Output {
        Command::new(BIN)
            .args(args)
            .env("HOME", &self.home)
            .env("XDG_CONFIG_HOME", self.home.join(".config"))
            .env("XDG_DATA_HOME", self.home.join(".local/share"))
            // Keep df and ps available, but not lutris.
            .env("PATH", "/usr/bin:/bin")
            .output()
            .expect("failed to run librarybridge")
    }

    fn scan(&self) -> String {
        let output = self.run(&["lutris", "scan", "--root", self.games.to_str().unwrap()]);
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8_lossy(&output.stdout).to_string()
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

/// Pull the block of scan output describing one game.
fn block<'a>(text: &'a str, name: &str) -> &'a str {
    let start = text
        .find(&format!("] {name}\n"))
        .unwrap_or_else(|| panic!("{name} not in:\n{text}"));
    let rest = &text[start..];
    match rest[1..].find("\n\n") {
        Some(end) => &rest[..end + 1],
        None => rest,
    }
}

#[test]
fn detect_says_so_when_lutris_is_missing() {
    let fixture = Fixture::new("detect");
    let output = fixture.run(&["lutris", "detect"]);
    assert_eq!(output.status.code(), Some(2));
    let text = String::from_utf8_lossy(&output.stdout);
    assert!(text.contains("Lutris was not found"), "{text}");
    assert!(text.contains("net.lutris.Lutris"), "{text}");
}

#[test]
fn a_gog_folder_is_read_from_its_metadata() {
    let fixture = Fixture::new("gog");
    fixture.file(
        "Beneath a Steel Sky/goggame-1207658691.info",
        r#"{"name": "Beneath a Steel Sky", "playTasks": [
             {"isPrimary": true, "path": "BASS.exe", "type": "FileTask"},
             {"path": "manual.pdf", "type": "FileTask"}]}"#,
    );
    fixture.exe("Beneath a Steel Sky/BASS.exe", 1);

    let text = fixture.scan();
    let entry = block(&text, "Beneath a Steel Sky");
    assert!(entry.contains("confidence  high"), "{entry}");
    assert!(entry.contains("BASS.exe"), "{entry}");
    assert!(entry.contains("GOG metadata"), "{entry}");
}

#[test]
fn installers_and_helpers_are_never_the_game() {
    let fixture = Fixture::new("helpers");
    fixture.exe("Deep Rock Adventure/DeepRockAdventure.exe", 4);
    fixture.exe("Deep Rock Adventure/unins000.exe", 2);
    fixture.exe("Deep Rock Adventure/vcredist_x64.exe", 8);
    fixture.exe("Deep Rock Adventure/_CommonRedist/DirectX/dxsetup.exe", 12);
    fixture.exe("Deep Rock Adventure/CrashReporter.exe", 1);

    let text = fixture.scan();
    let entry = block(&text, "Deep Rock Adventure");
    assert!(entry.contains("DeepRockAdventure.exe"), "{entry}");
    assert!(!entry.contains("unins000"), "{entry}");
    assert!(!entry.contains("vcredist"), "{entry}");
    assert!(!entry.contains("dxsetup"), "{entry}");
    assert!(
        entry.contains("its name matches the folder name"),
        "{entry}"
    );
}

#[test]
fn an_unreal_shipping_build_is_recognised() {
    let fixture = Fixture::new("unreal");
    fixture.exe(
        "Silent Harbour/SilentHarbour/Binaries/Win64/SilentHarbour-Win64-Shipping.exe",
        30,
    );
    fixture.exe("Silent Harbour/SilentHarbour.exe", 1);
    fixture.exe(
        "Silent Harbour/Engine/Binaries/ThirdParty/CrashReportClient.exe",
        2,
    );

    let text = fixture.scan();
    let entry = block(&text, "Silent Harbour");
    assert!(
        entry.contains("Shipping.exe") || entry.contains("SilentHarbour.exe"),
        "{entry}"
    );
    assert!(!entry.contains("CrashReportClient"), "{entry}");
}

#[test]
fn an_ambiguous_folder_is_low_confidence_and_offers_alternatives() {
    let fixture = Fixture::new("ambiguous");
    fixture.exe("Bundle/alpha.exe", 5);
    fixture.exe("Bundle/beta.exe", 5);
    fixture.exe("Bundle/gamma.exe", 5);

    let text = fixture.scan();
    let entry = block(&text, "Bundle");
    assert!(entry.contains("confidence  low"), "{entry}");
    assert!(entry.contains("or maybe"), "{entry}");
    assert!(entry.contains("is a guess"), "{entry}");
}

#[test]
fn an_existing_wine_prefix_is_reused_not_replaced() {
    let fixture = Fixture::new("prefix");
    fixture.file("Riverside/system.reg", "WINE REGISTRY Version 2\n");
    fixture.file("Riverside/drive_c/.keep", "");
    fixture.exe("Riverside/drive_c/Games/Riverside/Riverside.exe", 3);

    let text = fixture.scan();
    assert!(text.contains("prefix      "), "{text}");
    assert!(text.contains("Riverside"), "{text}");
}

#[test]
fn a_native_linux_game_uses_the_linux_runner() {
    let fixture = Fixture::new("linux");
    fixture.script("Cave Story Clone/start.sh");
    fixture.file("Cave Story Clone/data.pak", "assets");

    let text = fixture.scan();
    let entry = block(&text, "Cave Story Clone");
    assert!(entry.contains("linux (linux source)"), "{entry}");
    assert!(entry.contains("start.sh"), "{entry}");
}

#[test]
fn symlinked_directories_are_not_followed_while_scanning() {
    let fixture = Fixture::new("symlink");
    fixture.exe("Real Game/RealGame.exe", 2);
    std::os::unix::fs::symlink("/", fixture.games.join("Real Game/everything")).unwrap();

    let text = fixture.scan();
    assert!(text.contains("Real Game"), "{text}");
    assert!(!text.contains("/usr/bin"), "{text}");
}

#[test]
fn scan_json_is_machine_readable() {
    let fixture = Fixture::new("json");
    fixture.exe("Tiny Game/TinyGame.exe", 1);

    let output = fixture.run(&[
        "lutris",
        "scan",
        "--root",
        fixture.games.to_str().unwrap(),
        "--json",
    ]);
    let text = String::from_utf8_lossy(&output.stdout);
    assert!(text.contains("\"schema\": 1"), "{text}");
    assert!(text.contains("\"name\": \"Tiny Game\""), "{text}");
    assert!(text.contains("\"runner\": \"wine\""), "{text}");
    assert!(text.contains("\"in_lutris\": false"), "{text}");
}

#[test]
fn plan_writes_only_the_chosen_games() {
    let fixture = Fixture::new("plan");
    fixture.exe("Chosen One/ChosenOne.exe", 3);
    fixture.exe("Left Behind/LeftBehind.exe", 3);

    let text = fixture.scan();
    let start = text.find("[").unwrap() + 1;
    let id = &text[start..start + 8];

    let plan_path = fixture.root.join("plan.json");
    let output = fixture.run(&[
        "lutris",
        "plan",
        "--root",
        fixture.games.to_str().unwrap(),
        "--candidate",
        id,
        "--output",
        plan_path.to_str().unwrap(),
    ]);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );

    let written = fs::read_to_string(&plan_path).unwrap();
    assert!(written.contains("\"schema\": 1"), "{written}");
    let chosen = written.matches("\"runner\"").count();
    assert_eq!(chosen, 1, "expected one game in the plan:\n{written}");
}

#[test]
fn import_refuses_without_lutris() {
    let fixture = Fixture::new("import");
    let plan_path = fixture.root.join("plan.json");
    fs::write(
        &plan_path,
        r#"{"schema": 1, "games": [{"name": "X", "runner": "wine", "exe": "/nope/x.exe"}]}"#,
    )
    .unwrap();

    let output = fixture.run(&["lutris", "import", "--plan", plan_path.to_str().unwrap()]);
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("Lutris was not found"), "{stderr}");
}

#[test]
fn forget_refuses_entries_it_did_not_create() {
    let fixture = Fixture::new("forget");
    let output = fixture.run(&["lutris", "forget", "--entry", "some-other-game"]);
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("no record"), "{stderr}");
}

#[test]
fn a_game_already_in_lutris_is_marked_and_hidden() {
    let fixture = Fixture::new("dedupe");
    fixture.exe("Known Game/KnownGame.exe", 3);

    // A Lutris config pointing at the same executable.
    let games_dir = fixture.home.join(".config/lutris/games");
    fs::create_dir_all(&games_dir).unwrap();
    fs::write(
        games_dir.join("known-game-1699999999.yml"),
        format!(
            "game:\n  exe: {}\n  prefix: /home/a/prefix\nrunner: wine\n",
            fixture.games.join("Known Game/KnownGame.exe").display()
        ),
    )
    .unwrap();

    let text = fixture.scan();
    assert!(
        text.contains("Nothing found that Lutris does not already have"),
        "{text}"
    );

    let output = fixture.run(&[
        "lutris",
        "scan",
        "--root",
        fixture.games.to_str().unwrap(),
        "--all",
    ]);
    let all = String::from_utf8_lossy(&output.stdout);
    assert!(all.contains("Lutris already has this one"), "{all}");
}

#[test]
fn steam_games_are_reported_as_duplicating_lutris_own_source() {
    let fixture = Fixture::new("steamnote");
    let steam = fixture.root.join("Steam");
    fs::create_dir_all(steam.join("steamapps")).unwrap();
    fs::write(
        steam.join("steamapps/libraryfolders.vdf"),
        format!(
            "\"libraryfolders\"\n{{\n\t\"0\"\n\t{{\n\t\t\"path\"\t\t\"{}\"\n\t}}\n}}\n",
            steam.display()
        ),
    )
    .unwrap();
    fs::write(
        steam.join("steamapps/appmanifest_220.acf"),
        "\"AppState\"\n{\n\t\"appid\"\t\t\"220\"\n\t\"name\"\t\t\"Half-Life 2\"\n}\n",
    )
    .unwrap();

    let output = fixture.run(&["--steam-root", steam.to_str().unwrap(), "lutris", "scan"]);
    let text = String::from_utf8_lossy(&output.stdout);
    assert!(text.contains("Half-Life 2"), "{text}");
    assert!(text.contains("steam app   220"), "{text}");
    assert!(text.contains("its own Steam source"), "{text}");
}

#[test]
fn nothing_outside_the_selected_roots_is_scanned() {
    let fixture = Fixture::new("scope");
    fixture.exe("Inside/Inside.exe", 2);
    let outside = fixture.root.join("Outside");
    fs::create_dir_all(&outside).unwrap();
    fs::write(outside.join("Outside.exe"), vec![0u8; 2048]).unwrap();

    let text = fixture.scan();
    assert!(text.contains("Inside"), "{text}");
    assert!(!text.contains("Outside"), "{text}");
}

#[test]
fn scanning_with_no_roots_touches_nothing_but_steam() {
    let fixture = Fixture::new("noroots");
    let output = fixture.run(&["lutris", "scan"]);
    assert!(output.status.success());
    let text = String::from_utf8_lossy(&output.stdout);
    assert!(text.contains("No folders were given"), "{text}");
}

#[test]
fn discovered_files_are_never_executed() {
    let fixture = Fixture::new("noexec");
    let marker = fixture.root.join("SHOULD-NOT-EXIST");
    let script = fixture.games.join("Trap/Trap.exe");
    fs::create_dir_all(script.parent().unwrap()).unwrap();
    fs::write(&script, format!("#!/bin/sh\ntouch {}\n", marker.display())).unwrap();
    fs::set_permissions(&script, fs::Permissions::from_mode(0o755)).unwrap();

    let _ = fixture.scan();
    assert!(
        !Path::new(&marker).exists(),
        "a discovered file was executed"
    );
}

// ---- regressions for the findings raised in the release review ----------

/// R16. `forget` names a record, it does not take a path. A path used to
/// delete whatever JSON file it pointed at.
#[test]
fn forget_refuses_anything_that_is_not_a_record_name() {
    let fixture = Fixture::new("forgetpath");
    let unrelated = fixture.root.join("unrelated.json");
    fs::write(&unrelated, r#"{"personal":"data"}"#).unwrap();

    let target = unrelated.with_extension("");
    for argument in [
        target.to_str().unwrap(),
        "../../../etc/passwd",
        "..",
        ".hidden",
        "has/slash",
        "",
    ] {
        let output = fixture.run(&["lutris", "forget", "--entry", argument]);
        assert!(!output.status.success(), "accepted {argument:?}");
    }
    assert!(unrelated.is_file(), "an unrelated file was deleted");
    assert_eq!(
        fs::read_to_string(&unrelated).unwrap(),
        r#"{"personal":"data"}"#
    );
}

/// R16. A file with the right name in the right place still has to look like
/// one of ours before it is removed.
#[test]
fn forget_refuses_a_file_that_is_not_one_of_our_records() {
    let fixture = Fixture::new("forgetalien");
    let imports = fixture
        .home
        .join(".local/share/librarybridge/lutris/imports");
    fs::create_dir_all(&imports).unwrap();
    let planted = imports.join("something.json");
    fs::write(&planted, r#"{"unrelated":true}"#).unwrap();

    let output = fixture.run(&["lutris", "forget", "--entry", "something"]);
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("not a LibraryBridge import record"),
        "{stderr}"
    );
    assert!(planted.is_file());
}

/// R17. A dry run must validate and stop: no definition written, no record
/// kept, and Lutris never started.
#[test]
fn lutris_import_honours_dry_run() {
    let fixture = Fixture::new("importdry");
    fixture.exe("Some Game/SomeGame.exe", 2);

    // Enough for Lutris to count as present without a binary to run.
    fs::create_dir_all(fixture.home.join(".config/lutris/games")).unwrap();

    let plan = fixture.root.join("plan.json");
    fs::write(
        &plan,
        format!(
            r#"{{"schema": 1, "games": [{{"name": "Some Game", "runner": "wine", "exe": "{}"}}]}}"#,
            fixture.games.join("Some Game/SomeGame.exe").display()
        ),
    )
    .unwrap();

    let output = fixture.run(&[
        "lutris",
        "import",
        "--plan",
        plan.to_str().unwrap(),
        "--dry-run",
    ]);
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(stdout.contains("Dry run"), "{stdout}");
    assert!(stdout.contains("Nothing was changed"), "{stdout}");
    assert!(stdout.contains("add   Some Game"), "{stdout}");

    let state = fixture.home.join(".local/share/librarybridge/lutris");
    assert!(
        !state.join("definitions").exists(),
        "a definition was written"
    );
    assert!(!state.join("imports").exists(), "a record was written");
}

/// R19. The recursive walker refuses symlinks, but the root's own children
/// were checked with a call that follows them, so a link could take the scan
/// out of the folder the user chose.
#[test]
fn scanning_does_not_follow_a_symlink_child_of_the_root() {
    let fixture = Fixture::new("rootlink");
    let outside = fixture.root.join("private-game");
    fs::create_dir_all(&outside).unwrap();
    fs::write(outside.join("RealGame.exe"), vec![0u8; 3 * 1024 * 1024]).unwrap();
    std::os::unix::fs::symlink(&outside, fixture.games.join("LinkedGame")).unwrap();

    // Something real inside the root, so the scan has a reason to report.
    fixture.exe("Inside Game/InsideGame.exe", 2);

    let text = fixture.scan();
    assert!(text.contains("Inside Game"), "{text}");
    assert!(
        !text.contains("RealGame.exe"),
        "the scan left the chosen folder:\n{text}"
    );
    assert!(
        !text.contains("LinkedGame"),
        "the scan followed a link out of the folder:\n{text}"
    );
}

/// R26. Detection and eligibility are different questions. A Steam game is
/// worth listing and is not worth importing, and every surface has to agree.
#[test]
fn steam_candidates_are_listed_but_cannot_be_planned() {
    let fixture = Fixture::new("steampolicy");
    let steam = fixture.root.join("Steam");
    fs::create_dir_all(steam.join("steamapps")).unwrap();
    fs::write(
        steam.join("steamapps/libraryfolders.vdf"),
        format!(
            "\"libraryfolders\"\n{{\n\t\"0\"\n\t{{\n\t\t\"path\"\t\t\"{}\"\n\t}}\n}}\n",
            steam.display()
        ),
    )
    .unwrap();
    fs::write(
        steam.join("steamapps/appmanifest_220.acf"),
        "\"AppState\"\n{\n\t\"appid\"\t\t\"220\"\n\t\"name\"\t\t\"Half-Life 2\"\n}\n",
    )
    .unwrap();

    let output = fixture.run(&[
        "--steam-root",
        steam.to_str().unwrap(),
        "lutris",
        "scan",
        "--json",
    ]);
    let json = String::from_utf8_lossy(&output.stdout);
    assert!(json.contains("\"name\": \"Half-Life 2\""), "{json}");
    assert!(json.contains("\"eligible\": false"), "{json}");
    assert!(json.contains("own Steam source"), "{json}");

    let id = json
        .split("\"id\": \"")
        .nth(1)
        .and_then(|rest| rest.get(..8))
        .expect("an id")
        .to_string();

    let plan = fixture.root.join("plan.json");
    let output = fixture.run(&[
        "--steam-root",
        steam.to_str().unwrap(),
        "lutris",
        "plan",
        "--candidate",
        &id,
        "--output",
        plan.to_str().unwrap(),
    ]);
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("cannot be imported"), "{stderr}");
    assert!(!plan.exists());
}

/// R26. A hand-written plan does not get around the same rule.
#[test]
fn a_hand_written_steam_row_is_refused_at_import() {
    let fixture = Fixture::new("handsteam");
    fs::create_dir_all(fixture.home.join(".config/lutris/games")).unwrap();
    let plan = fixture.root.join("plan.json");
    fs::write(
        &plan,
        r#"{"schema": 1, "games": [{"name": "Half-Life 2", "runner": "steam", "appid": "220"}]}"#,
    )
    .unwrap();

    let output = fixture.run(&[
        "lutris",
        "import",
        "--plan",
        plan.to_str().unwrap(),
        "--dry-run",
    ]);
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("skip  Half-Life 2"), "{stdout}");
    assert!(stdout.contains("duplicate"), "{stdout}");
}

/// R26. The window needs the runner-up executables and the prefix warning,
/// which the JSON used to drop.
#[test]
fn candidate_json_carries_alternatives_and_warnings() {
    let fixture = Fixture::new("altjson");
    fixture.exe("Bundle/alpha.exe", 3);
    fixture.exe("Bundle/beta.exe", 3);
    fixture.exe("Bundle/gamma.exe", 3);

    let output = fixture.run(&[
        "lutris",
        "scan",
        "--root",
        fixture.games.to_str().unwrap(),
        "--json",
    ]);
    let json = String::from_utf8_lossy(&output.stdout);
    assert!(json.contains("\"alternatives\": ["), "{json}");
    assert!(
        json.contains("alpha.exe") || json.contains("beta.exe"),
        "{json}"
    );
    assert!(json.contains("\"filesystem_warning\":"), "{json}");
    assert!(json.contains("\"eligible\": true"), "{json}");
}

/// R17 again, for the other subcommand that writes a file: a dry run must not
/// produce the plan, only describe it.
#[test]
fn lutris_plan_honours_dry_run() {
    let fixture = Fixture::new("plandry");
    fixture.exe("Some Game/SomeGame.exe", 2);

    let text = fixture.scan();
    let start = text.find('[').unwrap() + 1;
    let id = &text[start..start + 8];

    let output_path = fixture.root.join("should-not-exist.json");
    let output = fixture.run(&[
        "lutris",
        "plan",
        "--root",
        fixture.games.to_str().unwrap(),
        "--candidate",
        id,
        "--output",
        output_path.to_str().unwrap(),
        "--dry-run",
    ]);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("Dry run"), "{stdout}");
    assert!(stdout.contains("Nothing was written"), "{stdout}");
    assert!(!output_path.exists(), "the dry run wrote its plan file");
}

/// The scan reports what it could not read, and that has to reach every
/// surface. An empty list and an unreadable one must not look the same.
#[test]
fn scan_json_reports_what_it_could_not_read() {
    let fixture = Fixture::new("warnings");
    let steam = fixture.root.join("Steam");
    fs::create_dir_all(steam.join("steamapps")).unwrap();
    // Metadata that cannot be parsed.
    fs::write(
        steam.join("steamapps/libraryfolders.vdf"),
        "\"libraryfolders\" {",
    )
    .unwrap();

    let output = fixture.run(&["--steam-root", steam.to_str().unwrap(), "scan", "--json"]);
    let json = String::from_utf8_lossy(&output.stdout);
    assert!(json.contains("\"warnings\": ["), "{json}");
    let warnings = json
        .split("\"warnings\": [")
        .nth(1)
        .and_then(|rest| rest.split(']').next())
        .unwrap_or("");
    assert!(
        warnings.contains("libraryfolders.vdf"),
        "a broken vdf produced no warning: {json}"
    );
}
