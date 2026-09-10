#!/usr/bin/env bash
#
# Validates the artifacts in dist/ produced by scripts/package-release.sh.
# Verifies presence and integrity of the expected assets, the SHA256SUMS
# file, the SBOM and notices, and smoke-tests the CLI binary.
#
# Usage: scripts/check-release.sh [--require-appimage]
# One optional argument forces the AppImage to be present and valid.

set -uo pipefail
cd "$(dirname "$0")/.."

VERSION="$VERSION"
[ -n "${VERSION:-}" ] || VERSION="$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -1)"
REQUIRE_APPIMAGE=0
case "${1:-}" in
  --require-appimage) REQUIRE_APPIMAGE=1 ;;
  "") ;;
  *) echo "unknown argument: $1" >&2; exit 1 ;;
esac

[ -d dist ] || { echo "no dist/ directory; run scripts/package-release.sh first" >&2; exit 1; }
echo "==> checking release v$VERSION"

fail=0
check() { # msg, cond (shell exit status: 0 = pass)
  if [ "$2" = "0" ]; then echo "  ok: $1"; else echo "  FAIL: $1"; fail=1; fi
}

# Expected artifacts
for f in \
  "librarybridge-cli-$VERSION-linux-x86_64.tar.gz" \
  "librarybridge-desktop-$VERSION-linux-x86_64.tar.gz" \
  "librarybridge-$VERSION-source.tar.gz" \
  "SHA256SUMS" "sbom.cdx.json" "THIRD_PARTY_NOTICES.txt" "compatibility-$VERSION.md"; do
  [ -f "dist/$f" ]; check "asset $f present" "$?"
done
if [ "$REQUIRE_APPIMAGE" -eq 1 ]; then
  [ -f "dist/LibraryBridge-$VERSION-x86_64.AppImage" ]; check "AppImage present (required)" "$?"
fi

# SHA256SUMS verifies itself and every listed artifact
(cd dist && sha256sum -c SHA256SUMS >/dev/null 2>&1); check "SHA256SUMS verifies all artifacts" "$?"
[ -s dist/SHA256SUMS ]; check "SHA256SUMS non-empty" "$?"

# Tarballs are readable gzip and contain the expected top entries
for f in \
  "librarybridge-cli-$VERSION-linux-x86_64.tar.gz" \
  "librarybridge-desktop-$VERSION-linux-x86_64.tar.gz" \
  "librarybridge-$VERSION-source.tar.gz"; do
  tar -tzf "dist/$f" >/dev/null 2>&1; check "$f is a readable tarball" "$?"
done

# CLI tarball smoke test: extract the binary and run --version
SMOKE="dist/.check-smoke"
rm -rf "$SMOKE"; mkdir -p "$SMOKE"
tar -xzf "dist/librarybridge-cli-$VERSION-linux-x86_64.tar.gz" -C "$SMOKE"
[ -x "$SMOKE/librarybridge" ]; check "CLI binary present and executable" "$?"
if [ -x "$SMOKE/librarybridge" ]; then
  OUT="$("$SMOKE/librarybridge" --version 2>&1)"; check "CLI runs --version ($OUT)" "$?"
fi
[ -f "$SMOKE/RECOVERY.txt" ]; check "CLI recovery guide present" "$?"
[ -f "$SMOKE/THIRD_PARTY_NOTICES.txt" ]; check "CLI third-party notices present" "$?"
rm -rf "$SMOKE"

# Desktop tarball contains AppRun and the AppDir
DESK="dist/.check-desktop"
rm -rf "$DESK"; mkdir -p "$DESK"
tar -xzf "dist/librarybridge-desktop-$VERSION-linux-x86_64.tar.gz" -C "$DESK"
[ -f "$DESK/LibraryBridge.AppDir/AppRun" ] && [ -x "$DESK/LibraryBridge.AppDir/AppRun" ]; check "AppRun present and executable" "$?"
[ -x "$DESK/LibraryBridge.AppDir/usr/bin/librarybridge-gui" ]; check "GUI binary present" "$?"
[ -f "$DESK/LibraryBridge.AppDir/usr/share/applications/librarybridge.desktop" ]; check "desktop entry present" "$?"
[ -f "$DESK/LibraryBridge.AppDir/usr/share/metainfo/io.github.amcdev7.LibraryBridge.metainfo.xml" ]; check "AppStream metainfo present" "$?"
rm -rf "$DESK"

# Source tarball contains the tree
SRC="dist/.check-src"
rm -rf "$SRC"; mkdir -p "$SRC"
tar -xzf "dist/librarybridge-$VERSION-source.tar.gz" -C "$SRC"
# Source archive carries a top-level librarybridge-<version>/ directory.
[ -f "$SRC/librarybridge-$VERSION/README.md" ] \
  && [ -f "$SRC/librarybridge-$VERSION/Cargo.toml" ] \
  && [ -f "$SRC/librarybridge-$VERSION/LICENSE" ] \
  && [ -f "$SRC/librarybridge-$VERSION/Cargo.lock" ]
check "source tarball has README/Cargo.toml/LICENSE/Cargo.lock" "$?"
rm -rf "$SRC"

# SBOM is valid CycloneDX JSON
python3 - "$VERSION" <<'PY' || fail=1
import json, sys
v = sys.argv[1]
doc = json.load(open("dist/sbom.cdx.json"))
assert doc.get("bomFormat") == "CycloneDX", "bomFormat"
assert doc["components"], "components empty"
print("  ok: SBOM is CycloneDX with", len(doc["components"]), "components")
PY

# Notices mention the project and are non-empty
[ -s dist/THIRD_PARTY_NOTICES.txt ]; check "notices non-empty" "$?"
grep -qi "LibraryBridge" dist/THIRD_PARTY_NOTICES.txt; check "notices reference LibraryBridge" "$?"

# Compatibility notes non-empty
[ -s "dist/compatibility-$VERSION.md" ]; check "compatibility notes non-empty" "$?"

if [ "$fail" -eq 1 ]; then
  echo "==> RELEASE CHECK FAILED"
  exit 1
fi
echo "==> release check passed (v$VERSION)"