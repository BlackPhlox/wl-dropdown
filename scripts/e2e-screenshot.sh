#!/usr/bin/env bash
# End-to-end screenshot test against a real compositor.
#
# Starts a headless sway, runs the real wl-dropdown binary in it, takes a
# screenshot with grim and compares it with tests/e2e/<scene>.png. Where the
# unit-level screenshot tests (`cargo test --test screenshots`) check the
# renderer in isolation, this checks the whole path: layer-shell surface,
# anchoring, shared-memory buffers, fractional scaling and the pty.
#
#   scripts/e2e-screenshot.sh            compare against the references
#   scripts/e2e-screenshot.sh --update   rewrite the references
#
# Needs: sway, grim, and the DejaVu Sans Mono font installed.
set -euo pipefail

root=$(cd "$(dirname "$0")/.." && pwd)
update=0
[[ "${1:-}" == "--update" ]] && update=1

for tool in sway grim; do
    command -v "$tool" >/dev/null || { echo "e2e: $tool is required" >&2; exit 2; }
done

cargo build --quiet --manifest-path "$root/Cargo.toml" --bins --features snapshot-tool
bin="$root/target/debug/wl-dropdown"
snap="$root/target/debug/wl-dropdown-snapshot"
out="$root/target/e2e"
mkdir -p "$out"

work=$(mktemp -d)
sway_pid=
cleanup() {
    [[ -n "$sway_pid" ]] && kill "$sway_pid" 2>/dev/null || true
    rm -rf "$work"
}
trap cleanup EXIT

# One scene per output scale: name, scale.
scenes=("e2e_scale1 1" "e2e_scale1_25 1.25")
status=0

for entry in "${scenes[@]}"; do
    read -r name scale <<<"$entry"
    runtime="$work/$name/run"
    mkdir -p "$runtime" "$work/$name/home/.config/wl-dropdown"
    chmod 700 "$runtime"

    cat >"$work/$name/sway.conf" <<CONF
output HEADLESS-1 resolution 1280x720 position 0 0 scale $scale bg #2a3340 solid_color
default_border none
CONF
    # The baseline layer, as the home-manager module would write it: a
    # font every CI image has, and no animation to wait out.
    cat >"$work/$name/home/.config/wl-dropdown/defaults.conf" <<CONF
[General]
fontFamily=DejaVu Sans Mono
animationMs=0
heightPercent=0.4
CONF

    export XDG_RUNTIME_DIR="$runtime" HOME="$work/$name/home" XDG_CONFIG_HOME="$work/$name/home/.config"
    export WLR_BACKENDS=headless WLR_RENDERER=pixman WLR_LIBINPUT_NO_DEVICES=1
    unset WAYLAND_DISPLAY DISPLAY
    sway -c "$work/$name/sway.conf" >"$out/$name.sway.log" 2>&1 &
    sway_pid=$!
    for _ in $(seq 50); do
        [[ -S "$runtime/wayland-1" ]] && break
        sleep 0.1
    done
    export WAYLAND_DISPLAY=wayland-1

    # Deterministic output; the cursor is hidden so its blink cannot race
    # the screenshot.
    "$bin" -e sh -c 'printf "\033[?25l"
        printf "\033[1;32muser\033[0m@\033[1;34mhost\033[0m:\033[1;36m~\033[0m\$ ls --color\n"
        printf "\033[1;34mbin\033[0m  \033[1;34msrc\033[0m  Cargo.toml  \033[1;32mrun.sh\033[0m  \033[35mshot.png\033[0m\n"
        for c in 41 42 43 44 45 46 47; do printf "\033[${c}m    \033[0m"; done; printf "\n"
        printf "\033[1mbold\033[0m \033[3mitalic\033[0m \033[4munderline\033[0m \033[7minverse\033[0m\n"
        exec sleep 600' >"$out/$name.app.log" 2>&1 &
    sleep 2
    grim -o HEADLESS-1 "$out/$name.png"
    kill "$sway_pid" 2>/dev/null || true
    wait "$sway_pid" 2>/dev/null || true
    sway_pid=

    reference="$root/tests/e2e/$name.png"
    if [[ $update == 1 ]]; then
        cp "$out/$name.png" "$reference"
        echo "updated $reference"
    elif [[ ! -f "$reference" ]]; then
        echo "$name: no reference; run with --update to create $reference" >&2
        status=1
    elif "$snap" diff "$out/$name.png" "$reference" --out "$out/$name.diff.png" --tolerance 24; then
        echo "$name: ok"
    else
        echo "$name: differs; see $out/$name.png and $out/$name.diff.png" >&2
        status=1
    fi
done
exit $status
