#!/bin/bash
# Runs a Windows game through Voile's Linux engine (voile-lkd): voile-wine built for x86-64 Linux, which
# fexdarwin-run executes in native macOS processes. Starts the window server, Neptune's render server and the input
# service, then the game, and ends them when the game ends or this script is stopped. Every guest process follows
# this script (FEXDARWIN_SUPERVISOR_PID) and ends within half a second once it is gone.
#
# Usage: launch.sh RUNTIME HOME INSTALL CWD [OPTION]... -- EXECUTABLE [ARGUMENT]...
#   RUNTIME     the Linux engine runtime (engine/, graphics/, wine/, root/)
#   HOME        the guest's /home/user; the prefix is its prefix folder
#   INSTALL     the game's installation, seen as /opt/game
#   CWD         the guest working directory
#   EXECUTABLE  the guest path of the Windows program, which Wine starts with its arguments
# Options:
#   --graphics dxmt|d3dmetal  what renders the game's Direct3D (default dxmt)
#   --bind GUEST=HOST[:ro]    a folder the guest also sees, at GUEST, whose parent exists in the root
#   --env NAME=VALUE          a variable of the guest
#   --pass NAME               this script's variable NAME, when set, as a variable of the guest
# OBELISK_APP_NAME names the game in the menu bar, the Dock and Cmd-Tab.
set -u
runtime=$1 home=$2 install=$3 cwd=$4
shift 4
graphics=dxmt binds= guest_env=()
while [ $# -gt 0 ] && [ "$1" != "--" ]; do
    [ $# -ge 2 ] || exit 2
    case $1 in
    --graphics) graphics=$2 ;;
    --bind) binds="$binds;$2" ;;
    --env) guest_env+=(--env "$2") ;;
    --pass)
        [[ $2 =~ ^[A-Za-z_][A-Za-z0-9_]*$ ]] || exit 2
        [ -n "${!2+set}" ] && guest_env+=(--env "$2=${!2}")
        ;;
    *) exit 2 ;;
    esac
    shift 2
done
shift

# The guest root has no time zone file: every program ran in UTC, and glibc looked for /etc/localtime again at each
# localtime() (about 80 times a second in ELDEN RING, 1.9 us each instead of 0.04). The rule that ends the Mac's zone
# file gives the guest the Mac's zone, unless the caller passed one.
case " ${guest_env[*]-} " in
*" TZ="*) ;;
*)
    tz=$(tail -n 1 /etc/localtime 2>/dev/null | LC_ALL=C tr -cd '[:print:]')
    [ -n "$tz" ] && guest_env+=(--env "TZ=$tz")
    ;;
esac

# Wine, the render server and D3DMetal keep a file open for each mapped resource. An app the Finder starts gets 256,
# which Wine outgrows: take up to 65536, as far as the hard limit and macOS allow.
files=$(ulimit -H -n)
{ [ "$files" = unlimited ] || [ "$files" -gt 65536 ]; } && files=65536
ulimit -S -n "$files" 2>/dev/null || ulimit -S -n 10240 2>/dev/null

# wsd and the render server bind UNIX sockets in a private folder; a relative path keeps them inside sun_path.
sockets_root=${TMPDIR:-/tmp}
sockets=obelisk-engine-$$
cd "$sockets_root" || exit 1
mkdir -m 700 "$sockets" || exit 1

helpers=()
game=
finish() {
    [ -n "$game" ] && kill -TERM "$game" 2>/dev/null
    for helper in ${helpers[@]+"${helpers[@]}"}; do kill -TERM "$helper" 2>/dev/null; done
    # They end within a few milliseconds of SIGTERM; a helper still there after a second is killed.
    for _ in $(seq 1 20); do
        alive=
        for helper in ${helpers[@]+"${helpers[@]}"}; do kill -0 "$helper" 2>/dev/null && alive=1; done
        [ -z "$alive" ] && break
        sleep 0.05
    done
    for helper in ${helpers[@]+"${helpers[@]}"}; do kill -KILL "$helper" 2>/dev/null; done
    rm -rf "${sockets_root:?}/$sockets"
}
trap 'finish; trap - EXIT; exit 143' TERM INT HUP
trap finish EXIT

