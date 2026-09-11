#!/usr/bin/env bash
#
# Produces the versioned, distributable release assets under dist/.
#
# Usage:
#   scripts/package-release.sh [--no-appimage]
#
# Reads the version from Cargo.toml unless VERSION is set in the environment.
# Builds the release binaries (scripts/build-release.sh), assembles each
# artifact in dist/, generates checksums, an SBOM and third-party notices,
# then writes SHA256SUMS over all artifacts.
#
# The AppImage requires appimagetool. It is used when found on PATH or when
# $APPIMAGE_TOOL points at an appimagetool-compatible image. In CI the release
# workflow downloads a pinned appimagetool. Use --no-appimage to skip it
# locally (the desktop tarball is the FUSE-independent fallback anyway).
#
# Artifacts produced in dist/:
#   LibraryBridge-<version>-x86_64.AppImage
#   librarybridge-desktop-<version>-linux-x86_64.tar.gz
#   librarybridge-cli-<version>-linux-x86_64.tar.gz
#   librarybridge-<version>-source.tar.gz
#   SHA256SUMS, sbom.cdx.json, THIRD_PARTY_NOTICES.txt, compatibility-<version>.md

set -euo pipefail
cd "$(dirname "$0")/.."

VERSION="${VERSION:-$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -1)}"
[ -n "$VERSION" ] || { echo "could not read version from Cargo.toml" >&2; exit 1; }
ARCH="x86_64"
GIT_COMMIT="$(git rev-parse --short HEAD)"

echo "==> Packaging LibraryBridge $VERSION (commit $GIT_COMMIT)"

# --no-appimage to skip the AppImage build (see header).
SKIP_APPIMAGE=0
case "${1:-}" in
  --no-appimage) SKIP_APPIMAGE=1 ;;
  "") ;;
  *) echo "unknown argument: $1" >&2; exit 1 ;;
esac

rm -rf dist
mkdir -p dist

echo "==> Building release binaries"
scripts/build-release.sh

echo "==> Generating third-party notices and SBOM"
python3 scripts/make-notices.py --out dist/THIRD_PARTY_NOTICES.txt
python3 scripts/make-sbom.py --out dist/sbom.cdx.json

echo "==> Compatibility notes"
cat > "dist/compatibility-$VERSION.md" <<EOF
# LibraryBridge $VERSION compatibility

- Version: $VERSION
- Commit: $GIT_COMMIT
- Architecture: x86_64
- Minimum host baseline: Ubuntu 24.04 userspace (glibc 2.39)
- AppImage requires a FUSE runtime; the desktop tarball is the FUSE-independent fallback.
- The CLI tarball has no display-server dependency.

See the README for the supported filesystem, Steam and Lutris matrix.
EOF

echo "==> Source archive"
git archive --format=tar.gz --prefix="librarybridge-$VERSION/" -o "dist/librarybridge-$VERSION-source.tar.gz" HEAD

echo "==> CLI tarball"
CLI_STAGE="dist/_stage-cli"
mkdir -p "$CLI_STAGE"
install -m0755 target/release/librarybridge "$CLI_STAGE/librarybridge"
install -m0644 LICENSE "$CLI_STAGE/LICENSE"
install -m0644 docs/recovery-guide.txt "$CLI_STAGE/RECOVERY.txt"
install -m0644 dist/THIRD_PARTY_NOTICES.txt "$CLI_STAGE/THIRD_PARTY_NOTICES.txt"
printf '%s\n' 'See RECOVERY.txt for how to recover a library without this tool.' > "$CLI_STAGE/README.txt"
tar -C "$CLI_STAGE" -czf "dist/librarybridge-cli-$VERSION-linux-$ARCH.tar.gz" .

echo "==> Desktop tarball (FUSE-independent AppDir)"
APP_DIR="dist/_stage-appdir/LibraryBridge.AppDir"
mkdir -p "$APP_DIR/usr/bin" "$APP_DIR/usr/share/applications" "$APP_DIR/usr/share/icons/hicolor" "$APP_DIR/usr/share/metainfo"
cp -r packaging/linux/usr/share/applications/librarybridge.desktop "$APP_DIR/usr/share/applications/"
cp -r packaging/linux/usr/share/icons/hicolor/. "$APP_DIR/usr/share/icons/hicolor/"
cp -r packaging/linux/usr/share/metainfo/. "$APP_DIR/usr/share/metainfo/"
install -m0755 target/release/librarybridge-gui "$APP_DIR/usr/bin/librarybridge-gui"
install -m0755 target/release/librarybridge "$APP_DIR/usr/bin/librarybridge"
cp packaging/AppRun "$APP_DIR/AppRun"
chmod +x "$APP_DIR/AppRun"

# appimagetool expects a top-level .desktop file. Copy and fix the Exec to
# point inside the AppDir through AppRun, since the binary is at usr/bin/.
cp packaging/linux/usr/share/applications/librarybridge.desktop "$APP_DIR/librarybridge.desktop"
# appimagetool requires the desktop Icon= to resolve inside the AppDir; it
# looks at the AppDir root and at usr/share/icons. Provide the 256x256 PNG at
# the root (also used as .DirIcon), matching the Icon= name.
cp "$APP_DIR/usr/share/icons/hicolor/256x256/apps/librarybridge.png" "$APP_DIR/librarybridge.png"
cp "$APP_DIR/librarybridge.png" "$APP_DIR/.DirIcon"

tar -C "dist/_stage-appdir" -czf "dist/librarybridge-desktop-$VERSION-linux-$ARCH.tar.gz" LibraryBridge.AppDir

echo "==> AppImage"
if [ "$SKIP_APPIMAGE" -eq 1 ]; then
  echo "    skipped (--no-appimage). Desktop tarball is the FUSE-independent fallback."
elif [ -n "${APPIMAGE_TOOL:-}" ] || command -v appimagetool >/dev/null 2>&1; then
  # APPIMAGE_EXTRACT_AND_RUN lets a FUSE-less CI run an appimagetool that is
  # itself distributed as an AppImage. Harmless for a native binary.
  export APPIMAGE_EXTRACT_AND_RUN=1
  TOOL="${APPIMAGE_TOOL:-appimagetool}"
  (cd "dist/_stage-appdir" && "$TOOL" "LibraryBridge.AppDir") >/dev/null
  mv dist/_stage-appdir/LibraryBridge-*x86_64.AppImage "dist/LibraryBridge-$VERSION-$ARCH.AppImage"
  # The desktop tarball and the AppImage share the same AppDir; keep the
  # tarball as the FUSE-independent fallback.
else
  echo "    appimagetool not found; either install it, export APPIMAGE_TOOL, or pass --no-appimage." >&2
  exit 1
fi

echo "==> Checksums"
rm -rf dist/_stage-cli dist/_stage-appdir  # staging dirs are not artifacts
(cd dist && sha256sum -- * > SHA256SUMS)
[ -s dist/SHA256SUMS ] || { echo "SHA256SUMS empty" >&2; exit 1; }

echo ""
echo "==> dist/ artifacts"
ls -lh dist/*
echo ""
echo "Verify with: scripts/check-release.sh"
