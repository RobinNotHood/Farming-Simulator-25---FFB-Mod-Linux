#!/usr/bin/env bash
# FS25 FFB Enhancer - installer for CachyOS / Arch / any modern distro.
#
# Usage:
#   ./packaging/install.sh               # full install (daemon + mod + udev + service)
#   ./packaging/install.sh --mod-only    # just (re)copy the Lua mod into FS25
#   ./packaging/install.sh --no-service  # skip systemd unit
#   ./packaging/install.sh --uninstall   # reverse everything
#
# Safe to re-run. Always prompts before sudo steps.

set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
MOD_SRC="$REPO_ROOT/mod/FS25_FFBEnhancer"
DAEMON_DIR="$REPO_ROOT/daemon"
FS25_APPID=2300320
PREFIX="${PREFIX:-/usr/local}"

mode=full
for arg in "$@"; do
    case "$arg" in
        --mod-only)     mode=mod ;;
        --no-service)   mode=no_service ;;
        --uninstall)    mode=uninstall ;;
        --help|-h)
            grep '^#' "$0" | sed 's/^# \{0,1\}//'
            exit 0 ;;
        *) echo "unknown arg: $arg" >&2; exit 2 ;;
    esac
done

log()  { printf '\033[1;34m==>\033[0m %s\n' "$*"; }
warn() { printf '\033[1;33m!!\033[0m  %s\n' "$*" >&2; }
fail() { printf '\033[1;31mXX\033[0m  %s\n' "$*" >&2; exit 1; }

ask_sudo() {
    if [[ $EUID -ne 0 ]]; then
        sudo -v || fail "sudo required for $1"
    fi
}

find_fs25_mods_dir() {
    local roots=(
        "$HOME/.local/share/Steam/steamapps/compatdata/$FS25_APPID"
        "$HOME/.steam/steam/steamapps/compatdata/$FS25_APPID"
        "$HOME/.steam/root/steamapps/compatdata/$FS25_APPID"
        "$HOME/.var/app/com.valvesoftware.Steam/.local/share/Steam/steamapps/compatdata/$FS25_APPID"
    )
    for r in "${roots[@]}"; do
        local d="$r/pfx/drive_c/users/steamuser/Documents/My Games/FarmingSimulator2025/mods"
        if [[ -d "$r" ]]; then
            mkdir -p "$d"
            printf '%s\n' "$d"
            return 0
        fi
    done
    return 1
}

install_mod() {
    log "installing Lua mod into FS25 mods directory"
    local dest
    if ! dest=$(find_fs25_mods_dir); then
        warn "No FS25 Proton prefix yet. Launch FS25 once in Steam so Proton creates it."
        warn "Re-run this script afterwards."
        return 1
    fi
    log "mods dir: $dest"

    # Install as a directory, not a zip. FS25 accepts both, but directory
    # form avoids the common "modDesc.xml must be at zip root" trap
    # (GIANTS' parser is strict about the zip top-level layout).
    local target="$dest/FS25_FFBEnhancer"
    rm -rf "$target"
    rm -f "$dest/FS25_FFBEnhancer.zip"
    mkdir -p "$target"
    # rsync is nicer but may be absent; cp -a works everywhere.
    cp -a "$MOD_SRC/." "$target/"
    # Strip dev-only sidecars from the installed copy.
    find "$target" -name "*.dds.README" -delete
    find "$target" -name "README.md" -delete

    log "installed mod folder at $target"
}

build_daemon() {
    log "building daemon/GUI (cargo --release)"
    command -v cargo >/dev/null || fail "cargo not found; install rustup or rust"
    ( cd "$DAEMON_DIR" && cargo build --release )
}

install_binary() {
    log "installing fs25-ffb binary to $PREFIX/bin"
    ask_sudo "binary install"
    sudo install -Dm0755 "$DAEMON_DIR/target/release/fs25-ffb" "$PREFIX/bin/fs25-ffb"
    sudo install -Dm0644 "$REPO_ROOT/packaging/fs25-ffb.desktop" \
        "$PREFIX/share/applications/fs25-ffb.desktop"
}

install_udev() {
    log "installing udev rule"
    ask_sudo "udev rule"
    sudo install -Dm0644 "$REPO_ROOT/packaging/99-fs25-ffb.rules" \
        /etc/udev/rules.d/99-fs25-ffb.rules
    sudo udevadm control --reload
    sudo udevadm trigger
    log "adding $USER to input group (takes effect on next login)"
    sudo gpasswd -a "$USER" input || true
}

install_service() {
    log "installing systemd --user unit"
    install -Dm0644 "$REPO_ROOT/packaging/fs25-ffb.service" \
        "$HOME/.config/systemd/user/fs25-ffb.service"
    systemctl --user daemon-reload || true
    log "enable with: systemctl --user enable --now fs25-ffb.service"
}

do_uninstall() {
    log "removing systemd user unit"
    systemctl --user disable --now fs25-ffb.service 2>/dev/null || true
    rm -f "$HOME/.config/systemd/user/fs25-ffb.service"
    systemctl --user daemon-reload || true

    log "removing binary and desktop file"
    ask_sudo "uninstall"
    sudo rm -f "$PREFIX/bin/fs25-ffb"
    sudo rm -f "$PREFIX/share/applications/fs25-ffb.desktop"
    sudo rm -f /etc/udev/rules.d/99-fs25-ffb.rules
    sudo udevadm control --reload || true

    log "removing Lua mod zip from FS25 mods dir"
    local dest
    if dest=$(find_fs25_mods_dir); then
        rm -f "$dest/FS25_FFBEnhancer.zip"
        rm -rf "$dest/FS25_FFBEnhancer"
    fi
    log "done. Config at ~/.config/fs25-ffb left intact."
}

case "$mode" in
    mod)
        install_mod
        ;;
    no_service)
        build_daemon
        install_binary
        install_udev
        install_mod || warn "mod install deferred"
        log "skipping systemd unit (per --no-service)"
        ;;
    full)
        build_daemon
        install_binary
        install_udev
        install_service
        install_mod || warn "mod install deferred - launch FS25 once then run: $0 --mod-only"
        log "done. Next:"
        log "  1) log out / log in so 'input' group takes effect"
        log "  2) systemctl --user enable --now fs25-ffb.service"
        log "  3) add Steam launch option:  SDL_JOYSTICK_HIDAPI=0 %command%"
        log "  4) launch fs25-ffb from your app menu to tune"
        ;;
    uninstall)
        do_uninstall
        ;;
esac
