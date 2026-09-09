//! The three commands: scan, fix, undo.

use std::fs;
use std::io::Write;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

use crate::fsops::{self, human_bytes};
use crate::lock;
use crate::record;
use crate::safefs;
use crate::sha256::{hex, Sha256};
use crate::state::{self, State};
use crate::steam::{self, Library};
use crate::system::{self, Capability};

/// Room left for the prefix to grow after the move, over and above the bytes
/// being copied. A policy choice, not a Proton requirement.
const GROWTH_RESERVE: u64 = 2 * 1024 * 1024 * 1024;

pub struct Options {
    pub steam_root: Option<PathBuf>,
    pub json: bool,
    pub dry_run: bool,
    pub force: bool,
    pub assume_yes: bool,
    /// Folders the user explicitly chose to scan. Nothing is scanned without
    /// being named here or being a known Steam library.
    pub roots: Vec<PathBuf>,
    pub candidates: Vec<String>,
    pub output: Option<PathBuf>,
    pub plan: Option<PathBuf>,
    pub entry: Option<String>,
    pub all: bool,
    /// Resolve a destination conflict by keeping what is already at the
    /// destination and setting the library's current compatdata aside.
    /// The plan identity the caller reviewed. If it no longer matches, the
    /// operation is refused rather than applied to something else.
    pub expect: Option<String>,
    pub keep_destination: bool,
    /// Resolve it the other way: set the destination aside and copy the
    /// library's current compatdata over.
    pub replace_destination: bool,
}

// ---------------------------------------------------------------- scan

pub fn scan(options: &Options) -> Result<i32, String> {
    let (libraries, warnings) = steam::all_libraries(options.steam_root.as_deref());

    if options.json {
        print!("{}", scan_json(&libraries, &warnings));
        return Ok(0);
    }

    if libraries.is_empty() {
        println!("No Steam libraries found.");
        println!();
        println!("Looked for Steam in the usual places under your home directory.");
        println!("If Steam is installed somewhere else, point at it directly:");
        println!("    librarybridge --steam-root /path/to/Steam scan");
        for warning in &warnings {
            println!("  note: {warning}");
        }
        return Ok(0);
    }

    let installs = steam::find_installs(options.steam_root.as_deref());
    println!("Steam installations");
    for install in &installs {
        println!("  {:<8} {}", install.kind.label(), install.root.display());
    }
    println!();

    println!("Libraries");
    let mut actionable = 0;
    for library in &libraries {
        let report = state::inspect(library);
        let mount = system::mount_for(&library.path);

        println!();
        println!("  [{}] {}", library.id, library.path.display());
        println!("      name        {}", library.display_name());

        match &mount {
            Some(mount) => {
                let note = match mount.capability {
                    Capability::NeedsRepair => "Proton data does not belong here",
                    Capability::NoSymlinks => "no symlink support, cannot be repaired",
                    Capability::Native => "fine for Proton",
                    Capability::Unknown => "not recognised",
                };
                println!("      filesystem  {} ({note})", mount.describe());
            }
            None => println!("      filesystem  unknown (no mount table on this system)"),
        }

        match &report.state {
            State::NotRepaired { prefixes } | State::Repaired { prefixes, .. } => {
                println!("      compatdata  {prefixes} prefixes");
            }
            _ => {}
        }
        if library.connected {
            let names = steam::app_names(library);
            let mut titles: Vec<&str> = names.values().map(String::as_str).collect();
            titles.sort_unstable();
            if !titles.is_empty() {
                let shown = titles
                    .iter()
                    .take(3)
                    .copied()
                    .collect::<Vec<_>>()
                    .join(", ");
                if titles.len() > 3 {
                    println!("      games       {shown}, and {} more", titles.len() - 3);
                } else {
                    println!("      games       {shown}");
                }
            }
        }
        if let State::Repaired { target, .. } = &report.state {
            println!("      moved to    {}", target.display());
        }
        if let State::LinkedElsewhere { target } | State::DanglingLink { target } = &report.state {
            println!("      links to    {}", target.display());
        }

        println!("      state       {}", report.state.headline());
        for line in advice(library, &report, mount.as_ref()) {
            println!("      {line}");
        }
        for backup in &report.backups {
            println!("      backup      {}", backup.display());
        }
        if let Some(destination) = &report.existing_destination {
            println!(
                "      note        the destination already holds data: {}",
                destination.display()
            );
        }
        if let Some(restore) = &report.interrupted_restore {
            println!(
                "      note        a copy back to this drive stopped part way: {}",
                restore.display()
            );
            println!("                  the repair is untouched; that folder is not in use");
        }
        if let Some(abandoned) = &report.abandoned_copy {
            println!(
                "      leftover    {} (unfinished copy, safe to delete)",
                abandoned.display()
            );
        }

        if matches!(
            report.state,
            State::NotRepaired { .. } | State::InterruptedAwaitingLink { .. }
        ) {
            actionable += 1;
        }
    }

    if !warnings.is_empty() {
        println!();
        for warning in &warnings {
            println!("  note: {warning}");
        }
    }

    println!();
    if actionable == 0 {
        println!("Nothing to do.");
    } else if actionable == 1 {
        println!("1 library can be repaired. Run `librarybridge fix <id>`.");
    } else {
        println!("{actionable} libraries can be repaired. Run `librarybridge fix <id>` for each.");
    }
    Ok(0)
}

