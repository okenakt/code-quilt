#!/bin/sh
# Installs CodeQuilt into ~/.codequilt/bin, or removes it with --uninstall. See docs/features.md.
#   curl -fsSL https://github.com/okenakt/code-quilt/releases/latest/download/install.sh | sh -s -- [options]
set -eu

REPO=okenakt/code-quilt
ASSET=codequilt-x86_64-unknown-linux-gnu.tar.gz
ROOT="$HOME/.codequilt"
BIN="$ROOT/bin"
BINS="cq cq-orchestrator cq-board"
ENV_LINE='. "$HOME/.codequilt/env"'
RC_FILES="$HOME/.profile $HOME/.bashrc $HOME/.zshenv"
DESKTOP_FILE="$HOME/.local/share/applications/codequilt.desktop"
ICON_FILE="$HOME/.local/share/icons/hicolor/128x128/apps/codequilt.png"

usage() {
    cat <<EOF
usage: install.sh [--version vX.Y.Z] [--archive FILE] [--no-modify-path] [--desktop]
       install.sh --uninstall

  --version VER     install that release instead of the latest
  --archive FILE    install from a local archive instead of downloading
  --no-modify-path  do not add ~/.codequilt/bin to PATH
  --desktop         add CodeQuilt to the application launcher
  --uninstall       remove what this script added (sessions and settings in ~/.codequilt are kept)
EOF
}

say() { printf '%s\n' "$*"; }
die() {
    printf 'error: %s\n' "$*" >&2
    exit 1
}

version=latest archive="" modify_path=1 desktop=0 uninstall=0
while [ $# -gt 0 ]; do
    case "$1" in
        --version) version="${2:?--version needs a value}"; shift ;;
        --archive) archive="${2:?--archive needs a file}"; shift ;;
        --no-modify-path) modify_path=0 ;;
        --desktop) desktop=1 ;;
        --uninstall) uninstall=1 ;;
        -h | --help) usage; exit 0 ;;
        *) usage >&2; exit 2 ;;
    esac
    shift
done

stop_daemon() {
    "$BIN/cq" daemon stop || say "warning: could not stop the daemon"
}

download() {
    if [ "$version" = latest ]; then
        base="https://github.com/$REPO/releases/latest/download"
    else
        base="https://github.com/$REPO/releases/download/$version"
    fi
    say "downloading $base/$ASSET"
    curl -fsSL -o "$tmp/$ASSET" "$base/$ASSET"
    curl -fsSL -o "$tmp/SHA256SUMS" "$base/SHA256SUMS"
    (cd "$tmp" && grep " $ASSET\$" SHA256SUMS | sha256sum -c --quiet -) || die "checksum mismatch for $ASSET"
}

# Sources ~/.codequilt/env from the login and interactive shell files (zsh only when it is used).
add_to_path() {
    printf '%s\n' 'case ":$PATH:" in *":$HOME/.codequilt/bin:"*) ;; *) PATH="$HOME/.codequilt/bin:$PATH" ;; esac' >"$ROOT/env"
    for rc in $RC_FILES; do
        if [ "$rc" = "$HOME/.zshenv" ] && [ ! -e "$rc" ] && ! command -v zsh >/dev/null 2>&1; then
            continue
        fi
        if ! grep -qxF "$ENV_LINE" "$rc" 2>/dev/null; then
            if [ -s "$rc" ] && [ -n "$(tail -c 1 "$rc")" ]; then echo >>"$rc"; fi
            say "$ENV_LINE" >>"$rc"
        fi
    done
}

remove_from_path() {
    rm -f "$ROOT/env"
    for rc in $RC_FILES; do
        if [ -f "$rc" ] && grep -qxF "$ENV_LINE" "$rc"; then
            grep -vxF "$ENV_LINE" "$rc" >"$rc.cq-tmp" || true
            cat "$rc.cq-tmp" >"$rc"
            rm -f "$rc.cq-tmp"
        fi
    done
}

add_desktop_entry() {
    mkdir -p "$(dirname "$DESKTOP_FILE")" "$(dirname "$ICON_FILE")"
    cp "$tmp/x/codequilt.png" "$ICON_FILE"
    cat >"$DESKTOP_FILE" <<EOF
[Desktop Entry]
Type=Application
Name=CodeQuilt
Exec=$BIN/cq-board
Icon=codequilt
Categories=Development;
EOF
}

uninstall() {
    if [ -x "$BIN/cq" ]; then stop_daemon; fi
    for b in $BINS; do rm -f "$BIN/$b"; done
    rmdir "$BIN" 2>/dev/null || true
    remove_from_path
    rm -f "$DESKTOP_FILE" "$ICON_FILE"
    say "removed CodeQuilt; sessions and settings are kept in $ROOT"
}

install_release() {
    [ "$(uname -sm)" = "Linux x86_64" ] || die "this script supports Linux x86_64 only (found $(uname -sm))"
    tmp="$(mktemp -d)"
    trap 'rm -rf "$tmp"' EXIT
    if [ -n "$archive" ]; then
        cp "$archive" "$tmp/$ASSET"
    else
        download
    fi
    mkdir "$tmp/x"
    tar -xzf "$tmp/$ASSET" -C "$tmp/x"
    for b in $BINS; do [ -f "$tmp/x/$b" ] || die "$ASSET has no $b"; done

    # The cq that started the daemon speaks its protocol version, so it stops it.
    updating=0
    if [ -x "$BIN/cq" ]; then
        updating=1
        stop_daemon
    fi
    mkdir -p "$BIN"
    # Renaming over the old file leaves running sessions on it until they exit.
    for b in $BINS; do
        install -m 755 "$tmp/x/$b" "$BIN/.$b.new"
        mv -f "$BIN/.$b.new" "$BIN/$b"
    done
    say "installed $("$BIN/cq" --version) into $BIN"

    if [ "$modify_path" = 1 ]; then add_to_path; fi
    if [ "$desktop" = 1 ]; then add_desktop_entry; fi

    missing="$(ldd "$BIN/cq-board" 2>/dev/null | awk '/not found/ {print $1}')"
    if [ -n "$missing" ]; then
        say "the board needs libraries that are not installed:" $missing
        say "  on Ubuntu: sudo apt install libwebkit2gtk-4.1-0 libgtk-3-0"
    fi
    if [ "$updating" = 1 ]; then
        "$BIN/cq" daemon restart || say "warning: could not restart the daemon"
    fi
    for p in $(pgrep -x cq-board || true); do
        case "$(readlink "/proc/$p/exe" 2>/dev/null)" in
            "$BIN/cq-board"*) say "restart the board to use the new version"; break ;;
        esac
    done
    case ":$PATH:" in
        *":$BIN:"*) ;;
        *) if [ "$modify_path" = 1 ]; then say "open a new shell, or run: . \"\$HOME/.codequilt/env\""; fi ;;
    esac
}

if [ "$uninstall" = 1 ]; then
    uninstall
else
    install_release
fi
