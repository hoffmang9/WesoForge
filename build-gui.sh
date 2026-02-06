#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
cd "$ROOT"

UNAME="$(uname -s)"
workspace_version() {
  awk '
    /^\[workspace\.package\]/ { in_pkg = 1; next }
    in_pkg && /^\[/ { in_pkg = 0 }
    in_pkg && match($0, /^version[[:space:]]*=[[:space:]]*"/) {
      rest = substr($0, RSTART + RLENGTH)
      if (match(rest, /[^"]*/)) {
        print substr(rest, RSTART, RLENGTH)
        exit
      }
    }
  ' Cargo.toml
}

platform_arch() {
  local arch
  arch="$(uname -m)"
  case "$arch" in
    x86_64|amd64) echo "amd64" ;;
    aarch64|arm64) echo "arm64" ;;
    *) echo "$arch" ;;
  esac
}

TARGET_DIR="${CARGO_TARGET_DIR:-$ROOT/target}"
mkdir -p "$TARGET_DIR"
if ! : >"$TARGET_DIR/.wesoforge-write-test" 2>/dev/null; then
  echo "warning: CARGO_TARGET_DIR is not writable ($TARGET_DIR); using $ROOT/target instead" >&2
  TARGET_DIR="$ROOT/target"
  mkdir -p "$TARGET_DIR"
fi
rm -f "$TARGET_DIR/.wesoforge-write-test" >/dev/null 2>&1 || true
export CARGO_TARGET_DIR="$TARGET_DIR"

if ! command -v pnpm >/dev/null 2>&1; then
  echo "error: pnpm not found (needed to build the Svelte frontend)." >&2
  exit 1
fi

if [[ ! -d ui/node_modules ]]; then
  pnpm -C ui install
fi

if ! cargo tauri --help >/dev/null 2>&1; then
  cat >&2 <<'EOF'
error: `cargo tauri` not found.

Install the Tauri CLI (cargo subcommand) first, for example:
  cargo install tauri-cli
EOF
  exit 1
fi

DIST_DIR="${DIST_DIR:-$ROOT/dist}"
mkdir -p "$DIST_DIR"

SUPPORT_DEVTOOLS="${SUPPORT_DEVTOOLS:-0}"
FEATURES="prod-backend"
OUT_PREFIX="WesoForge-gui"
if [[ "$SUPPORT_DEVTOOLS" == "1" ]]; then
  FEATURES="$FEATURES,support-devtools"
  OUT_PREFIX="WesoForge-gui-support"
fi

VERSION="$(workspace_version)"
if [[ -z "${VERSION:-}" ]]; then
  echo "error: failed to determine workspace version from Cargo.toml" >&2
  exit 1
fi
ARCH="$(platform_arch)"

CARGO_ARGS=()
if [[ "${CARGO_LOCKED:-0}" == "1" ]]; then
  CARGO_ARGS+=(--locked)
fi
if [[ "${CARGO_OFFLINE:-0}" == "1" ]]; then
  CARGO_ARGS+=(--offline)
fi