fn advice(library: &Library, report: &state::Report, mount: Option<&system::Mount>) -> Vec<String> {
    let capability = mount
        .map(|m| m.capability.clone())
        .unwrap_or(Capability::Unknown);
    match (&report.state, capability) {
        (State::NotRepaired { .. }, Capability::NoSymlinks) => vec![format!(
            "            This filesystem has no symlinks, so the repair cannot work. \
             Move the library to a Linux filesystem with Steam itself."
        )],
        (State::NotRepaired { .. }, Capability::Native) => {
            vec!["            Already on a Linux filesystem. No repair needed.".to_string()]
        }
        (State::NotRepaired { .. }, _) => {
            vec![format!(
                "            run:  librarybridge fix {}",
                library.id
            )]
        }
        (State::InterruptedAwaitingLink { backup }, _) => vec![
            format!("            An earlier run stopped after moving the original to"),
            format!("            {}", backup.display()),
            format!(
                "            Nothing was lost. Run:  librarybridge fix {}",
                library.id
            ),
        ],
        (State::DanglingLink { .. }, _) => vec![
            "            The destination is missing. If it is on another drive,".to_string(),
            "            connect it. LibraryBridge will not change anything meanwhile.".to_string(),
        ],
        (State::LinkedElsewhere { .. }, _) => vec![
            "            This link was not made by LibraryBridge, so it is left alone.".to_string(),
        ],
        (State::Repaired { .. }, _) => {
            vec![format!(
                "            to reverse:  librarybridge undo {}",
                library.id
            )]
        }
        (State::Unusable { detail }, _) => vec![format!("            {detail}")],
        _ => Vec::new(),
    }
}

fn scan_json(libraries: &[Library], warnings: &[String]) -> String {
    let mut out = String::from("{\n  \"schema\": 1,\n  \"libraries\": [\n");
    for (index, library) in libraries.iter().enumerate() {
        let report = state::inspect(library);
        let mount = system::mount_for(&library.path);
        out.push_str("    {\n");
        out.push_str(&format!("      \"id\": {},\n", json_string(&library.id)));
        out.push_str(&format!(
            "      \"path\": {},\n",
            json_string(&library.path.to_string_lossy())
        ));
        out.push_str(&format!(
            "      \"name\": {},\n",
            json_string(&library.display_name())
        ));
        out.push_str(&format!(
            "      \"steam\": {},\n",
            json_string(library.install_kind.label())
        ));
        out.push_str(&format!(
            "      \"filesystem\": {},\n",
            json_string(
                &mount
                    .as_ref()
                    .map(|m| m.fs_type.clone())
                    .unwrap_or_default()
            )
        ));
        out.push_str(&format!("      \"connected\": {},\n", library.connected));
        out.push_str(&format!(
            "      \"state\": {},\n",
            json_string(report.state.code())
        ));
        out.push_str(&format!(
            "      \"target\": {},\n",
            json_string(&library.target().to_string_lossy())
        ));
        let backups: Vec<String> = report
            .backups
            .iter()
            .map(|b| json_string(&b.to_string_lossy()))
            .collect();
        out.push_str(&format!("      \"backups\": [{}]\n", backups.join(", ")));
        out.push_str(if index + 1 == libraries.len() {
            "    }\n"
        } else {
            "    },\n"
        });
    }
    out.push_str("  ],\n  \"warnings\": [");
    let notes: Vec<String> = warnings.iter().map(|w| json_string(w)).collect();
    out.push_str(&notes.join(", "));
    out.push_str("]\n}\n");
    out
}

