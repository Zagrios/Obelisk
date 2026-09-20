//! SDK consumer matching the game's architecture. The private engine stays ARM64.
use forge_steam_native::{events, native, sdk};
use steam_runtime::protocol::{Event, Failure};

fn main() {
    let code = run();
    // SAFETY: Steam libraries remain loaded for process lifetime; no unverified
    // C++ destructors run during executable teardown.
    unsafe { libc::_exit(code) }
}

fn run() -> i32 {
    let arguments: Vec<_> = std::env::args_os().collect();
    if arguments.get(1).is_some_and(|mode| mode == "exec-game") {
        // SETEXEC must run before SDK output redirection or rlimit changes. It
        // receives the exact game arguments/environment supplied by the supervisor.
        let _ = native::exec_game(&arguments[2..]);
        return 1;
    }
    if native::disable_core_dumps().is_err() {
        return 2;
    }
    let args: Vec<_> = std::env::args().collect();
    let Ok(mut output) = events::event_pipe() else {
        return 2;
    };
    let result = match args.get(1).map(String::as_str) {
        Some("check") if args.len() == 5 => args[4].parse::<u32>().ok().map(|app| {
            let asynchronous_request =
                sdk::verify(args[2].as_ref(), args[3].as_ref(), app, |stage| {
                    events::emit(&mut output, &Event::SdkStage { stage })
                })?;
            events::emit(
                &mut output,
                &Event::SdkVerified {
                    app_id: app,
                    asynchronous_request,
                },
            )
        }),
        #[cfg(debug_assertions)]
        Some("check-storage") if args.len() == 6 => args[4].parse::<u32>().ok().map(|app| {
            sdk::storage_probe(
                args[2].as_ref(),
                args[3].as_ref(),
                app,
                args[5] == "read",
                &mut output,
            )
        }),
        _ => None,
    };
    match result {
        Some(Ok(())) => 0,
        _ => {
            let _ = events::emit(
                &mut output,
                &Event::Failed {
                    reason: Failure::Sdk,
                },
            );
            1
        }
    }
}
