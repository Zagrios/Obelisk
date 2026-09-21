//! Intel game exec handoff. Steamworks initialization belongs to the game.
use forge_steam_native::native;

fn main() {
    let code = run();
    // SAFETY: terminate this helper after failed exec without running teardown
    // for libraries inherited through the game's discovery environment.
    unsafe { libc::_exit(code) }
}

fn run() -> i32 {
    let arguments: Vec<_> = std::env::args_os().collect();
    if arguments.get(1).is_some_and(|mode| mode == "exec-game") {
        // SETEXEC receives the exact game arguments/environment supplied by the
        // supervisor and preserves its process admission handshake.
        let _ = native::exec_game(&arguments[2..]);
        return 1;
    }
    2
}
