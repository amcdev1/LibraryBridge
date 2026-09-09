<p align="center">
  <img src="assets/branding/librarybridge-controller-bridge-top-lb-1024.png" alt="LibraryBridge icon" width="180">
</p>

# LibraryBridge

Move Steam Proton compatibility data off game drives that cannot host it, onto a
Linux filesystem — without deleting the original.

LibraryBridge finds Steam libraries whose Proton `compatdata` sits on a
filesystem that cannot safely support it (NTFS), copies that data to a Linux
filesystem, verifies the copy file by file, and leaves a symlink where Steam
expects the data to be. It can also find standalone and GOG games and prepare
them for Lutris.

> LibraryBridge is an independent project and is not affiliated with, endorsed
> by, or sponsored by Valve Corporation or Steam. Steam and Proton are
> trademarks of Valve Corporation.

## Why the data has to move

A Wine prefix — the `pfx` inside each game's `compatdata` folder — needs
real Linux filesystem behavior to work under Proton:

- POSIX permissions on every file and directory
- symlinks that behave like Linux symlinks
- case-sensitive names
- safe `dosdevices` drive mappings

**NTFS provides none of that reliably**, no matter which driver mounts it. A
prefix on NTFS fails in subtle ways: games that install fine break when they
try to write, saves corrupt, permissions collapse, and case-only renames
collide. Proton itself documents NTFS as unsupported for its prefixes.

So the problem is not *where the symlink points* — it is that the data
physically sits on a filesystem that cannot hold a working prefix.

**A symlink alone cannot fix this.** A symlink only says "look over here
instead." If the data stays on NTFS, the symlink points at the same broken
filesystem. LibraryBridge's repair does both halves of the fix:

1. **It moves the data** — copies every prefix to a Linux filesystem and
   verifies the copy.
2. **It redirects Steam** — renames the original aside as `compatdata.backup`
   and puts a symlink where Steam looks, so Steam keeps working with no
   configuration changes.

The data is on Linux where it can actually work; the game drive keeps its
games and the original data; and Steam never knows the difference.

## Where the data goes

Relocated data lives under your Linux data home by default:

```
~/.local/share/librarybridge/<Library>-<id>/compatdata
```

If you have a roomier Linux filesystem (a dedicated games SSD, a second drive
formatted ext4 or btrfs), point repairs at it with `--data-dir`:

```bash
librarybridge --data-dir /mnt/games/librarybridge fix <id>
```

The destination must be a Linux filesystem for the same reason the data had to
leave NTFS in the first place. exFAT is refused; so is a destination on the
same filesystem as the library.

> Note: `--data-dir` applies to *new* repairs. An existing repair stays where
> it is, and the tool keeps treating it as the repaired location for that
> library.

**Space.** The copied prefixes live on the Linux drive, so they take up room
there, not on the game drive. Your played games are what add prefixes; games
you have never launched add nothing. Check the summary at any time:

```bash
librarybridge storage
```

It shows each moved library, how much it occupies, what the original still
holds, and how much headroom is left. When a game has launched and loaded its
saves from the moved copy, you can reclaim the game drive's space:

```bash
librarybridge backup <id>
```

The backup is deleted only when the library is repaired, you have recorded
that a game actually works, and the moved copy still matches what it replaced.
Nothing else is ever deleted.

## Features

- **Read-only by default.** `scan` and `--dry-run` change nothing.
- **Verified copies.** Every file is checked against the source before the
  original is touched.
- **Nothing is ever deleted by a repair.** Your original stays as
  `compatdata.backup` until you remove it.
- **Recoverable.** An interrupted repair is finished, not restarted, by
  running `fix` again. `undo` copies the current data back.
- **Native and Flatpak Steam.** Both layouts are discovered and repaired; only
  one repair scope is created for a library shared by both.
- **NTFS detection that matches reality.** Kernel `ntfs3` and `ntfs-3g`
  (including udisks-style mounts that hide the driver) are recognised and
  reported; unknown filesystems are blocked with an explanation rather than
  guessed.
- **Lutris import.** Finds GOG and standalone Windows games and hands them to
  Lutris through its own installer, one dialog per game.
- **CLI and window, one engine.** The desktop window only runs the command
  line tool and shows its output, so the two can never disagree.

## Requirements

- **Linux.** LibraryBridge reads `/proc/self/mountinfo` and needs a display
  per session. It is not supported elsewhere.
- **Rust and Cargo** to build from source (no packaged releases yet).
- **A Linux filesystem for the destination** (ext4, btrfs, xfs…). exFAT is
  intentionally refused.

## Installation

```bash
cargo build --locked --release --workspace
```

The binaries land in `target/release`: `librarybridge` (the core tool) and
`librarybridge-gui` (an optional window over the same commands).

### Desktop integration (so the icon shows)

On Wayland (and most modern desktops) the *taskbar and app-menu icon is
provided by a `.desktop` entry*, not by the window itself. Without it you get
a generic icon. The same script also symlinks the binaries into `~/.local/bin`
so the app can be launched from the app grid:

