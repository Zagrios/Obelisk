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
    let Ok(mut output) = host::event_pipe() else {
        return 2;
    };
    let args: Vec<_> = std::env::args().collect();
    let result = match args.get(1).map(String::as_str) {
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
