#!/usr/bin/env python3
"""Generate THIRD_PARTY_NOTICES.txt from the locked dependency graph.

Reads `cargo metadata --locked` and lists every packaged dependency with the
license expression each crate declares. This is attribution of the declared
license; the SPDX expression points at the upstream license, which every
packaged crate is required to ship in its own source distribution.

The dependency list is taken from Cargo.lock (via cargo metadata), the same
lockfile the build uses, so it stays accurate without manual maintenance.
"""

import argparse
import json
import subprocess
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--out", type=Path, default=ROOT / "THIRD_PARTY_NOTICES.txt")
    args = parser.parse_args()
    meta = json.loads(
        subprocess.run(
            ["cargo", "metadata", "--locked", "--format-version", "1"],
            cwd=ROOT,
            check=True,
            capture_output=True,
            text=True,
        ).stdout
    )

    # The workspace itself is included; skip it, only third-party crates.
    workspace_members = set(meta["workspace_members"])
    deps = [
        {"name": p["name"], "version": p["version"], "license": p.get("license")}
        for p in meta["packages"]
        if p["id"] not in workspace_members
    ]
    deps.sort(key=lambda d: (d["name"].lower(), d["version"]))

    lines = [
        "LibraryBridge third-party notices",
        "================================",
        "",
        "LibraryBridge is MIT-licensed (see LICENSE). The following packaged",
        "Rust crates are included in binary releases. Each crate declares the",
        "SPDX license expression shown; the license text is distributed by",
        "the respective crate and can be retrieved from its source or from",
        "crates.io.",
        "",
        "Dependencies",
        "------------",
    ]
    for d in deps:
        lines.append(f"{d['name']} {d['version']} - {d['license'] or 'license not declared'}")
    lines.append("")

    out = args.out
    out.write_text("\n".join(lines), encoding="utf-8")
    print(f"Wrote {out} ({len(deps)} dependencies)")
    return 0


if __name__ == "__main__":
    sys.exit(main())