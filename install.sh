#!/usr/bin/env bash
set -euo pipefail

NAME=ancs-linux
REPO="${ANCS_LINUX_REPO:-schizophrenicautist/ancs-linux}"
BIN_DIR="${BIN_DIR:-$HOME/.local/bin}"
CONFIG_HOME="${XDG_CONFIG_HOME:-$HOME/.config}"
DATA_HOME="${XDG_DATA_HOME:-$HOME/.local/share}"
CACHE_HOME="${XDG_CACHE_HOME:-$HOME/.cache}"
UNIT_DIR="$CONFIG_HOME/systemd/user"
CONFIG_DIR="$CONFIG_HOME/$NAME"
DOC_DIR="$DATA_HOME/doc/$NAME"
NOTIFYRC="$DATA_HOME/knotifications6/$NAME.notifyrc"

usage() {
    cat <<EOF
Usage: install.sh [options]

Installs $NAME for the current user and starts it as a systemd user service.

  (no option)        Install: from the files next to this script if they are a release
                     or a source checkout, otherwise from the latest GitHub release
  --from-source      Build with cargo from the checkout this script is in
  --version TAG      Download release TAG (e.g. v1.0.0) instead of the latest one
  --uninstall        Stop and remove $NAME, keeping its config and icon cache
  --purge            Like --uninstall, and also remove the config and icon cache
  -h, --help         Show this help

Environment: BIN_DIR (default ~/.local/bin), ANCS_LINUX_REPO (default $REPO)
EOF
}

say() { printf '\033[1m==>\033[0m %s\n' "$*" >&2; }
warn() { printf '\033[33mwarning:\033[0m %s\n' "$*" >&2; }
die() { printf '\033[31merror:\033[0m %s\n' "$*" >&2; exit 1; }

script_dir() {
    local source="${BASH_SOURCE[0]:-}"
    if [[ -n "$source" && -f "$source" ]]; then
        cd "$(dirname "$source")" && pwd
    fi
}

user_systemd() {
    systemctl --user show-environment >/dev/null 2>&1
}

arch() {
    case "$(uname -m)" in
        x86_64 | amd64) echo x86_64 ;;
        aarch64 | arm64) echo aarch64 ;;
        *) die "no prebuilt binary for $(uname -m); install from source: ./install.sh --from-source" ;;
    esac
}

download() {
    local tag="$1" work="$2" asset url
    command -v curl >/dev/null || die "curl is required to download a release"
    asset="$NAME-$(arch)-linux.tar.gz"
    if [[ -n "$tag" ]]; then
        url="https://github.com/$REPO/releases/download/$tag/$asset"
    else
        url="https://github.com/$REPO/releases/latest/download/$asset"
    fi
    say "Downloading $url"
    curl -fsSL "$url" -o "$work/$asset" || die "download failed: $url"
    curl -fsSL "$url.sha256" -o "$work/$asset.sha256" || die "download failed: $url.sha256"
    (cd "$work" && sha256sum --check --quiet "$asset.sha256") || die "checksum mismatch for $asset"
    tar -xzf "$work/$asset" -C "$work"
    echo "$work/$NAME-$(arch)-linux"
}

build() {
    local src="$1"
    command -v cargo >/dev/null || die "cargo is required to build from source (https://rustup.rs)"
    say "Building $NAME"
    cargo build --release --locked --manifest-path "$src/Cargo.toml" >&2
    echo "$src/target/release/$NAME"
}

install_files() {
    local binary="$1" src="$2"
    say "Installing $BIN_DIR/$NAME"
    install -Dm755 "$binary" "$BIN_DIR/$NAME"
    sed -e "s|%h/.local/bin/$NAME|$BIN_DIR/$NAME|" -e "s|%h/.local/share/doc/$NAME|$DOC_DIR|" \
        "$src/packaging/$NAME.service" >"$src/.unit.tmp"
    install -Dm644 "$src/.unit.tmp" "$UNIT_DIR/$NAME.service"
    rm -f "$src/.unit.tmp"
    install -Dm644 "$src/packaging/$NAME.notifyrc" "$NOTIFYRC"
    install -Dm644 "$src/packaging/config.toml" "$DOC_DIR/config.toml"
    install -Dm644 "$src/README.md" "$DOC_DIR/README.md"
    if [[ -e "$CONFIG_DIR/config.toml" ]]; then
        say "Keeping existing $CONFIG_DIR/config.toml"
    else
        install -Dm644 "$src/packaging/config.toml" "$CONFIG_DIR/config.toml"
        say "Created $CONFIG_DIR/config.toml"
    fi
}