```bash
packaging/install-desktop.sh
```

Run it after every build that changes the GUI. It installs
`~/.local/share/applications/librarybridge.desktop` and the hicolor icons
(16→512 px), creates `~/.local/bin/librarybridge{,-gui}`, and refreshes the
desktop caches. When distributing, ship the two binaries **and** the
`packaging/linux/usr/share/{applications,icons/hicolor}` tree, and run this
script (or your packaging system's equivalent) so the icon and launch entry
follow the app.

To undo: remove `~/.local/share/applications/librarybridge.desktop`,
`~/.local/share/icons/hicolor/*/apps/librarybridge.png`, and
`~/.local/bin/librarybridge{,-gui}`.

## Quick start

```bash
./target/release/librarybridge scan
```

`scan` changes nothing. It lists your Steam libraries, the filesystem each one
is on, how many prefixes it holds, and whether the repair applies. For a
library that needs it:

```bash
./target/release/librarybridge fix <id> --dry-run   # preview, changes nothing
./target/release/librarybridge fix <id>             # run it
```

**Close Steam first.** The tool refuses to run a repair while Steam is open,
because writes made during the move would be lost.

Inspect what is kept, and reverse a repair when needed:

```bash
./target/release/librarybridge storage
./target/release/librarybridge backup <id>   # reclaim the game drive after confirmation
./target/release/librarybridge undo <id>     # copy the current data back, remove the link
```

`undo` copies the *current* data back — saves made since the repair survive.
`backup` is the only deletion in the whole tool, and it is gated (see
[Where the data goes](#where-the-data-goes)).

If a repair is interrupted, run `fix` again. It detects the state left behind
and finishes the remaining step instead of starting over.

## Desktop window

```bash
./target/release/librarybridge-gui
```

The window shows the same libraries, filesystems, and states as `scan`, keeps
the location of every library visible, and shows a coloured status for each
one. It can set where relocated data goes (the same `--data-dir` choice, saved
between sessions), review a repair as the tool would run it, and apply it.

Every action the window takes runs the command line tool and displays that
tool's own output, so nothing the window says can disagree with what the tool
does. Anything reachable in the window is also reachable from a terminal —
which is what keeps recovery honest if the window will not start.

## Optional: import games into Lutris

```bash
./target/release/librarybridge lutris detect
./target/release/librarybridge lutris scan --root /run/media/you/Games
```

This reads only the folders you name. It finds GOG installs from their own
metadata, DRM-free Windows games by ranking the executables, and native Linux
launchers — explaining each choice and its confidence. Steam games are listed
but not offered for import, because Lutris already shows those through its own
Steam source.

```bash
./target/release/librarybridge lutris plan --root /run/media/you/Games --candidate <id> --output plan.json
./target/release/librarybridge lutris import --plan plan.json
```

`plan` writes a file you can read and edit first. `import` rechecks every path
and hands each game to Lutris through its own installer, which shows a dialog
per game. No game files, prefixes, or saves are ever modified.

## Safety model

- A repair **never deletes**. It renames your original to
  `compatdata.backup` beside itself and leaves it there.
- The copy is written under a temporary name and only renamed into place after
  every file has been verified by hash.
- Because no step destroys anything, every state an interruption can leave
  behind is readable straight off the disk — there is no journal or registry
  that can drift from reality.
- A repair proves the files copied. It does **not** prove a particular game
  runs under Proton; that is a separate check you record with
  `librarybridge evidence <id>` after playing.

## Development

```bash
cargo test --locked --workspace
cargo clippy --locked --workspace --all-targets -- -D warnings
cargo build --locked --release --workspace
```

The automated suite covers parsing, discovery, copying, verification,
recovery, symlink safety, dry-run behavior, and command flows against
synthetic Steam trees.

## Limitations

- **Timestamps, hard links, and sparse files** are not preserved on copy. The
  review discloses hard links before a repair; the rest is disclosed here.
- **No lock between two running copies** of the tool. Run one at a time.
- **Do not delete `compatdata.backup` by hand before you are sure.** Use
  `librarybridge backup <id>` so the tool verifies first.
- A successful repair does not make a game work under Proton; it only fixes
  where Proton's data lives.

## Reporting a problem and contributing

Keep changes focused, run the development checks above, and describe the
filesystem and Steam behavior you tested. When reporting an issue, include the
LibraryBridge version or commit, your Linux distribution and kernel,
filesystem and mount driver, Steam installation type, and the command output
with personal paths redacted. Never include game saves, Proton prefixes,
registry files, or Steam account data.

## License

LibraryBridge is available under the MIT License. See [LICENSE](LICENSE).

## In plain English

Keep your games on the external drive. Put Proton's working data on a
filesystem it can use — a Linux one. Preserve what was already there, explain
everything that still needs attention, and never delete anything a repair did
not clearly replace.