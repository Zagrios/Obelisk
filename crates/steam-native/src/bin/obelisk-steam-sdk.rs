//! Obelisk's Intel helper: translates D3DMetal ahead of the Linux engine's
//! games.
use obelisk_steam_native::native;

fn main() {
    let code = run();
    // SAFETY: ends this helper without running the teardown of the libraries
    // it loaded.
    unsafe { libc::_exit(code) }
}

fn run() -> i32 {
    let arguments: Vec<_> = std::env::args_os().collect();
    if arguments.len() == 4 && arguments[1] == "warm-d3dmetal" {
        return if native::warm_d3dmetal(&arguments[2], &arguments[3]) {
            0
        } else {
            1
        };
    }
    2
}