start_service() {
    if ! user_systemd; then
        warn "no systemd user session; start it later with: systemctl --user enable --now $NAME"
        return
    fi
    systemctl --user daemon-reload
    systemctl --user enable "$NAME.service" >/dev/null 2>&1
    systemctl --user restart "$NAME.service"
    say "Started $NAME.service"
}

check_system() {
    [[ "$(uname -s)" == Linux ]] || die "$NAME runs on Linux only"
    command -v systemctl >/dev/null || die "systemd is required"
    if ! systemctl is-active --quiet bluetooth 2>/dev/null; then
        warn "bluetooth.service is not running: sudo systemctl enable --now bluetooth"
    fi
    case ":$PATH:" in
        *":$BIN_DIR:"*) ;;
        *) warn "$BIN_DIR is not on PATH; the service works regardless" ;;
    esac
}

do_install() {
    local mode="$1" tag="$2" here src binary
    check_system
    here="$(script_dir)"
    if [[ "$mode" == source ]]; then
        [[ -n "$here" && -f "$here/Cargo.toml" ]] || die "--from-source must be run from a source checkout"
        src="$here"
        binary="$(build "$src")"
    elif [[ -z "$tag" && -n "$here" && -x "$here/$NAME" && -d "$here/packaging" ]]; then
        src="$here"
        binary="$here/$NAME"
    elif [[ -z "$tag" && -n "$here" && -f "$here/Cargo.toml" ]]; then
        src="$here"
        binary="$(build "$src")"
    else
        WORK="$(mktemp -d)"
        trap 'rm -rf "$WORK"' EXIT
        src="$(download "$tag" "$WORK")"
        binary="$src/$NAME"
    fi
    install_files "$binary" "$src"
    start_service
    cat <<EOF

$NAME is installed. To pair your iPhone:
  1. bluetoothctl pairable on
  2. On the iPhone, open Settings > Bluetooth and tap this desktop.
  3. Confirm the PIN on both devices.
  4. In the desktop's entry on the iPhone, turn on "Share System Notifications".

Settings: $CONFIG_DIR/config.toml (then: systemctl --user restart $NAME)
Logs:     journalctl --user -u $NAME -f
EOF
}

do_uninstall() {
    local purge="$1"
    if user_systemd; then
        systemctl --user disable --now "$NAME.service" >/dev/null 2>&1 || true
    fi
    rm -f "$BIN_DIR/$NAME" "$UNIT_DIR/$NAME.service" "$NOTIFYRC"
    rm -rf "$DOC_DIR"
    if user_systemd; then
        systemctl --user daemon-reload
    fi
    if [[ "$purge" == yes ]]; then
        rm -rf "$CONFIG_DIR" "$CACHE_HOME/$NAME"
        say "Removed $NAME, its config and its icon cache"
    else
        say "Removed $NAME; kept $CONFIG_DIR and $CACHE_HOME/$NAME (--purge removes them)"
    fi
}

main() {
    local action=install mode=auto tag=""
    while [[ $# -gt 0 ]]; do
        case "$1" in
            --from-source) mode=source ;;
            --version)
                [[ $# -ge 2 ]] || die "--version needs a tag, e.g. v1.0.0"
                tag="$2"
                shift
                ;;
            --uninstall) action=uninstall ;;
            --purge) action=purge ;;
            -h | --help) usage; exit 0 ;;
            *) usage >&2; die "unknown option: $1" ;;
        esac
        shift
    done
    [[ "$(id -u)" -ne 0 ]] || die "run as your user, not root: the service runs in your desktop session"
    case "$action" in
        install) do_install "$mode" "$tag" ;;
        uninstall) do_uninstall no ;;
        purge) do_uninstall yes ;;
    esac
}

main "$@"