# wsd runs from an application bundle named after the game: macOS takes the name in the menu bar, the Dock and
# Cmd-Tab from it (the game gives the icon). The bundle keeps one place per game, so Launch Services sees one app.
app_name=${OBELISK_APP_NAME:-Game}
app_name=${app_name//\//-}
app_id=$(printf '%s' "$app_name" | LC_ALL=C tr -c 'A-Za-z0-9' '-')
app="$sockets_root/obelisk-apps/$app_id/$app_name.app"
mkdir -p "$app/Contents/MacOS" || exit 1
rm -f "$app/Contents/MacOS/wsd"
cp -c "$runtime/engine/wsd" "$app/Contents/MacOS/wsd" 2>/dev/null || cp "$runtime/engine/wsd" "$app/Contents/MacOS/wsd" || exit 1
xml_name=$(printf '%s' "$app_name" | sed -e 's/&/\&amp;/g' -e 's/</\&lt;/g' -e 's/>/\&gt;/g')
cat > "$app/Contents/Info.plist" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>CFBundleExecutable</key><string>wsd</string>
  <key>CFBundleIdentifier</key><string>com.zagrios.obelisk.game.$app_id</string>
  <key>CFBundleName</key><string>$xml_name</string>
  <key>CFBundleDisplayName</key><string>$xml_name</string>
  <key>CFBundlePackageType</key><string>APPL</string>
  <key>LSApplicationCategoryType</key><string>public.app-category.games</string>
  <key>NSHighResolutionCapable</key><true/>
</dict>
</plist>
PLIST

# The render server runs from a helper bundle inside the game's, one per graphics backend: Metal keeps its compiled
# shaders per bundle identifier. Outside a bundle every render server shared one cache, and each start of DXMT's arm64
# server emptied what D3DMetal's Intel server had compiled there: ELDEN RING compiled its 7,500 Metal functions again,
# about 4 minutes, after every Rocket League start. The helper is in place before wsd starts: for a moment after an
# executable of the bundle starts, macOS refuses any new file in it (EPERM). Prints the path to run.
render_server() {
    local helper="$app/Contents/Helpers/$2.app"
    local server="$helper/Contents/MacOS/virgl_render_server"
    mkdir -p "$helper/Contents/MacOS" || return 1
    # A copy kept from one launch to the next keeps its Rosetta translation.
    if ! cmp -s "$1" "$server"; then
        rm -f "$server"
        cp -c "$1" "$server" 2>/dev/null || cp "$1" "$server" || return 1
    fi
    cat > "$helper/Contents/Info.plist" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>CFBundleExecutable</key><string>virgl_render_server</string>
  <key>CFBundleIdentifier</key><string>com.zagrios.obelisk.game.$app_id.$2</string>
  <key>CFBundleName</key><string>$xml_name</string>
  <key>CFBundlePackageType</key><string>APPL</string>
  <key>LSBackgroundOnly</key><true/>
</dict>
</plist>
PLIST
    # The Intel server starts at the game's first Direct3D call, inside Easy Anti-Cheat's window, and Rosetta
    # translates a new copy then: half a second. Have it translated now; it stops at the unknown option (9 ms once
    # translated).
    if [ "$2" = d3dmetal ]; then
        "$server" --obelisk-translate </dev/null >/dev/null 2>&1
    fi
    printf '%s\n' "$server"
}
if [ "$graphics" = d3dmetal ]; then
    render=$(render_server "$runtime/graphics/x86_64/libexec/virgl_render_server" d3dmetal) || exit 1
else
    render=$(render_server "$runtime/graphics/libexec/virgl_render_server" dxmt) || exit 1
fi

"$app/Contents/MacOS/wsd" --dir "$sockets" &
helpers+=($!)
if [ "$graphics" = d3dmetal ]; then
    # D3DMetal is an Intel library: the Intel render server runs it under Rosetta. d3dmetal-native loads the
    # runtime's D3DMetal, unmodified, from its absolute path. DMN_RESIDENCY_SET=0: the game's buffers are made
    # resident by each command buffer that uses them instead of being pinned for good in a residency set. ELDEN
    # RING's three 4 GiB upload buffers then no longer count as GPU memory (14.6 GB instead of 26.3 on average), the
    # pinned memory no longer peaks at 24 GB while it loads, and the frame rate and hitches stay the same.
    server="$runtime/graphics/x86_64"
    d3dmetal="$server/lib/libd3dmetal-native.dylib"
    RENDER_SERVER_EXEC_PATH="$render" NPT_BACKEND=d3dmetal DMN_RESIDENCY_SET=0 \
        NPT_D3D11_LIBRARY_PATH="$d3dmetal" NPT_DXGI_LIBRARY_PATH="$d3dmetal" NPT_D3D12_LIBRARY_PATH="$d3dmetal" \
        D3DMETAL_FRAMEWORK_PATH="$runtime/../lib/external/D3DMetal.framework" \
        "$server/bin/virgl_test_server" --no-virgl --no-fork --multi-clients --socket-path "$sockets/vtest.sock" &
else
    dxmt="$runtime/graphics/dxmt-native.framework/Versions/A/dxmt-native"
    RENDER_SERVER_EXEC_PATH="$render" NPT_BACKEND=dxmt \
        NPT_D3D11_LIBRARY_PATH="$dxmt" NPT_DXGI_LIBRARY_PATH="$dxmt" NPT_D3D12_LIBRARY_PATH="$dxmt" \
        "$runtime/graphics/bin/virgl_test_server" --no-virgl --no-fork --multi-clients --socket-path "$sockets/vtest.sock" &
fi
helpers+=($!)
# The input service gives the Mac's gamepads to Wine's Obelisk bus. A runtime without it starts the game without
# gamepads.
input=
if [ -x "$runtime/engine/obelisk-input" ]; then
    "$runtime/engine/obelisk-input" --dir "$sockets" &
    helpers+=($!)
    input=1
fi
for _ in $(seq 1 100); do
    [ -S "$sockets/wsd.sock" ] && [ -S "$sockets/vtest.sock" ] && { [ -z "$input" ] || [ -S "$sockets/input.sock" ]; } &&
        break
    sleep 0.05
done

export FEXDARWIN_LKD=1 FEXDARWIN_ROOT="$runtime/root" FEXDARWIN_ROOT_RO=1 FEXDARWIN_SUPERVISOR_PID=$$
export FEXDARWIN_BIND="/opt/wine=$runtime/wine:ro;/home/user=$home;/opt/game=$install;/run/obelisk=$sockets_root/$sockets$binds"
# wineserver writes the registry only when it exits: wait for it inside the guest before the game ends. Started
# with -p0, it stops Wine's own processes as soon as the game's last process ends, instead of 3 seconds later.
"$runtime/engine/fexdarwin-run" --cwd "$cwd" ${guest_env[@]+"${guest_env[@]}"} \
    /bin/sh -c '/opt/wine/bin/wineserver -p0; /opt/wine/bin/wine "$@"; status=$?; /opt/wine/bin/wineserver -w; exit $status' sh "$@" &
game=$!
wait "$game"
status=$?
game=
exit "$status"
