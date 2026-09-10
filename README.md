<p align="center">
  <img src="assets/branding/librarybridge-controller-bridge-top-lb-1024.png" alt="LibraryBridge icon" width="180">
</p>

# LibraryBridge

Reuse an existing Windows game library on Linux: fix Steam Proton prefixes on
NTFS without moving games, and import GOG/standalone games into Lutris.

LibraryBridge helps in two common situations:

- **Steam:** keep the games on an existing NTFS drive, while moving Proton's
  compatibility data to a Linux filesystem where it works reliably.
- **Lutris:** find GOG and standalone Windows games that are already on a
  drive, then add them to Lutris without moving the game files.

The tool is Linux-only and is currently built from source. No packaged release
is available yet.

> LibraryBridge is an independent project and is not affiliated with, endorsed
> by, or sponsored by Valve Corporation or Steam. Steam and Proton are
> trademarks of Valve Corporation.

## Why the data has to move

When Proton runs a Windows game, it creates a per-game Wine prefix in
`steamapps/compatdata`. That prefix needs Linux filesystem features such as
working permissions, symlinks, and case-sensitive filenames.

This is a common problem when reusing a Windows game drive on Linux. The game
files can stay on NTFS, but Proton's working data should live on a Linux
filesystem. exFAT is detected but cannot be repaired automatically.

LibraryBridge:

1. Copies the existing Proton data to a Linux filesystem.
2. Verifies the copy file by file.
3. Keeps the original as `compatdata.backup`.
4. Leaves a link where Steam expects the data.

Your games stay where they are, and Steam needs no configuration change.

## Where the data goes

By default, moved Proton data lives under:

```
~/.local/share/librarybridge/<library>-<id>/compatdata
```

You can choose a roomier Linux filesystem instead:

```bash
librarybridge --data-dir /mnt/games/librarybridge fix <id>
```

The destination must be a Linux filesystem such as ext4, btrfs, or xfs. It
cannot be exFAT or the same filesystem as the Steam library.

Only Proton data is copied. The game files remain on the original drive.

Use `storage` to see how much space the moved data uses:

```bash
librarybridge storage
```

## Features

- Scans existing Steam libraries and explains what needs attention.
- Works with native Steam and Flatpak Steam layouts.
- Copies and verifies data before changing the Steam library.
- Keeps the original data and supports recovery with `undo`.
- Shows a dry-run review before a repair.
- Finds existing GOG and standalone Windows games for Lutris.
- Provides both a command-line tool and a desktop window.

## Requirements

- Linux.
- Steam for the Steam repair feature.
- Lutris for the optional game import feature.
- A Linux filesystem with enough free space for the moved Proton data.

## Installation

There are no packaged releases yet. Build from source with Rust and Cargo:

```bash
cargo build --locked --release --workspace
```

The binaries are created in `target/release`:

- `librarybridge` — command-line tool
- `librarybridge-gui` — desktop window

### Desktop integration (so the icon shows)

For a local build, install the desktop entry and icons with:

```bash
packaging/install-desktop.sh
```

This adds LibraryBridge to your app menu and puts the binaries in
`~/.local/bin`. Future packages will handle this automatically.

## Quick start

Close Steam before repairing a library.

First, scan your libraries:

```bash
./target/release/librarybridge scan
```

Preview a repair without changing anything:

```bash
./target/release/librarybridge fix <id> --dry-run
```

If the review looks right, apply it:

```bash
./target/release/librarybridge fix <id>
```

The original Proton data is kept beside the library as
`compatdata.backup`. To reverse a repair, copy the current data back with:

```bash
./target/release/librarybridge undo <id>
```

Do not remove the backup by hand. After you have confirmed that the games work,
`backup` can remove the original copy and reclaim that space:

```bash
./target/release/librarybridge backup <id>
```

## Desktop window

```bash
./target/release/librarybridge-gui
```

The window shows your libraries, explains their status, lets you choose the
destination, previews repairs, and runs the same commands as the CLI.

## Optional: import games into Lutris

Use this when GOG or standalone Windows games already exist on a drive and you
want Lutris to manage them.

```bash
./target/release/librarybridge lutris detect
./target/release/librarybridge lutris scan --root /run/media/you/Games
```

LibraryBridge looks for GOG metadata and likely game executables. It does not
move game files, prefixes, or saves. Steam games are not imported because
Lutris already lists them through its Steam integration.

Review the games first, then import the ones you choose:

```bash
./target/release/librarybridge lutris plan \
  --root /run/media/you/Games \
  --candidate <id> \
  --output plan.json

./target/release/librarybridge lutris import --plan plan.json
```

Lutris shows its own installer dialog for each game.

## Safety model

- `scan` and `--dry-run` do not change anything.
- A repair verifies the copy before changing the library.
- A repair does not delete the original data.
- Interrupted repairs can be resumed with `fix`.
- `undo` copies current data back, so saves made after a repair are retained.
- `backup` is the only command that deletes anything, and it is gated by
  verification and recorded evidence.

## Development

```bash
cargo test --locked --workspace
cargo clippy --locked --workspace --all-targets -- -D warnings
cargo build --locked --release --workspace
```

The automated tests cover parsing, discovery, copying, verification, recovery,
symlink safety, dry-run behavior, and command flows using synthetic trees.

## Limitations

- Linux only; real Linux, Steam, Proton, Lutris, and Steam Deck testing is
  still required before a public repair release.
- exFAT libraries are detected but are not repaired automatically.
- Timestamps, hard-link relationships, and sparse-file allocation are not
  preserved by the copy.
- A successful repair fixes the location of Proton data; it does not guarantee
  that a particular game will run.

## Reporting a problem and contributing

Please include the LibraryBridge version, Linux distribution and kernel,
filesystem and mount driver, Steam installation type, and redacted command
output. Never attach saves, Proton prefixes, registry files, or Steam account
data.

## License

LibraryBridge is available under the MIT License. See [LICENSE](LICENSE).

## In plain English

Keep your Windows games on the drive where they already live. Put Proton's
Linux-specific data on a Linux filesystem, or let Lutris find existing games
for you. LibraryBridge explains what it will do, verifies its copies, and keeps
the original data until you decide it is safe to remove.