fn json_string(value: &str) -> String {
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

// ------------------------------------------------------------- storage

/// What is taking up space and where, so retained originals can be found and
/// understood before anyone decides to delete one.
///
/// Sizes are measured on demand rather than during every scan, because
/// walking every backup on a slow external drive is not something a listing
/// should do.
pub fn storage(options: &Options) -> Result<i32, String> {
    let (libraries, _) = steam::all_libraries(options.steam_root.as_deref());
    let mut rows = Vec::new();

    for library in &libraries {
        let report = state::inspect(library);
        let live = match &report.state {
            State::Repaired { target, .. } => Some(target.clone()),
            _ => report.existing_destination.clone(),
        };
        let live = live.filter(|path| path.is_dir()).map(|path| {
            let bytes = fsops::tree_size(&path).unwrap_or(0);
            (path, bytes)
        });
        let backups: Vec<(PathBuf, u64)> = report
            .backups
            .iter()
            .map(|path| (path.clone(), fsops::tree_size(path).unwrap_or(0)))
            .collect();
        let leftover = report
            .abandoned_copy
            .clone()
            .or_else(|| report.interrupted_restore.clone())
            .map(|path| {
                let bytes = fsops::tree_size(&path).unwrap_or(0);
                (path, bytes)
            });

        if live.is_some() || !backups.is_empty() || leftover.is_some() {
            rows.push((library, live, backups, leftover));
        }
    }

    if options.json {
        let mut out = String::from("{\n  \"schema\": 1,\n  \"libraries\": [\n");
        let entry = |path: &Path, bytes: u64| {
            format!(
                "{{\"path\": {}, \"bytes\": {bytes}}}",
                json_string(&path.to_string_lossy())
            )
        };
        let blocks: Vec<String> = rows
            .iter()
            .map(|(library, live, backups, leftover)| {
                format!(
                    "    {{\n      \"id\": {},\n      \"name\": {},\n      \"live\": {},\n      \"backups\": [{}],\n      \"leftover\": {}\n    }}",
                    json_string(&library.id),
                    json_string(&library.display_name()),
                    live.as_ref()
                        .map(|(path, bytes)| entry(path, *bytes))
                        .unwrap_or_else(|| "null".to_string()),
                    backups
                        .iter()
                        .map(|(path, bytes)| entry(path, *bytes))
                        .collect::<Vec<_>>()
                        .join(", "),
                    leftover
                        .as_ref()
                        .map(|(path, bytes)| entry(path, *bytes))
                        .unwrap_or_else(|| "null".to_string()),
                )
            })
            .collect();
        out.push_str(&blocks.join(",\n"));
        out.push_str("\n  ]\n}\n");
        print!("{out}");
        return Ok(0);
    }

    if rows.is_empty() {
        println!("Nothing stored by LibraryBridge yet.");
        return Ok(0);
    }

    for (library, live, backups, leftover) in &rows {
        println!();
        println!("  [{}] {}", library.id, library.display_name());
        if let Some((path, bytes)) = live {
            println!("      live        {} ({})", path.display(), human_bytes(*bytes));
        }
        for (path, bytes) in backups {
            println!("      original    {} ({})", path.display(), human_bytes(*bytes));
        }
        if let Some((path, bytes)) = leftover {
            println!(
                "      unfinished  {} ({}), left by a copy that stopped",
                path.display(),
                human_bytes(*bytes)
            );
        }
    }
    println!();
    println!("Originals are kept on purpose. Delete one yourself once a game has launched");
    println!("and loaded a save from the live copy. LibraryBridge will not delete them.");
    Ok(0)
}

// ---------------------------------------------------------------- fix

pub fn fix(options: &Options, reference: &str) -> Result<i32, String> {
    let (libraries, _) = steam::all_libraries(options.steam_root.as_deref());
    let library = steam::resolve(&libraries, reference)?;

    // Held until this function returns, so a second process cannot act on a
    // library while this one is deciding what to do with it. Taken before the
    // state is read, because a decision made from a stale read is the problem
    // it exists to prevent. A dry run takes none: it changes nothing, so it
    // has nothing to protect and no business creating a lock file.
    let _lock = if options.dry_run {
        None
    } else {
        Some(lock::Lock::acquire(&library.id)?)
    };

    let report = state::inspect(library);
    let target = library.effective_target();

    // Machine output carries these fields itself; printing them first would
    // put prose in front of the document.
    if !options.json {
        println!("Library     {}", library.path.display());
        println!("Name        {}", library.display_name());
    }

    preflight(library)?;

    match &report.state {
        State::Repaired { target, .. } => {
            println!(
                "State       already repaired, pointing at {}",
                target.display()
            );
            println!("Nothing to do.");
            return Ok(0);
        }
        State::InterruptedAwaitingLink { backup } => {
            return finish_interrupted(options, library, backup, &target);
        }
        State::NotRepaired { .. } | State::NoCompatdata => {}
        other => {
            return Err(format!(
                "{}: cannot repair while the library is in the state '{}'. \
                 Run `librarybridge scan` for details.",
                library.path.display(),
                other.headline()
            ))
        }
    }

    // --- preconditions -------------------------------------------------

    let source_mount = system::mount_for(&library.path);
    match source_mount.as_ref().map(|m| m.capability.clone()) {
        Some(Capability::NoSymlinks) => {
            return Err(format!(
                "{} is on {}, which has no symlink support. This repair cannot work there. \
                 Use Steam to move the library to a Linux filesystem instead.",
                library.path.display(),
                source_mount.unwrap().fs_type
            ))
        }
        Some(Capability::Native) if !options.force => {
            return Err(format!(
                "{} is already on {}, a Linux filesystem. There is nothing to repair. \
                 Pass --force to do it anyway.",
                library.path.display(),
                source_mount.unwrap().fs_type
            ))
        }
        // No mount table, or a filesystem this build does not recognise.
        // Repairing something we cannot identify is how a tool reports
        // success without establishing that it fixed anything.
        None | Some(Capability::Unknown) if !options.force => {
            return Err(format!(
                "the filesystem under {} could not be identified, so there is no way to tell \
                 whether this repair applies or would even work. This is expected off Linux, \
                 where there is no mount table to read. Pass --force to proceed anyway.",
                library.path.display()
            ))
        }
        _ => {}
    }
    let target_parent = target.parent().unwrap_or(Path::new("/")).to_path_buf();
    if let (Some(source_mount), Some(target_mount)) = (
        &source_mount,
        system::mount_for(&existing_ancestor(&target_parent)),
    ) {
        if source_mount.mount_point == target_mount.mount_point {
            return Err(format!(
                "the destination {} is on the same filesystem as the library. \
                 Moving the data there would not fix anything.",
                target.display()
            ));
        }
    }

    // A stronger form of the same question, and one that works without a
    // mount table: two names can lead to one directory.
    if safefs::same_directory(&library.steamapps, &existing_ancestor(&target_parent))? {
        return Err(format!(
            "the destination {} is the same directory as the library itself. Moving the data \
             there would not fix anything.",
            target.display()
        ));
    }

    let source_exists = matches!(report.state, State::NotRepaired { .. });
    let mut hard_linked = 0usize;
    let (source_entries, source_bytes) = if source_exists {
        // One walk, used for the size, the link check and the plan identity.
        let inventory = fsops::inventory(&library.compatdata, false)?;

        let escaping = fsops::escaping_relative_links(&library.compatdata, &inventory);
        if !escaping.is_empty() {
            let listed: Vec<String> = escaping
                .iter()
                .take(10)
                .map(|(from, to)| format!("  {} -> {}", from.display(), to.display()))
                .collect();
            return Err(format!(
                "this prefix contains relative links that point outside it, and moving the \
                 tree would change where they lead:\n{}\n\nCopying them unchanged would \
                 leave them pointing at the wrong place, and rewriting them is a separate \
                 decision this tool does not make on its own. Nothing was changed.",
                listed.join("\n")
            ));
        }
        hard_linked = inventory.hard_linked.len();
        (inventory.entries.len(), inventory.bytes)
    } else {
        (0, 0)
    };

    // What was reviewed, in one line. An apply that quotes a different one is
    // acting on something the user never saw.
    let fingerprint = plan_fingerprint(library, &report, &target, source_entries, source_bytes);

    // Something is already at the destination. If it is the same tree we are
    // about to copy, it is the leftover from an undone repair and can be set
    // aside without a word. If it differs, an interrupted repair was very
    // likely overtaken by Steam recreating compatdata, and the destination
    // holds the real prefixes. Never guess which one the user wants.
    if let Some(destination) = &report.existing_destination {
        let same = source_exists && same_data(&library.compatdata, destination)?;
        if !same && !options.keep_destination && !options.replace_destination {
            return Err(conflict_message(library, destination, source_exists)?);
        }
        if options.keep_destination {
            return keep_destination(options, library, destination);
        }
    }

    // Deliberately no directory is created yet: a dry run must leave the
    // disk exactly as it found it, so the space check asks about the nearest
    // parent that already exists.
    let available = system::free_bytes(&existing_ancestor(&target_parent));
    let needed = source_bytes + GROWTH_RESERVE;
    match available {
        Some(free) if free < needed => {
            return Err(format!(
                "not enough room at {}. The copy needs {} plus {} spare, and only {} is free.",
                target_parent.display(),
                human_bytes(source_bytes),
                human_bytes(GROWTH_RESERVE),
                human_bytes(free)
            ))
        }
        Some(_) => {}
        None => println!("Space       could not be checked on this system"),
    }

    // What the library actually holds, so the scope of the repair is visible
    // before it is approved rather than summarised as a number of bytes.
    let installed = steam::app_names(library);
    let (prefixes, unknown): (usize, Vec<String>) = if source_exists {
        let mut unknown = Vec::new();
        let mut total = 0usize;
        if let Ok(entries) = fs::read_dir(&library.compatdata) {
            for entry in entries.flatten() {
                total += 1;
                let name = entry.file_name().to_string_lossy().to_string();
                if !installed.contains_key(&name) {
                    unknown.push(name);
                }
            }
        }
        unknown.sort();
        (total, unknown)
    } else {
        (0, Vec::new())
    };
    if options.json {
        print!(
            "{}",
            plan_json(
                library,
                &report,
                &target,
                &state::new_backup_path(&library.steamapps),
                source_bytes,
                GROWTH_RESERVE,
                available,
                installed.len(),
                prefixes,
                &unknown,
                hard_linked,
                &fingerprint,
            )
        );
        return Ok(0);
    }

    println!(
        "Games       {} installed, {prefixes} prefix folders",
        installed.len()
    );
    if !unknown.is_empty() {
        println!(
            "Unknown     {} folders with no installed game: {}",
            unknown.len(),
            unknown.join(", ")
        );
    }
    if hard_linked > 0 {
        println!(
            "Note        {hard_linked} files share storage with another file; the copy gives each its own"
        );
    }
    println!(
        "Filesystem  {}",
        source_mount
            .map(|m| m.describe())
            .unwrap_or_else(|| "unknown".into())
    );
    println!("Source      {}", library.compatdata.display());
    println!("Destination {}", target.display());
    println!("To copy     {}", human_bytes(source_bytes));
    if report.existing_destination.is_some() {
        println!(
            "Note        a copy is already at the destination and will be renamed to {}",
            state::next_free(&target_parent, state::PREVIOUS_PREFIX).display()
        );
    }
    if source_exists {
        println!(
            "Backup      {} (the original, kept in place)",
            state::new_backup_path(&library.steamapps).display()
        );
    } else {
        println!("Backup      not needed, this library has no Proton data yet");
    }

    println!("Plan        {fingerprint}");
    println!(
        "Checks      symlink support is tested when you apply; path containment is {}",
        if safefs::CONTAINMENT_ENFORCED {
            "enforced by the kernel"
        } else {
            "checked by this tool only, which is weaker"
        }
    );

    if let Some(expected) = &options.expect {
        if expected != &fingerprint {
            return Err(format!(
                "this library has changed since the plan was reviewed. It was {expected} and \
                 is now {fingerprint}. Nothing was done. Review it again."
            ));
        }
    }

    if options.dry_run {
        println!();
        println!("Dry run. Nothing was changed.");
        return Ok(0);
    }
    if !options.assume_yes && !confirm("Continue?")? {
        println!("Cancelled. Nothing was changed.");
        return Ok(0);
    }

    // --- apply ---------------------------------------------------------

    // The one write that has to happen on the game drive before the copy:
    // proving the filesystem can hold the link the repair depends on.
    fsops::probe_symlink_support(&library.steamapps)?;

    fs::create_dir_all(&target_parent).map_err(|e| format!("{}: {e}", target_parent.display()))?;
    let _ = fs::set_permissions(&target_parent, fs::Permissions::from_mode(0o700));

    // A destination left behind by an earlier repair that was undone. It is
    // renamed aside rather than removed, like everything else here, so a
    // second repair never has to be unblocked by hand.
    if fs::symlink_metadata(&target).is_ok() {
        let moved = state::next_free(&target_parent, state::PREVIOUS_PREFIX);
        fs::rename(&target, &moved)
            .map_err(|e| format!("{} -> {}: {e}", target.display(), moved.display()))?;
        println!(
            "Set aside a copy from an earlier repair: {}",
            moved.display()
        );
    }

    if source_exists {
        // A fresh directory each run. A leftover from an interrupted copy is
        // left exactly where it is: its name does not prove who made it.
        let staging = library.new_staging();

        println!();
        println!("Copying...");
        let mut copied_files = 0usize;
        let mut copied_bytes = 0u64;
        let mut progress = |_path: &Path, bytes: u64| {
            copied_files += 1;
            copied_bytes += bytes;
            if copied_files % 250 == 0 {
                eprint!("\r  {copied_files} files, {}", human_bytes(copied_bytes));
                let _ = std::io::stderr().flush();
            }
        };
        let source_manifest = fsops::copy_tree(&library.compatdata, &staging, &mut progress)?;
        eprint!("\r");
        println!(
            "  {} files, {} directories, {} symlinks, {}",
            source_manifest.files,
            source_manifest.dirs,
            source_manifest.symlinks,
            human_bytes(source_manifest.bytes)
        );
        if !source_manifest.hard_linked.is_empty() {
            println!(
                "  note: {} files were hard links and are now independent copies",
                source_manifest.hard_linked.len()
            );
        }

        println!("Checking every file...");
        let copy_manifest = fsops::inventory(&staging, true)?;
        if let Err(problems) = fsops::verify_against(&source_manifest, &copy_manifest) {
            return Err(format!(
                "the copy does not match the source, so nothing on the game drive was touched.\n  {}",
                problems
                    .iter()
                    .take(10)
                    .cloned()
                    .collect::<Vec<_>>()
                    .join("\n  ")
            ));
        }

        let source_now = fsops::inventory(&library.compatdata, false)?;
        let changes = fsops::changed_since(&source_manifest, &source_now);
        if !changes.is_empty() {
            return Err(format!(
                "the source changed while it was being copied, so nothing on the game drive \
                 was touched. Make sure Steam is closed and try again.\n  {}",
                changes
                    .iter()
                    .take(10)
                    .cloned()
                    .collect::<Vec<_>>()
                    .join("\n  ")
            ));
        }
        println!("  every file matches");

        // Everything from here is ordered so that each step is on disk before
        // the next one starts. A flush that fails is reported, not ignored:
        // the record that does not survive a power cut is exactly the one
        // recovery would have needed.
        let backup = state::new_backup_path(&library.steamapps);
        let mut operation = record::Record {
            library_id: library.id.clone(),
            source: library.compatdata.clone(),
            destination: target.clone(),
            backup: backup.clone(),
            digest: fsops::manifest_digest(&copy_manifest),
            stage: record::Stage::Verified,
        };
        operation.write()?;

        record::sync(&staging)?;

        // Publish the copy and perform the cutover through open directory
        // descriptors. Each directory is opened once and every step names a
        // single entry inside it, so nothing is re-resolved from a string
        // after it has been checked.
        let destination_dir = safefs::Dir::open(&target_parent)?;
        destination_dir.rename(file_name(&staging)?, file_name(&target)?)?;
        record::sync(&target_parent)?;
        operation.advance(record::Stage::Published)?;

        // --- cutover ---------------------------------------------------

        println!();
        println!("Moving the original aside...");
        let library_dir = safefs::Dir::open(&library.steamapps)?;
        cutover_precheck(&library_dir, &library.compatdata)?;
        library_dir
            .rename(file_name(&library.compatdata)?, file_name(&backup)?)
            .map_err(|e| {
                format!(
                    "{e}. The copy is complete at {}, and the original is untouched.",
                    target.display()
                )
            })?;
        record::sync(&library.steamapps)?;
        operation.advance(record::Stage::BackedUp)?;
        println!("  {}", backup.display());
    } else {
        fs::create_dir_all(&target).map_err(|e| format!("{}: {e}", target.display()))?;
        record::sync(&target_parent)?;
    }

    println!("Linking...");
    link_and_check(&target, &library.compatdata)?;
    record::sync(&library.steamapps)?;
    // The operation is complete, so its record has nothing left to report.
    record::Record::clear(&library.id);

    println!();
    println!("Done.");
    println!("  Proton data now lives at   {}", target.display());
    println!(
        "  Steam still sees it at     {}",
        library.compatdata.display()
    );
    if source_exists {
        println!();
        println!("The original is still on the game drive. Once a game has launched and");
        println!("loaded a save, you can delete it to reclaim the space.");
    }
    Ok(0)
}

fn finish_interrupted(
    options: &Options,
    library: &Library,
    backup: &Path,
    target: &Path,
) -> Result<i32, String> {
    println!("State       an earlier run stopped just before the last step");
    println!("Original    {}", backup.display());
    println!("Copy        {}", target.display());

    if !target.is_dir() {
        return Err(format!(
            "the copy at {} is missing, so this cannot be finished automatically. \
             Your data is intact at {}. Rename it back to {} to return to the starting point.",
            target.display(),
            backup.display(),
            library.compatdata.display()
        ));
    }

    // The copy has to be shown good before Steam is pointed at it. A record
    // left by the interrupted run is the cheap proof; comparing the copy
    // against the original beside it is the authoritative one, and is what
    // happens when there is no record or it does not match.
    let evidence = recovery_evidence(library, backup, target)?;
    println!("Checked     {evidence}");

    if options.dry_run {
        println!();
        println!("Dry run. The remaining step is to create the link.");
        return Ok(0);
    }
    if !options.assume_yes && !confirm("Create the link and finish?")? {
        println!("Cancelled. Nothing was changed.");
        return Ok(0);
    }
    link_and_check(target, &library.compatdata)?;
    record::sync(&library.steamapps)?;
    record::Record::clear(&library.id);
    println!();
    println!("Done. The original is still at {}", backup.display());
    Ok(0)
}

/// Establish that the copy at `target` is complete before adopting it.
///
/// Existence used to be treated as evidence, which meant a half-written or
/// unrelated directory could become the live data.
fn recovery_evidence(library: &Library, backup: &Path, target: &Path) -> Result<String, String> {
    if let Some(operation) = record::Record::load(&library.id) {
        if operation.destination == target {
            let manifest = fsops::inventory(target, true)?;
            if fsops::manifest_digest(&manifest) == operation.digest {
                return Ok("the copy matches the one the interrupted run verified".to_string());
            }
        }
    }

    if !backup.is_dir() {
        return Err(format!(
            "there is no way to tell whether the copy at {} is complete: no record of the \
             interrupted run survived, and there is no original beside the library to \
             compare it against. Nothing was changed.",
            target.display()
        ));
    }

    if same_data(backup, target)? {
        Ok("the copy matches the original still on the game drive".to_string())
    } else {
        Err(format!(
            "the copy at {} does not match the original at {}, so it is not safe to point \
             Steam at it. Nothing was changed. Compare them yourself, or rename the original \
             back to {} to return to the starting point.",
            target.display(),
            backup.display(),
            library.compatdata.display()
        ))
    }
}

fn link_and_check(target: &Path, link_path: &Path) -> Result<(), String> {
    // The link is created relative to the opened parent, so the name cannot
    // be redirected between the check and the write.
    let parent = link_path
        .parent()
        .ok_or_else(|| format!("{}: has no parent directory", link_path.display()))?;
    safefs::Dir::open(parent)?.symlink(target, file_name(link_path)?)?;

    let meta =
        fs::symlink_metadata(link_path).map_err(|e| format!("{}: {e}", link_path.display()))?;
    if !meta.file_type().is_symlink() {
        return Err(format!(
            "{}: did not end up as a symlink",
            link_path.display()
        ));
    }
    let read_back =
        fs::read_link(link_path).map_err(|e| format!("{}: {e}", link_path.display()))?;
    if read_back != target {
        return Err(format!(
            "{}: link points at {} instead of {}",
            link_path.display(),
            read_back.display(),
            target.display()
        ));
    }
    if !link_path.is_dir() {
        return Err(format!(
            "{}: the link was created but the destination is not readable through it",
            link_path.display()
        ));
    }
    Ok(())
}

// ---------------------------------------------------------------- undo

pub fn undo(options: &Options, reference: &str) -> Result<i32, String> {
    let (libraries, _) = steam::all_libraries(options.steam_root.as_deref());
    let library = steam::resolve(&libraries, reference)?;
    let _lock = if options.dry_run {
        None
    } else {
        Some(lock::Lock::acquire(&library.id)?)
    };
    let report = state::inspect(library);
    preflight(library)?;

    let target = match &report.state {
        State::Repaired { target, .. } => target.clone(),
        State::InterruptedAwaitingLink { backup } => {
            return Err(format!(
                "this library is mid-repair. Your original is at {}. \
                 Either run `librarybridge fix {}` to finish, or rename that directory \
                 back to compatdata by hand.",
                backup.display(),
                library.id
            ))
        }
        other => {
            return Err(format!(
                "nothing to undo: this library is '{}'.",
                other.headline()
            ))
        }
    };

    // The live data is copied back, never the old backup. Saves made since
    // the repair are the ones that matter.
    let staging = state::new_restore_path(&library.steamapps);

    // The same rule a repair applies, in the other direction. A relative link
    // pointing out of the prefix means something different once the prefix
    // moves, whichever way it is moving.
    let live = fsops::inventory(&target, false)?;
    let escaping = fsops::escaping_relative_links(&target, &live);
    if !escaping.is_empty() {
        let listed: Vec<String> = escaping
            .iter()
            .take(10)
            .map(|(from, to)| format!("  {} -> {}", from.display(), to.display()))
            .collect();
        return Err(format!(
            "this prefix contains relative links that point outside it, and moving the tree \
             back would change where they lead:\n{}\n\nNothing was changed. The repair is \
             still in place and your data is still reachable through it.",
            listed.join("\n")
        ));
    }
    let live_bytes = live.bytes;

    println!("Library     {}", library.path.display());
    println!("Current     {}", target.display());
    println!("Going back  {}", library.compatdata.display());
    println!("To copy     {}", human_bytes(live_bytes));
    for backup in &report.backups {
        println!("Old backup  {} (left alone)", backup.display());
    }

    match system::free_bytes(&library.steamapps) {
        Some(free) if free < live_bytes => {
            return Err(format!(
                "not enough room on the game drive: {} needed, {} free.",
                human_bytes(live_bytes),
                human_bytes(free)
            ))
        }
        _ => {}
    }

    if options.dry_run {
        println!();
        println!("Dry run. Nothing was changed.");
        return Ok(0);
    }
    if !options.assume_yes && !confirm("Continue?")? {
        println!("Cancelled. Nothing was changed.");
        return Ok(0);
    }

    println!();
    println!("Copying the current data back...");
    let mut progress = |_p: &Path, _b: u64| {};
    let source_manifest = fsops::copy_tree(&target, &staging, &mut progress)?;
    println!(
        "  {} files, {}",
        source_manifest.files,
        human_bytes(source_manifest.bytes)
    );

    println!("Checking every file...");
    let copy_manifest = fsops::inventory(&staging, true)?;
    if let Err(problems) = fsops::verify_against(&source_manifest, &copy_manifest) {
        return Err(format!(
            "the copy back does not match, so the repair was left in place.\n  {}",
            problems
                .iter()
                .take(10)
                .cloned()
                .collect::<Vec<_>>()
                .join("\n  ")
        ));
    }

    // The same recheck a repair does: if the live prefix changed while it was
    // being copied, the copy is a mixture and must not become the live data.
    let live_now = fsops::inventory(&target, false)?;
    let changes = fsops::changed_since(&source_manifest, &live_now);
    if !changes.is_empty() {
        return Err(format!(
            "the data changed while it was being copied back, so the repair was left in \
             place. Make sure Steam is closed and try again.\n  {}",
            changes.iter().take(10).cloned().collect::<Vec<_>>().join("\n  ")
        ));
    }
    println!("  every file matches");

    // Removing the symlink deletes a link, never data. Both steps happen
    // relative to the opened library directory, and the removal refuses
    // anything that is not a symlink.
    let library_dir = safefs::Dir::open(&library.steamapps)?;
    library_dir.remove_symlink(file_name(&library.compatdata)?)?;
    library_dir
        .rename(file_name(&staging)?, file_name(&library.compatdata)?)
        .map_err(|e| {
            format!(
                "{e}. The data is safe at {} and at {}.",
                staging.display(),
                target.display()
            )
        })?;
    sync_dir(&library.steamapps);

    println!();
    println!("Done. Steam is using the game drive again.");
    println!("  Still on disk, delete when you are happy:");
    println!("    {}", target.display());
    for backup in &report.backups {
        println!("    {}", backup.display());
    }
    Ok(0)
}

// ---------------------------------------------------------------- helpers

/// Are two trees the same data?
///
/// Entry counts and byte totals are checked first because they are cheap and
/// a mismatch settles it. Matching totals prove nothing on their own: two
/// saves of the same length are the case that made this necessary. So when
/// the shape matches, every file is hashed and every link text compared
/// before the trees are called equivalent.
fn same_data(left: &Path, right: &Path) -> Result<bool, String> {
    let left_shape = fsops::inventory(left, false)?;
    let right_shape = fsops::inventory(right, false)?;
    if left_shape.entries.len() != right_shape.entries.len()
        || left_shape.bytes != right_shape.bytes
    {
        return Ok(false);
    }
    let left_full = fsops::inventory(left, true)?;
    let right_full = fsops::inventory(right, true)?;
    Ok(fsops::verify_against(&left_full, &right_full).is_ok())
}

fn conflict_message(
    library: &Library,
    destination: &Path,
    source_exists: bool,
) -> Result<String, String> {
    let there = fsops::inventory(destination, false)?;
    let here = if source_exists {
        let here = fsops::inventory(&library.compatdata, false)?;
        format!(
            "{} ({} files, {})",
            library.compatdata.display(),
            here.files,
            human_bytes(here.bytes)
        )
    } else {
        format!("{} (nothing there)", library.compatdata.display())
    };

    let lines = [
        "two different sets of Proton data exist, and only you can say which one counts."
            .to_string(),
        String::new(),
        format!(
            "  at the destination: {} ({} files, {})",
            destination.display(),
            there.files,
            human_bytes(there.bytes)
        ),
        format!("  on the game drive:  {here}"),
        String::new(),
        "This usually means an earlier repair was interrupted and Steam recreated".to_string(),
        "compatdata before it could be finished. If so, the destination holds your real".to_string(),
        "prefixes and the game drive holds a fresh empty one.".to_string(),
        String::new(),
        "Nothing has been changed. Look at both, then choose:".to_string(),
        String::new(),
        format!(
            "  librarybridge fix {} --keep-destination     keep the destination, set the game drive copy aside",
            library.id
        ),
        format!(
            "  librarybridge fix {} --replace-destination  keep the game drive copy, set the destination aside",
            library.id
        ),
        String::new(),
        "Either way, both copies are kept.".to_string(),
    ];
    Ok(lines.join("\n"))
}

/// Finish by keeping what is at the destination: the current compatdata is
/// renamed to a backup, and the link is created pointing at the destination.
fn keep_destination(
    options: &Options,
    library: &Library,
    destination: &Path,
) -> Result<i32, String> {
    let backup = state::new_backup_path(&library.steamapps);
    println!("Keeping     {}", destination.display());
    println!("Setting aside the game drive copy as {}", backup.display());

    if options.dry_run {
        println!();
        println!("Dry run. Nothing was changed.");
        return Ok(0);
    }
    if !options.assume_yes && !confirm("Continue?")? {
        println!("Cancelled. Nothing was changed.");
        return Ok(0);
    }

    let library_dir = safefs::Dir::open(&library.steamapps)?;
    if library_dir.kind(file_name(&library.compatdata)?)? != safefs::Kind::Absent {
        library_dir.rename(file_name(&library.compatdata)?, file_name(&backup)?)?;
    }
    link_and_check(destination, &library.compatdata)?;
    sync_dir(&library.steamapps);

    println!();
    println!("Done. Steam reads the data at {}", destination.display());
    println!("The copy that was on the game drive is at {}", backup.display());
    Ok(0)
}

/// A short identity for what is about to happen: which library, from where,
/// to where, in what state, and how much data. Anything that would change the
/// operation changes this.
fn plan_fingerprint(
    library: &Library,
    report: &state::Report,
    target: &Path,
    entries: usize,
    bytes: u64,
) -> String {
    let mut hasher = Sha256::new();
    for part in [
        library.id.as_str(),
        &library.path.to_string_lossy(),
        &library.compatdata.to_string_lossy(),
        &target.to_string_lossy(),
        report.state.code(),
    ] {
        hasher.update(part.as_bytes());
        hasher.update(b"\0");
    }
    hasher.update(&(entries as u64).to_le_bytes());
    hasher.update(&bytes.to_le_bytes());
    hex(&hasher.finish())[..16].to_string()
}

/// The reviewed plan as data, so a frontend does not have to read prose to
/// find out what is about to happen.
#[allow(clippy::too_many_arguments)]
fn plan_json(
    library: &Library,
    report: &state::Report,
    target: &Path,
    backup: &Path,
    copy_bytes: u64,
    reserve_bytes: u64,
    available: Option<u64>,
    installed_games: usize,
    prefixes: usize,
    unknown: &[String],
    hard_linked: usize,
    fingerprint: &str,
) -> String {
    let number = |value: Option<u64>| match value {
        Some(value) => value.to_string(),
        None => "null".to_string(),
    };
    let remaining = available.map(|free| free.saturating_sub(copy_bytes));

    let mut consequences: Vec<String> =
        vec!["Game installation files stay on the game drive.".to_string()];
    if library.install_kind == steam::InstallKind::Flatpak {
        consequences.push(
            "The data will live inside Steam's Flatpak directory. Removing Steam and its data \
             would remove it too."
                .to_string(),
        );
    }
    if hard_linked > 0 {
        consequences.push(format!(
            "{hard_linked} files currently share storage with another file. Each gets its own \
             copy, so the result uses more space than the source."
        ));
    }

    let list = |items: &[String]| {
        items
            .iter()
            .map(|item| json_string(item))
            .collect::<Vec<_>>()
            .join(", ")
    };

    let mut out = String::from("{\n");
    out.push_str("  \"schema\": 1,\n");
    out.push_str(&format!("  \"fingerprint\": {},\n", json_string(fingerprint)));
    out.push_str("  \"kind\": \"repair\",\n");
    out.push_str(&format!("  \"library_id\": {},\n", json_string(&library.id)));
    out.push_str(&format!(
        "  \"library\": {},\n",
        json_string(&library.path.to_string_lossy())
    ));
    out.push_str(&format!("  \"state\": {},\n", json_string(report.state.code())));
    out.push_str(&format!(
        "  \"steam\": {},\n",
        json_string(library.install_kind.label())
    ));
    out.push_str(&format!(
        "  \"source\": {},\n",
        json_string(&library.compatdata.to_string_lossy())
    ));
    out.push_str(&format!(
        "  \"destination\": {},\n",
        json_string(&target.to_string_lossy())
    ));
    out.push_str(&format!(
        "  \"backup\": {},\n",
        json_string(&backup.to_string_lossy())
    ));
    out.push_str(&format!("  \"copy_bytes\": {copy_bytes},\n"));
    out.push_str(&format!("  \"reserve_bytes\": {reserve_bytes},\n"));
    out.push_str(&format!("  \"available_bytes\": {},\n", number(available)));
    out.push_str(&format!(
        "  \"expected_remaining_bytes\": {},\n",
        number(remaining)
    ));
    out.push_str(&format!("  \"installed_games\": {installed_games},\n"));
    out.push_str(&format!("  \"prefix_folders\": {prefixes},\n"));
    out.push_str(&format!("  \"unknown_folders\": [{}],\n", list(unknown)));
    out.push_str(&format!("  \"consequences\": [{}]\n", list(&consequences)));
    out.push_str("}\n");
    out
}

/// The checks every path that changes a library must pass, whether it is a
/// repair, a recovery, a conflict resolution or an undo.
///
/// Recovery used to skip these entirely, which meant an interrupted repair
/// could be finished while Steam was running.
fn preflight(library: &Library) -> Result<(), String> {
    let running = system::running_steam_processes().map_err(|reason| {
        format!(
            "{reason}. LibraryBridge will not change a library while it cannot tell whether \
             Steam is running, because writes made during the move would be lost."
        )
    })?;
    if !running.is_empty() {
        return Err(format!(
            "these processes are running: {}. Close Steam and any running game first, \
             otherwise writes made during the move would be lost.",
            running.join(", ")
        ));
    }

    if let Some(mount) = system::mount_for(&library.path) {
        if mount.read_only {
            return Err(format!(
                "{} is mounted read-only. Remount it writable and try again. \
                 A Windows fast-startup or hibernation state is the usual cause.",
                mount.mount_point.display()
            ));
        }
    }
    Ok(())
}

/// The last component of a path, which is what a descriptor-relative call
/// takes. A path with no last component is a bug, not a user error.
fn file_name(path: &Path) -> Result<&Path, String> {
    path.file_name()
        .map(Path::new)
        .ok_or_else(|| format!("{}: has no final component", path.display()))
}

/// The last look before the game drive is touched.
///
/// The type is read through the opened directory rather than by path, and on
/// Linux the kernel is asked to confirm the name still resolves without
/// leaving that directory. A component swapped for a symlink since the plan
/// was made is caught here.
fn cutover_precheck(dir: &safefs::Dir, compatdata: &Path) -> Result<(), String> {
    let name = file_name(compatdata)?;
    match dir.kind(name)? {
        safefs::Kind::Directory => {}
        other => {
            return Err(format!(
                "{} is a {} now, not a directory. Something changed it since this repair was \
                 planned, so nothing was moved.",
                compatdata.display(),
                other.label()
            ))
        }
    }
    if !dir.resolves_beneath(name)? {
        return Err(format!(
            "{} no longer resolves inside {}. Something replaced a directory along that path \
             since this repair was planned, so nothing was moved.",
            compatdata.display(),
            dir.path().display()
        ));
    }
    Ok(())
}

fn existing_ancestor(path: &Path) -> PathBuf {
    let mut probe = path.to_path_buf();
    while !probe.exists() {
        if !probe.pop() {
            return PathBuf::from("/");
        }
    }
    probe
}

fn sync_dir(path: &Path) {
    if let Ok(handle) = fs::File::open(path) {
        let _ = handle.sync_all();
    }
}

fn confirm(question: &str) -> Result<bool, String> {
    print!("\n{question} [y/N] ");
    std::io::stdout().flush().map_err(|e| e.to_string())?;
    let mut answer = String::new();
    std::io::stdin()
        .read_line(&mut answer)
        .map_err(|e| e.to_string())?;
    Ok(matches!(answer.trim(), "y" | "Y" | "yes" | "Yes"))
}
