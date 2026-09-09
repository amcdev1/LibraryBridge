<p align="center">
  <img src="assets/branding/librarybridge-controller-bridge-top-lb-1024.png" alt="LibraryBridge icon" width="180">
</p>

# LibraryBridge

Proton stores its Windows compatibility data beside each Steam library. On an
NTFS drive that data does not work properly, and on exFAT it cannot work at
all. LibraryBridge moves that data onto a filesystem that can hold it, leaves
a symlink where Steam expects to find it, and never deletes your original.

It also finds installed games that no launcher knows about, such as GOG
installs and standalone Windows games sitting on the same drive, and can add
them to Lutris.

> LibraryBridge is an independent project and is not affiliated with, endorsed by, or sponsored by Valve Corporation or Steam. Steam and Proton are trademarks of Valve Corporation.

**Status: early Linux preview — not ready for important data.**

The command-line tool, Lutris integration, and desktop window are implemented
and covered by 105 automated tests. Linux filesystem and Steam/Proton behavior
still need validation on a real Linux machine.

## Safety first

**Nothing is ever deleted.** A repair renames your original `compatdata` to
`compatdata.backup` beside itself and leaves it there.
The copy is written under a temporary name and only renamed into place after
every file has been verified by hash.

That rule is why there is no journal, no transaction log and no registry file.
Because no step destroys anything, every state an interruption can leave
behind can be read straight off the disk.

## Install or build from source

There are no packaged downloads yet. Build the two binaries on Linux with
Rust and Cargo:

```bash
cargo build --locked --release --workspace
```

The binaries land in `target/release`: `librarybridge` and
`librarybridge-gui`. The CLI is the core repair tool; the GUI is an optional
window over the same commands.

## Before you start

- Use a Linux filesystem for the destination. exFAT is intentionally refused.
- Close Steam before running a repair. LibraryBridge refuses to mutate a live
  Steam installation.
- Start with a disposable library or a backup you have verified separately.
- Run `scan` and the `--dry-run` preview before applying anything.

## Repairing a Steam library

```bash
./target/release/librarybridge scan
```

`scan` changes nothing and needs no arguments. It lists your Steam libraries,
the filesystem each one is on, how many prefixes it holds, and whether the
repair applies. If it does:

```bash
./target/release/librarybridge fix <id> --dry-run
./target/release/librarybridge fix <id>
```

Close Steam first; the tool refuses to run while it is open. `fix` copies the
library's compatdata to your Linux drive, checks every file, renames the
original aside, and puts a symlink where Steam expects it.

Inspect retained data and reverse a repair when needed:

```bash
./target/release/librarybridge storage
./target/release/librarybridge undo <id>
```

`undo` copies the *current* data back, not the backup, so saves made since the
repair are the ones that survive. Do not delete a `.backup` directory until
you have launched a game and confirmed its saves.

If a repair is interrupted, run `fix` again. It detects that the original was
already moved aside and finishes the remaining step.

## Finding games Lutris does not have

This reads only the folders you name.

```bash
./target/release/librarybridge lutris detect
./target/release/librarybridge lutris scan --root /run/media/you/Games
```

It finds GOG installs from their metadata, DRM-free Windows games by ranking
the executables in each folder, and native Linux launchers. It tells you why
it picked each one and how confident it is, and offers the runner-up
executables when it is guessing.

Steam games are listed but not offered for import, because Lutris already
shows those through its own Steam source and importing them would make
duplicates.

```bash
./target/release/librarybridge lutris plan --root /run/media/you/Games --candidate <id> --output plan.json
./target/release/librarybridge lutris import --plan plan.json
```

`plan` writes a file you can read and edit before anything reaches Lutris.
`import` rechecks every path and hands each game to Lutris through its own
installer, which shows a dialog per game. No game files, prefixes or saves are
ever modified.

## The window

```bash
./target/release/librarybridge-gui
```

Two screens for the two jobs. Folders are picked with the desktop's own file
chooser, or typed in if no chooser is available.

The window holds no repair or discovery logic. It runs the command line tool
beside it and shows you that tool's own dry run before anything changes, so
the two can never describe an operation differently. Anything the window can
do is reachable from a terminal, which is what keeps recovery honest when the
window will not start.

## Current limits

- **Linux is required for real use.** macOS is only a development and test
  environment.
- **exFAT is refused**, because it cannot provide the symlink behavior this
  repair needs.
- **No packages or release downloads exist yet.** You build from source.
- **Steam Runtime, Flatpak Steam, real Lutris, Steam Cloud, and NTFS drivers
  still need Linux acceptance testing.**
- **Timestamps, hard links, and sparse files** are not preserved on copy.
- **There is no lock** between two running copies of the tool.

A successful repair does not prove that a particular game works under Proton.

## Testing

```bash
cargo test --locked --workspace
```

The automated suite covers parsing, discovery, copying, verification, recovery,
symlink safety, dry-run behavior, and the command flows against synthetic Steam
trees. It does not replace testing on Linux with real filesystems, Steam, Proton,
Steam Cloud, or Lutris.

## Reporting a problem

When reporting an issue, include the LibraryBridge version or commit, Linux
distribution and kernel, filesystem and mount driver, Steam installation type,
and the command output with personal paths redacted. Do not attach saves, whole
Proton prefixes, registry files, or Steam account configuration.

## In plain English

Keep your games on the external drive. Put Proton data on a filesystem it can
use. Preserve what was already there, and explain anything that still needs
attention.
