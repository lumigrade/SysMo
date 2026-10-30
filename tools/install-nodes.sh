#!/bin/sh
# install-nodes.sh - put SysMo's background reporter on computers nobody
# logs in to (render or compute nodes), from this computer, over SSH.
#
# Run it from the SysMo source folder on the computer that shows the
# Network tab, after installing SysMo there:
#
#   tools/install-nodes.sh 192.168.1.227 192.168.1.224 192.168.1.229
#   tools/install-nodes.sh --remove 192.168.1.227      # undo
#
# For each node it checks that this computer can log in with its SSH key and
# that the node runs the same OS release, copies sysmo-reporter and the
# sensor engine to ~/.local on the node, adds a line to the node's crontab
# that starts the reporter at boot (and again if it ever stops), and starts
# it now. The nodes need no sudo, compilers or internet access.
#
# Computers with the SysMo desktop app are skipped: the app reports by
# itself. If a node reporter is found there anyway, it is taken off (it would
# hold the app's network port) and the app restarted; the engine files the
# app shares are never deleted, with or without --remove.
#
# SPDX-License-Identifier: GPL-3.0-or-later

set -eu
cd "$(dirname "$0")/.."

remove=false
if [ "${1:-}" = "--remove" ]; then
    remove=true
    shift
fi
if [ $# -eq 0 ]; then
    echo "Usage: $0 [--remove] [user@]address ..." >&2
    exit 2
fi

SSH="ssh -o BatchMode=yes -o ConnectTimeout=10"

# Runs on the node (read from stdin, so no pattern below ever matches its own
# command line). Stops any running reporter, then installs or removes. Mode
# "desktop": the computer has the SysMo desktop app.
node_script() {
    cat <<'EOF'
set -eu
mode=$1
loop='while :; do $HOME/.local/bin/sysmo-reporter; sleep 5; done'
app=$HOME/.local/bin/sysmo
[ -e "$app" ] || app=$(command -v sysmo || echo "$app")
desktop=false
if [ -e "$app" ] || pgrep -x sysmo >/dev/null; then
    desktop=true
fi
reporter=false
if pgrep -x sysmo-reporter >/dev/null || crontab -l 2>/dev/null | grep -q sysmo-reporter; then
    reporter=true
fi
if [ "$desktop" = true ] && [ "$reporter" = false ]; then
    if [ "$mode" = remove ]; then
        echo "has the SysMo desktop app and no node reporter: nothing to remove"
    else
        echo "skipped: has the SysMo desktop app, which reports by itself"
    fi
    exit 0
fi
# Stop the restart loop first, then the reporter (which stops its engine),
# and wait until it has let go of its network port.
pkill -f 'sysmo-reporter; sleep 5' 2>/dev/null || true
pkill -x sysmo-reporter 2>/dev/null || true
waited=0
while pgrep -x sysmo-reporter >/dev/null && [ "$waited" -lt 20 ]; do
    sleep 1
    waited=$((waited + 1))
done
if pgrep -x sysmo-reporter >/dev/null; then
    pkill -KILL -x sysmo-reporter 2>/dev/null || true
    pkill -x sysmo-magpie 2>/dev/null || true
fi
crontab -l 2>/dev/null | grep -v 'sysmo-reporter' > "$HOME/.sysmo-crontab" || true
if [ "$mode" = remove ] || [ "$desktop" = true ]; then
    crontab "$HOME/.sysmo-crontab"
    rm -f "$HOME/.sysmo-crontab"
    if [ "$desktop" = false ]; then
        rm -f "$HOME/.local/bin/sysmo-reporter" "$HOME/.local/bin/sysmo-magpie"
        rm -rf "$HOME/.local/share/sysmo"
        echo "removed"
        exit 0
    fi
    # The desktop app shares the engine and its data, so they stay. It only
    # claims its network port when it starts: restart it to take the port back.
    note="has the SysMo desktop app: removed a node reporter that blocked it"
    if pgrep -x sysmo >/dev/null; then
        pkill -x sysmo 2>/dev/null || true
        sleep 3
        pkill -KILL -x sysmo 2>/dev/null || true
        sleep 1
        if systemd-run --user --collect --quiet "$app" 2>/dev/null; then
            echo "$note, restarted SysMo"
        else
            echo "$note; start SysMo there again"
        fi
    else
        echo "$note"
    fi
    exit 0
fi
# Swap in the new files; mv works even while an old copy still runs.
mv -f "$HOME/.local/bin/sysmo-reporter.new" "$HOME/.local/bin/sysmo-reporter"
mv -f "$HOME/.local/bin/sysmo-magpie.new" "$HOME/.local/bin/sysmo-magpie"
missing=$(ldd "$HOME/.local/bin/sysmo-reporter" "$HOME/.local/bin/sysmo-magpie" 2>/dev/null \
    | awk '/not found/ { print $1 }' | sort -u | tr '\n' ' ')
if [ -n "$missing" ]; then
    echo "missing libraries: $missing"
    exit 1
fi
if ! command -v crontab >/dev/null; then
    echo "cron is not installed (sudo apt install cron)"
    exit 1
fi
echo "@reboot sh -c '$loop' >/dev/null 2>&1" >> "$HOME/.sysmo-crontab"
crontab "$HOME/.sysmo-crontab"
rm -f "$HOME/.sysmo-crontab"
setsid sh -c "$loop" </dev/null >/dev/null 2>&1 &
waited=0
until pgrep -x sysmo-reporter >/dev/null || [ "$waited" -ge 10 ]; do
    sleep 1
    waited=$((waited + 1))
done
# Still there a few seconds later: it got its port and is reporting.
sleep 3
if pgrep -x sysmo-reporter >/dev/null; then
    echo "running, starts at boot"
else
    echo "installed, but it did not start: run ~/.local/bin/sysmo-reporter there to see why"
    exit 1
fi
EOF
}

install_node() {
    host=$1
    # Its release, e.g. "Ubuntu 24.04" (point updates such as 24.04.4 and
    # 24.04.5 share the same system libraries), and whether it has the SysMo
    # desktop app.
    # shellcheck disable=SC2016 # expands on the node
    info=$($SSH "$host" '. /etc/os-release && echo "$NAME $VERSION_ID"
        if [ -e "$HOME/.local/bin/sysmo" ] || command -v sysmo >/dev/null || pgrep -x sysmo >/dev/null; then
            echo desktop
        fi' 2>/dev/null) || {
        echo "can't log in with this computer's SSH key (try: ssh $host)"
        return 1
    }
    there=$(printf '%s\n' "$info" | sed -n 1p)
    if [ "$remove" = true ]; then
        node_script | $SSH "$host" 'sh -s remove'
        return
    fi
    # Copy nothing to a computer with the desktop app.
    if [ "$(printf '%s\n' "$info" | sed -n 2p)" = desktop ]; then
        node_script | $SSH "$host" 'sh -s desktop'
        return
    fi
    if [ "$there" != "$here" ]; then
        echo "runs $there, this computer runs $here: not installed"
        return 1
    fi
    if ! {
        $SSH "$host" 'mkdir -p .local/bin .local/share/sysmo' &&
            scp -q -o BatchMode=yes build/src/sysmo-reporter "$host:.local/bin/sysmo-reporter.new" &&
            scp -q -o BatchMode=yes build/src/sysmo-magpie "$host:.local/bin/sysmo-magpie.new" &&
            scp -q -o BatchMode=yes build/src/hw.db "$host:.local/share/sysmo/hw.db"
    }; then
        echo "copying failed"
        return 1
    fi
    node_script | $SSH "$host" 'sh -s install'
}

if [ "$remove" = false ]; then
    # Build here first: the nodes get exactly these files.
    [ -d build ] || meson setup build --prefix="$HOME/.local" -Dbuildtype=release >/dev/null
    echo "Building SysMo on this computer..."
    if ! ninja -C build >build/install-nodes.log 2>&1; then
        tail -n 20 build/install-nodes.log
        echo "Build failed; the full log is in build/install-nodes.log" >&2
        exit 1
    fi
    for file in build/src/sysmo-reporter build/src/sysmo-magpie build/src/hw.db; do
        [ -f "$file" ] || { echo "Missing $file after the build" >&2; exit 1; }
    done
fi
# shellcheck disable=SC1091
here=$(. /etc/os-release && echo "$NAME $VERSION_ID")

# All nodes at once; each one's lines are prefixed with its address.
failures=$(mktemp -d)
n=0
for host in "$@"; do
    n=$((n + 1))
    { install_node "$host" 2>&1 || touch "$failures/$n"; } | sed "s|^|$host: |" &
done
wait
failed=$(find "$failures" -type f | wc -l)
rm -rf "$failures"

echo
if [ "$failed" -gt 0 ]; then
    echo "$failed of $# node(s) failed; see above."
    exit 1
fi
if [ "$remove" = true ]; then
    echo "Done."
else
    echo "Done. The nodes appear in SysMo's Network tab within a minute (or press ↻)."
    echo "If one does not, its firewall may block UDP 47917: on that node run"
    echo "  sudo ufw allow 47917/udp"
fi
