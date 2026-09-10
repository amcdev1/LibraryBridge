#!/usr/bin/env python3
"""Generate a CycloneDX SBOM (sbom.cdx.json) from the locked dependency graph.

Reads `cargo metadata --locked` and emits a CycloneDX 1.5 document listing
every packaged third-party crate with its package URL (purl). The component
set comes from Cargo.lock via cargo metadata, matching exactly what the build
links.

Usage: make-sbom.py [output.json]
Default output: sbom.cdx.json at the repository root.
"""

import argparse
import json
import subprocess
import sys
import uuid
from datetime import datetime, timezone
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent


def purl(name: str, version: str) -> str:
    return f"pkg:cargo/{name}@{version}"


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--out", type=Path, default=ROOT / "sbom.cdx.json")
    args = parser.parse_args()
    out_path = args.out
    meta = json.loads(
        subprocess.run(
            ["cargo", "metadata", "--locked", "--format-version", "1"],
            cwd=ROOT,
            check=True,
            capture_output=True,
            text=True,
        ).stdout
    )

    workspace_members = set(meta["workspace_members"])
    workspace_eps = {
        p["id"]: p["version"]
        for p in meta["packages"]
        if p["id"] in workspace_members and "version" in p
    }

    components = []
    for p in meta["packages"]:
        if p["id"] in workspace_members:
            continue
        components.append(
            {
                "type": "library",
                "name": p["name"],
                "version": p["version"],
                "purl": purl(p["name"], p["version"]),
                "licenses": (
                    [{"license": {"name": p["license"]}}]
                    if p.get("license")
                    else []
                ),
            }
        )
    components.sort(key=lambda c: (c["name"].lower(), c["version"]))

    doc = {
        "bomFormat": "CycloneDX",
        "specVersion": "1.5",
        "serialNumber": f"urn:uuid:{uuid.uuid4()}",
        "version": 1,
        "metadata": {
            "timestamp": datetime.now(timezone.utc).isoformat(),
            "tools": [{"name": "make-sbom.py", "vendor": "LibraryBridge"}],
            "component": {
                "type": "application",
                "name": "LibraryBridge",
                "version": meta["packages"][0]["version"],
                "bom-ref": "librarybridge",
            },
        },
        "components": components,
    }

    out_path.write_text(json.dumps(doc, indent=2) + "\n", encoding="utf-8")
    print(f"Wrote {out_path} ({len(components)} components)")
    return 0


if __name__ == "__main__":
    sys.exit(main())