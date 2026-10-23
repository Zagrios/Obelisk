#!/bin/bash
# Starts a Windows game through Valve's Windows Steam, hidden, in Rosetta's Wine, and lasts as long as the game: Steam
# starts it, signs in and stays beside it, so the Game session waits on this script. Waits for the game's program to
# start, then to end, then asks Steam to shut down. A Steam that ends before the game starts ends this script with it.
#
# Usage: windows_steam.sh WINE STEAM APPID PROGRAM [ARGUMENT]...
#   WINE      Rosetta's wine, whose environment this script runs with
#   STEAM     the Windows path of steam.exe in the prefix
#   APPID     the game Steam starts
#   PROGRAM   the file name of the game's program, as Wine names its process
#   ARGUMENT  what Steam adds after the game's own arguments
set -u
wine=$1 steam=$2 app=$3 program=$4
shift 4

# Steam's first start in a prefix updates it, which can take minutes under Rosetta.
START_TIMEOUT=900
# Steam writes its configuration as it shuts down.
SHUTDOWN_TIMEOUT=30

# Whether a Wine process runs the Windows program named $1. Wine names its processes by their Windows path; this
# script's own command line names the program too, but never after a drive letter.
running() {
    ps -axww -o command= | awk -v program="$1" '
        BEGIN { program = tolower(program) }
        /^[A-Za-z]:\\/ {
            path = tolower($0)
            sub(/\.exe( .*)?$/, ".exe", path)
            n = split(path, parts, "\\")
            if (parts[n] == program) { found = 1; exit }
        }
        END { exit !found }'
}

# Steam restarts itself as it updates, so its first process may end while Steam still runs.
steam_running() { running steam.exe; }

shutdown() {
    "$wine" "$steam" -shutdown >/dev/null 2>&1
    for ((i = 0; i < SHUTDOWN_TIMEOUT; i++)); do
        steam_running || return
        sleep 1
    done
}

# Its web views draw nothing on Voile's Wine with their GPU process, yet drive its sign-in even while hidden: they
# draw in software. Its friends and chat windows would take the focus from a fullscreen game.
"$wine" "$steam" -silent -nofriendsui -nochatui -cef-disable-gpu -cef-disable-gpu-compositing \
    -applaunch "$app" "$@" &
steam_pid=$!
trap 'kill "$steam_pid" 2>/dev/null; exit 143' TERM INT

started=
for ((i = 0; i < START_TIMEOUT; i++)); do
    if running "$program"; then
        started=1
        break
    fi
    # Its first process gets a few seconds to start the others.
    ((i < 10)) || steam_running || exit 1
    sleep 1
done
[ -n "$started" ] || {
    shutdown
    exit 1
}

while running "$program"; do
    sleep 1
done
shutdown
exit 0
