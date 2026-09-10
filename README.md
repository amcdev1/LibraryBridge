<p align="center">
  <img src="assets/branding/librarybridge-controller-bridge-top-lb-1024.png" alt="LibraryBridge icon" width="180">
</p>

# LibraryBridge

Keep Windows game files on their existing drive while putting Proton data on a
Linux filesystem. LibraryBridge also finds existing GOG and standalone games
and adds them to Lutris.

LibraryBridge runs on Linux. It is currently built from source, and there are
no packaged releases yet.

> LibraryBridge is independent and is not affiliated with or endorsed by Valve
> Corporation. Steam and Proton are trademarks of Valve Corporation.

## What it does

### Steam libraries

Proton stores a Wine prefix for each game in `steamapps/compatdata`. Those
prefixes need Linux filesystem features that are not reliable on NTFS. The
game files can stay on the NTFS drive while their Proton data moves to a Linux
filesystem.

LibraryBridge copies and verifies the data, keeps the original as
`compatdata.backup`, and leaves a link where Steam expects it. Your game files
do not move, and Steam does not need a configuration change.

### Lutris games

LibraryBridge can scan folders for GOG and standalone Windows games that
Lutris does not know about. It shows the likely executable for each game so
you can review the result before adding it to Lutris. Game files, prefixes,
and saves are not moved.

## Requirements

- Linux
- Steam for the Steam repair feature
- Lutris for the optional game import feature
- Rust and Cargo to build from source
- A Linux filesystem with enough free space for the moved Proton data

## Install from source

```bash
git clone https://github.com/amcdev7/LibraryBridge.git
cd LibraryBridge
cargo build --locked --release --workspace
```

The binaries are created in `target/release`:

- `librarybridge`, the command-line tool
- `librarybridge-gui`, the desktop window

### Optional desktop integration

For a source build, this installs the app-menu entry, icons, and links to the
release binaries in `~/.local/bin`:

```bash
packaging/install-desktop.sh
```

This step is not needed for a packaged release. Packages should install the
desktop entry and icons themselves.

## Quick start

Close Steam before repairing a library.

List the libraries and find the id to use in the next commands:

```bash
./target/release/librarybridge scan
```

Preview a repair without changing anything:

```bash
./target/release/librarybridge fix <id> --dry-run
```

If the preview looks right, apply it:

```bash
./target/release/librarybridge fix <id>
```

The command asks for confirmation unless you pass `--yes`. The original
Proton data stays beside the library as `compatdata.backup`.

To move the current data back to the game drive:

```bash
./target/release/librarybridge undo <id>
```

Do not delete the backup by hand. After you have confirmed that the game
works, `backup` can remove it and reclaim the space. This is the only command
that deletes data, and it requires recorded evidence that the game launched
and saved successfully.

```bash
./target/release/librarybridge backup <id>
```

## Choosing where Proton data goes

By default, moved data lives under:

```text
~/.local/share/librarybridge/<library>-<id>/compatdata
```

To use another Linux filesystem with more space:

```bash
./target/release/librarybridge \
  --data-dir /mnt/games/librarybridge \
  fix <id>
```

The destination can be ext4, btrfs, xfs, or another Linux filesystem that
supports the required features. It cannot be exFAT or the same filesystem as
the Steam library.

To see how much space the moved data uses:

```bash
./target/release/librarybridge storage
```

## Desktop window

```bash
./target/release/librarybridge-gui
```

The window shows library status, explains what needs attention, previews each
repair, and runs the same operations as the command-line tool. It also has
the Lutris scan and import flow.

## Import games into Lutris

Use this when GOG or standalone Windows games already exist on a drive and you
want Lutris to manage them.

Check that Lutris is installed:

```bash
./target/release/librarybridge lutris detect
```

Scan a folder:

```bash
./target/release/librarybridge lutris scan \
  --root /run/media/you/Games
```

The scan reports candidate ids. Review the candidates, then create an import
plan and pass it to Lutris:

```bash
./target/release/librarybridge lutris plan \
  --root /run/media/you/Games \
  --candidate <id> \
  --output plan.json

./target/release/librarybridge lutris import --plan plan.json
```

Lutris opens its own installer dialog for each game. Steam games are not
imported because Lutris already lists them through its Steam integration.

## Safety and recovery

- `scan`, `storage`, and `--dry-run` do not change files.
- A repair copies and verifies the data before changing the Steam library.
- A repair keeps the original. It does not delete it.
- `undo` copies the current data back, including saves made after the repair.
- `backup` is the only command that deletes anything. It checks the moved copy
  and requires evidence that the game launched and saved successfully.
- An interrupted repair can be reviewed again. Unfinished data is kept rather
  than deleted automatically.

## Limitations

- Linux only
- exFAT libraries are detected but are not repaired automatically
- Timestamps, hard-link relationships, and sparse-file allocation are not
  preserved by the copy
- A successful repair fixes the location of Proton data. It does not guarantee
  that a particular game will run

## Troubleshooting

Run a fresh scan first. Include the LibraryBridge version, Linux distribution
and kernel, filesystem and mount driver, Steam installation type, and redacted
command output when [opening an issue](https://github.com/amcdev7/LibraryBridge/issues).

Do not attach saves, Proton prefixes, registry files, or Steam account data.

## For contributors

```bash
cargo test --locked --workspace
cargo clippy --locked --workspace --all-targets -- -D warnings
cargo build --locked --release --workspace
```

The GUI has deterministic preview data for visual checks:

```bash
cargo run -p librarybridge-gui -- --preview --screen home
```

## License

LibraryBridge is available under the MIT License. See [LICENSE](LICENSE).
