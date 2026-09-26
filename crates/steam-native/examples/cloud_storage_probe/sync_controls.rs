//! Observe explicit sync admission, offline failure and cancellation, not transfers.
use super::{method, report};
use sailover_steam_native::{Result, abi::Callback, native::Library, symbol};
use std::{
    ffi::c_void,
    fs::File,
    time::{Duration, Instant},
};

pub(super) fn run(
    output: &mut File,
    storage: *mut c_void,
    base: usize,
    pipe: i32,
    library: &Library,
    mode: &str,
) -> Result<()> {
    if std::env::var_os("SAILOVER_CLOUD_PROBE_IDENTITY").is_none() {
        return Err("Sync control fixture requires the disconnected test identity".into());
    }
    let next = symbol!(
        library,
        c"Steam_BGetCallback",
        unsafe extern "C" fn(i32, *mut Callback) -> bool
    );
    let free = symbol!(
        library,
        c"Steam_FreeLastCallback",
        unsafe extern "C" fn(i32)
    );
    let direction = if mode == "sync-upload" { 2 } else { 1 };
    let expected_id = if direction == 1 { 1_250_001 } else { 1_250_002 };
    let address = method(storage, base, 78, 0x90393c)?;
    // SAFETY: pinned proxy encodes AppID u32, direction u32, options u64.
    // Server maps 1 to client/download completion and 2 to server/upload.
    // Zero options sets no auxiliary bits. All external network is denied.
    let accepted = unsafe {
        let call: unsafe extern "C" fn(*mut c_void, u32, u32, u64) -> bool =
            std::mem::transmute(address);
        call(storage, 480, direction, 0)
    };
    report(
        output,
        serde_json::json!({"event":"sync_request", "direction":direction, "options":0, "accepted":accepted}),
    )?;
    if mode == "sync-cancel" {
        let address = method(storage, base, 84, 0x904220)?;
        // SAFETY: no-argument private cancellation proxy. This disposable
        // runtime owns only this fixture; cancellation is NOT per-app scoped.
        unsafe {
            let call: unsafe extern "C" fn(*mut c_void) = std::mem::transmute(address);
            call(storage);
        }
        report(output, serde_json::json!({"event":"sync_cancel_requested"}))?;
    }
    let started = Instant::now();
    let mut completed = false;
    let mut outcome = None;
    let mut previous = None;
    while started.elapsed() < Duration::from_secs(5) {
        let address = method(storage, base, 79, 0x903af8)?;
        // SAFETY: pinned IsAppSyncInProgress proxy accepts AppID and returns bool.
        let active = unsafe {
            let call: unsafe extern "C" fn(*mut c_void, u32) -> bool = std::mem::transmute(address);
            call(storage, 480)
        };
        if previous != Some(active) {
            report(
                output,
                serde_json::json!({"event":"sync_in_progress", "value":active}),
            )?;
            previous = Some(active);
        }
        let mut callback = Callback::default();
        // SAFETY: exported callback API and existing 24-byte CallbackMsg layout.
        // Only exact statically established ID/size pairs may expose bytes.
        // Copy before freeing once; unknown events are represented by metadata.
        let event = unsafe {
            if next(pipe, &mut callback) {
                let recognized = matches!(
                    (callback.id, callback.size),
                    (1_250_001 | 1_250_002, 12) | (1_250_005 | 1_250_006, 8)
                );
                let words = if recognized && !callback.data.is_null() {
                    let bytes = std::slice::from_raw_parts(callback.data, callback.size as usize);
                    Some(
                        bytes
                            .as_chunks::<4>()
                            .0
                            .iter()
                            .map(|word| u32::from_le_bytes(*word))
                            .collect::<Vec<_>>(),
                    )
                } else {
                    None
                };
                let event = (callback.id, callback.size, words);
                free(pipe);
                Some(event)
            } else {
                None
            }
        };
        if let Some((id, size, words)) = event {
            report(
                output,
                serde_json::json!({"event":"sync_callback", "id":id, "size":size, "raw_u32":words}),
            )?;
            if id == expected_id
                && let Some(words) = words
                && words[0] == 480
            {
                completed = true;
                outcome = Some(words[1]);
            }
        }
        if completed && !active {
            break;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    report(
        output,
        serde_json::json!({"event":"offline_sync_observation", "callback_observed":completed, "result_word":outcome, "idle":previous == Some(false), "milliseconds":started.elapsed().as_millis()}),
    )?;
    if !accepted || !completed || previous != Some(false) {
        return Err("Offline sync control contract not observed".into());
    }
    // This fixture has no remote connection. A success word would need a new
    // explanation; it must never be counted as an uploaded/downloaded fixture.
    if outcome == Some(1) {
        return Err("Unexpected success in the disconnected sync fixture".into());
    }
    Ok(())
}