fix_linux_appimage_diricon() {
  local appimage="$1"
  if [[ ! -f "$appimage" ]]; then
    return 0
  fi
  if ! command -v mksquashfs >/dev/null 2>&1; then
    echo "warning: mksquashfs not found; skipping AppImage icon metadata normalization." >&2
    return 0
  fi

  local workdir
  workdir="$(mktemp -d)"
  if ! (cd "$workdir" && APPIMAGE_EXTRACT_AND_RUN=1 "$appimage" --appimage-extract >/dev/null 2>&1); then
    echo "warning: failed to extract AppImage for icon normalization; keeping original bundle." >&2
    rm -rf "$workdir"
    return 0
  fi

  local diricon="$workdir/squashfs-root/.DirIcon"
  local diricon_target=""
  if [[ -L "$diricon" ]]; then
    diricon_target="$(readlink "$diricon" || true)"
  fi
  if [[ -z "$diricon_target" || "$diricon_target" != /* ]]; then
    rm -rf "$workdir"
    return 0
  fi

  local icon_name=""
  if [[ -f "$workdir/squashfs-root/WesoForge.png" ]]; then
    icon_name="WesoForge.png"
  elif [[ -f "$workdir/squashfs-root/bbr-client-gui.png" ]]; then
    icon_name="bbr-client-gui.png"
  else
    echo "warning: AppImage extracted but no icon payload found; skipping icon normalization." >&2
    rm -rf "$workdir"
    return 0
  fi

  ln -snf "$icon_name" "$diricon"

  local offset=""
  offset="$(APPIMAGE_EXTRACT_AND_RUN=1 "$appimage" --appimage-offset 2>/dev/null || true)"
  if [[ ! "$offset" =~ ^[0-9]+$ ]]; then
    echo "warning: failed to read AppImage runtime offset; keeping original bundle." >&2
    rm -rf "$workdir"
    return 0
  fi

  dd if="$appimage" of="$workdir/runtime" bs=1 count="$offset" status=none
  mksquashfs "$workdir/squashfs-root" "$workdir/new.squashfs" -root-owned -noappend -quiet >/dev/null
  cat "$workdir/runtime" "$workdir/new.squashfs" > "$workdir/fixed.AppImage"
  chmod +x "$workdir/fixed.AppImage"
  cp -f "$workdir/fixed.AppImage" "$appimage"
  rm -rf "$workdir"
}

if [[ "$UNAME" == "Linux" ]]; then
  if [[ "${BBR_SKIP_CARGO_BUILD:-0}" != "1" ]]; then
    echo "Building WesoForge GUI AppImage (features: $FEATURES)..." >&2
    (
      cd crates/client-gui
      export NO_STRIP=1
      if [[ "${#CARGO_ARGS[@]}" -gt 0 ]]; then
        cargo tauri build --features "$FEATURES" --bundles appimage -- "${CARGO_ARGS[@]}"
      else
        cargo tauri build --features "$FEATURES" --bundles appimage
      fi
    )
  fi
  APPIMAGE_DIR="$TARGET_DIR/release/bundle/appimage"
  APPIMAGE_SRC="$(ls -1t "$APPIMAGE_DIR"/*.AppImage 2>/dev/null | head -n 1 || true)"
  if [[ -z "$APPIMAGE_SRC" ]]; then
    echo "error: no AppImage found under: $APPIMAGE_DIR" >&2
    exit 1
  fi
  fix_linux_appimage_diricon "$APPIMAGE_SRC"
  APPIMAGE_DST="$DIST_DIR/${OUT_PREFIX}_Linux_${VERSION}_${ARCH}.AppImage"
  install -m 0755 "$APPIMAGE_SRC" "$APPIMAGE_DST"
  echo "Wrote: $APPIMAGE_DST" >&2
elif [[ "$UNAME" == "Darwin" ]]; then
  if [[ "${BBR_SKIP_CARGO_BUILD:-0}" != "1" ]]; then
    echo "Building WesoForge GUI (macOS DMG, features: $FEATURES)..." >&2
    (
      cd crates/client-gui
      if [[ "${#CARGO_ARGS[@]}" -gt 0 ]]; then
        cargo tauri build --features "$FEATURES" --bundles dmg -- "${CARGO_ARGS[@]}"
      else
        cargo tauri build --features "$FEATURES" --bundles dmg
      fi
    )
  fi
  DMG_DIR="$TARGET_DIR/release/bundle/dmg"
  DMG_SRC="$(ls -1t "$DMG_DIR"/*.dmg 2>/dev/null | head -n 1 || true)"
  if [[ -z "$DMG_SRC" ]]; then
    echo "error: no DMG found under: $DMG_DIR" >&2
    exit 1
  fi
  DMG_DST="$DIST_DIR/${OUT_PREFIX}_macOS_${VERSION}_${ARCH}.dmg"
  cp "$DMG_SRC" "$DMG_DST"
  echo "Wrote: $DMG_DST" >&2
else
  echo "error: GUI build is only supported on Linux and macOS (got: $UNAME)" >&2
  exit 1
fi
