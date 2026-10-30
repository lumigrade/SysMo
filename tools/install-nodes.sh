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
# A computer whose SysMo desktop app already reports (holds the Network
# tab's UDP port) is skipped, and a node reporter found there anyway is taken
# off. Anywhere else, including where the app is installed but stopped or too
# old to report, the node reporter goes on as usual. The engine files a
# desktop app shares are never deleted, with or without --remove.
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
# "skip": the computer's SysMo desktop app already reports.
node_script() {
    cat <<'EOF'
set -eu
mode=$1
loop='while :; do $HOME/.local/bin/sysmo-reporter; sleep 5; done'
app=$HOME/.local/bin/sysmo
[ -e "$app" ] || app=$(command -v sysmo || echo "$app")
reporter=false
if pgrep -x sysmo-reporter >/dev/null || crontab -l 2>/dev/null | grep -q sysmo-reporter; then
    reporter=true
fi
if [ "$mode" = skip ] && [ "$reporter" = false ]; then
    echo "skipped: its SysMo desktop app reports by itself"
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
if [ "$mode" != install ]; then
    crontab "$HOME/.sysmo-crontab"
    rm -f "$HOME/.sysmo-crontab"
    if [ "$mode" = skip ]; then
        echo "skipped: its SysMo desktop app reports by itself (removed a spare node reporter)"
    elif [ -e "$app" ]; then
        # A desktop app shares the engine and its data: they stay.
        echo "removed (kept the files the SysMo desktop app here shares)"
    else
        rm -f "$HOME/.local/bin/sysmo-reporter" "$HOME/.local/bin/sysmo-magpie"
        rm -rf "$HOME/.local/share/sysmo"
        echo "removed"
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

# Prints the node's release, e.g. "Ubuntu 24.04", then "app-reports" when a
# running SysMo desktop app already holds the Network tab's UDP port 47917
# (hex BB2D): the kernel's socket table names the socket, and the app's open
# files show whether it is the app's.
probe_script() {
    cat <<'EOF'
. /etc/os-release && echo "$NAME $VERSION_ID"
inodes=$(awk '$2 ~ /:BB2D$/ { print $10 }' /proc/net/udp /proc/net/udp6 2>/dev/null)
for pid in $(pgrep -x sysmo); do
    for inode in $inodes; do
        if readlink "/proc/$pid/fd/"* 2>/dev/null | grep -qx "socket:\[$inode\]"; then
            echo app-reports
            exit 0
        fi
    done
done
EOF
}

install_node() {
    host=$1
    # Its release (point updates such as 24.04.4 and 24.04.5 share the same
    # system libraries), and whether its SysMo desktop app already reports.
    info=$(probe_script | $SSH "$host" sh -s 2>/dev/null) || {
        echo "can't log in with this computer's SSH key (try: ssh $host)"
        return 1
    }
    there=$(printf '%s\n' "$info" | sed -n 1p)
    if [ "$remove" = true ]; then
        node_script | $SSH "$host" 'sh -s remove'
        return
    fi
    # Copy nothing where the desktop app already reports.
    if [ "$(printf '%s\n' "$info" | sed -n 2p)" = app-reports ]; then
        node_script | $SSH "$host" 'sh -s skip'
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
