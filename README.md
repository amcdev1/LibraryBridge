# LibraryBridge

Proton stores its Windows compatibility data beside each Steam library. On an
NTFS drive that data does not work properly, and on exFAT it cannot work at
all. LibraryBridge moves that data onto a filesystem that can hold it, leaves
a symlink where Steam expects to find it, and never deletes your original.

It also finds installed games that no launcher knows about, such as GOG
installs and standalone Windows games sitting on the same drive, and can add
them to Lutris.

**Status: built and tested, never run on Linux, not ready for release.**
See [the release status](docs/RELEASE-STATUS.md) for what that means. The command line tool,
the Lutris integration and the desktop window all work and are covered by 105
tests. Every filesystem-specific claim is unverified, because NTFS behaviour,
Steam Cloud, the Steam Linux Runtime container and Flatpak sandboxing all need
a real Linux machine. See [what is not done](#what-it-does-not-do).

## The one design rule

**Nothing is ever deleted.** A repair renames your original `compatdata` to
`compatdata.backup` beside itself and leaves it there.
The copy is written under a temporary name and only renamed into place after
every file has been verified by hash.

That rule is why there is no journal, no transaction log and no registry file.
Because no step destroys anything, every state an interruption can leave
behind can be read straight off the disk.

## Build

```bash
cargo build --release --workspace
```

Two binaries land in `target/release`: `librarybridge` and
`librarybridge-gui`. The command line tool has one dependency, `rustix`, used
only to open a directory and then work relative to that descriptor, which is
what stops a path being swapped between the check and the write. The window has
the rest, and builds separately, so a repair or a recovery never needs it.

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

`storage` shows what is kept and how much space it uses. `undo <id>` reverses
the repair. It copies the *current* data back, not the backup, so
saves made since the repair are the ones that survive.

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

## What it does not do

Read [the decision register](docs/DECISIONS.md). It lists everything the
original plans called for that is not in the code, why, and what would bring
it back. The short version:

- **exFAT is refused**, not worked around. It has no symlinks.
- **Nothing is packaged.** No AppImage, no Flatpak, no distribution packages.
- **Nothing has run on Linux**, so no filesystem claim is verified. A library
  whose filesystem cannot be identified is refused rather than repaired, which
  is every non-Linux host.
- **Two reviews found defects.** Fifteen are fixed with regression tests.
  What remains open is listed in the decision register.
- **Timestamps, hard links and sparse files** are not preserved on copy.
- **There is no lock** between two running copies of the tool.
- The Lutris import **has never met a real Lutris**.

## Testing

```bash
cargo test --workspace
```

105 tests. They cover the Valve KeyValues and JSON parsers, SHA-256 against the
published vectors, the copier, the state machine, and the command flows end to
end against synthetic Steam trees. Two safety properties are tested directly:
discovered files are never executed, and symlinks are never followed while
scanning or copying. A third covers the case an external review raised: a
repair interrupted at the moment `compatdata` does not exist, with Steam
starting before recovery and creating its own.

What the tests cannot tell you is anything about NTFS, Proton, Steam Cloud or
Flatpak. Those need the acceptance matrix in the full plan, and a Linux
machine.

## Documentation

- [Decision register](docs/DECISIONS.md): what was dropped, why, and what
  would bring it back. Start here if you are wondering where a feature went.
- [Design notes](docs/DESIGN-NOTES.md): why the code is shaped the way it is.
- [Implementation plan](IMPLEMENTATION_PLAN.md): the design that was built.
- [Lutris integration plan](LUTRIS_INTEGRATION_PLAN.md): the game-finding
  feature, and the one place its priorities were inverted.
- [GUI plan](GUI_PLAN.md): the window, its screen specification and what it
  does not do yet.
- [Release status](docs/RELEASE-STATUS.md): where this stands against the
  release plan, phase by phase.
- [Release tests](docs/RELEASE-TESTS.md): the scenarios a release candidate has
  to pass. None have been run.
- [Long-form plan](IMPLEMENTATION_PLAN_FULL.md): the full treatment of repair
  strategies, data integrity, Steam integration, testing and release gates.
  Most of it is deliberately not built; the decision register says which parts
  and why. It is the reference for a public release.
- [Distribution guide](DISTRIBUTION.md): packaging and release procedure, none
  of which has happened yet.
- [Original draft](proton-bridge.txt): the document this started from.

## Product promise

Keep your games on the external drive. Put Proton's working data on a
filesystem it can use. Preserve everything that was already there, and explain
anything that still needs attention.

This is a filesystem repair tool. A successful repair does not establish that
any particular game works under Proton.
