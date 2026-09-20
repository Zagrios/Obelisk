use forge_steam_native::{host, native, sdk};
use steam_runtime::protocol::{Event, Failure};

fn main() {
    let code = run();
    // SAFETY: process exit owns the native engine's teardown. No unverified
    // private C++ shutdown/destructor ABI is invoked after Rust cleanup.
    unsafe { libc::_exit(code) }
}
fn run() -> i32 {
    if native::disable_core_dumps().is_err() || native::translated() {
        return 2;
    }
    let args: Vec<_> = std::env::args().collect();
    match args.get(1).map(String::as_str) {
        Some("process-identity") => return process_identity(args.get(2)),
        Some("supervise") => return supervise(),
        _ => {}
    }
    let Ok(mut output) = host::event_pipe() else {
        return 2;
    };
    let result = match args.get(1).map(String::as_str) {
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
        // Offline packaging validation: load pinned libraries and resolve the
        // engine factories only. No global user, credentials or network login.
        Some("verify-runtime") if args.len() == 3 => {
            Some(forge_steam_native::abi::Engine::open(args[2].as_ref()).map(|_| ()))
        }
        Some("engine") if args.len() == 5 => args[3]
            .parse::<u32>()
            .ok()
            .map(|app| host::serve(args[2].as_ref(), app, &args[4], &mut output)),
        Some("check") if args.len() == 5 => args[4].parse::<u32>().ok().map(|app| {
            let asynchronous_request =
                sdk::verify(args[2].as_ref(), args[3].as_ref(), app, |stage| {
                    host::emit(&mut output, &Event::SdkStage { stage })
                })?;
            host::emit(
                &mut output,
                &Event::SdkVerified {
                    app_id: app,
                    asynchronous_request,
                },
            )
        }),
        _ => None,
    };
    match result {
        Some(Ok(())) => 0,
        _ => {
            let _ = host::emit(
                &mut output,
                &Event::Failed {
                    reason: if args.get(1).is_some_and(|mode| mode == "check") {
                        Failure::Sdk
                    } else {
                        Failure::Engine
                    },
                },
            );
            1
        }
    }
}

fn process_identity(argument: Option<&String>) -> i32 {
    let Some(pid) = argument.and_then(|s| s.parse::<u32>().ok()) else {
        return 2;
    };
    let identity = native::process_identity(pid);
    if identity.is_none() && !native::process_is_absent(pid) {
        return 1;
    }
    use std::io::Write;
    let mut output = std::io::stdout().lock();
    let written = serde_json::to_writer(&mut output, &identity).is_ok()
        && output.write_all(b"\n").is_ok()
        && output.flush().is_ok();
    i32::from(!written)
}

fn supervise() -> i32 {
    let served = native::detach_supervisor().is_ok()
        && steam_runtime::supervisor::serve(
            std::io::stdin().lock(),
            std::io::stderr().lock(),
            native::process_identity,
        )
        .is_ok();
    i32::from(!served)
}
