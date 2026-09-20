//! Disposable, in-process private/public storage coherence experiment.
use super::{load_cache, method, read_fixture, report};
use forge_steam_native::{Result, abi};
use std::{
    ffi::{CStr, c_char, c_void},
    fs::File,
    path::Path,
    time::{Duration, Instant},
};

const APP: u32 = 480;
const ORIGINAL: &CStr = c"forge-offline-probe.dat";
const LIVE: &CStr = c"forge-offline-live.dat";
const FIRST: &[u8] = b"Forge offline RemoteStorage fixture v1\n";
const UPDATED: &[u8] = b"Forge offline RemoteStorage fixture v2 - changed length\n";

pub(super) fn prepare_pipe_app(output: &mut File, utils: *mut c_void, base: usize) -> Result<()> {
    let get = method(utils, base, 19, 0x8cc76c)?;
    let set = method(utils, base, 18, 0x8cc5c8)?;
    // SAFETY: pinned Utils proxies serialize no input for GetAppID and u32/u8
    // for SetAppIDForCurrentPipe; both decode four-byte results. The dispatch
    // tables independently identify slots 19/18. Fixture-only option 0 has no
    // assumed production meaning. Re-query before obtaining public interfaces.
    let (before, result, after) = unsafe {
        let get: unsafe extern "C" fn(*mut c_void) -> u32 = std::mem::transmute(get);
        let set: unsafe extern "C" fn(*mut c_void, u32, u8) -> u32 = std::mem::transmute(set);
        let before = get(utils);
        let result = set(utils, APP, 0);
        (before, result, get(utils))
    };
    report(
        output,
        serde_json::json!({"event":"fixture_pipe_app", "before":before, "result":result, "after":after, "option":0}),
    )?;
    if after != APP {
        return Err("Public fixture pipe has the wrong AppID".into());
    }
    Ok(())
}

pub(super) struct Storage {
    private: *mut c_void,
    public: *mut c_void,
    base: usize,
}

impl Storage {
    pub(super) fn new(private: *mut c_void, public: *mut c_void, base: usize) -> Self {
        Self {
            private,
            public,
            base,
        }
    }

    pub(super) fn run(
        &self,
        output: &mut File,
        mode: &str,
        scratch: &Path,
        engine: &abi::Engine,
        pipe: i32,
    ) -> Result<()> {
        if std::env::var_os("FORGE_CLOUD_PROBE_IDENTITY").is_none() {
            return Err("Live mutation requires the disconnected test identity".into());
        }
        let app = scratch.join("home/Library/Application Support/Steam/userdata/1/480");
        load_cache(output, self.private, self.base, engine, pipe)?;
        let mut callbacks = Vec::new();
        if mode == "live-mutate" {
            self.write(output, ORIGINAL, FIRST, false)?;
            self.read_both(output, ORIGINAL, FIRST)?;
            self.write(output, ORIGINAL, UPDATED, true)?;
            self.read_both(output, ORIGINAL, UPDATED)?;
            self.write(output, LIVE, UPDATED, false)?;
            self.read_both(output, LIVE, UPDATED)?;
            self.delete(output, ORIGINAL)?;
            self.check_final(output, &app)?;
            let started = Instant::now();
            let mut checkpoint = false;
            while started.elapsed() < Duration::from_secs(5) {
                if let Some((id, size)) = engine.callback(pipe) {
                    callbacks.push(serde_json::json!({"id":id,"size":size}));
                }
                if std::fs::read_to_string(app.join("remotecache.vdf")).is_ok_and(|cache| {
                    fixture_record_has(
                        &cache,
                        LIVE,
                        "sha",
                        "bd8f4fcb8d3ed72a2528e880e879697129768fa3",
                    ) && fixture_record_has(&cache, LIVE, "size", "56")
                        && fixture_record_has(&cache, ORIGINAL, "persiststate", "2")
                }) {
                    checkpoint = true;
                    break;
                }
                std::thread::sleep(Duration::from_millis(10));
            }
            report(
                output,
                serde_json::json!({"event":"live_cache_checkpoint", "record_present":checkpoint, "wait_milliseconds":started.elapsed().as_millis()}),
            )?;
            if !checkpoint {
                return Err("Live index checkpoint not observed".into());
            }
        } else {
            self.check_final(output, &app)?;
        }
        let address = method(self.private, self.base, 85, 0x904368)?;
        // SAFETY: pinned private GetLocalFileChangeCount takes AppID and returns u32.
        let changes = unsafe {
            let call: unsafe extern "C" fn(*mut c_void, u32) -> u32 = std::mem::transmute(address);
            call(self.private, APP)
        };
        report(
            output,
            serde_json::json!({"event":"local_file_change_count", "value":changes}),
        )?;
        // Observe callback metadata, not payloads, without inventing notification IDs.
        let started = Instant::now();
        while started.elapsed() < Duration::from_millis(500) {
            if let Some((id, size)) = engine.callback(pipe) {
                callbacks.push(serde_json::json!({"id":id,"size":size}));
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        report(
            output,
            serde_json::json!({"event":"post_mutation_callbacks", "callbacks":callbacks}),
        )?;
        let cache = std::fs::read_to_string(app.join("remotecache.vdf"))?;
        report(
            output,
            serde_json::json!({"event":"fixture_native_index", "vdf":cache}),
        )
    }

    fn write(&self, output: &mut File, name: &CStr, payload: &[u8], public: bool) -> Result<()> {
        let result = if public {
            let address = method(self.public, self.base, 0, 0x7e22d0)?;
            // SAFETY: public FileWrite takes filename, bytes, i32 size and returns bool.
            unsafe {
                let call: unsafe extern "C" fn(*mut c_void, *const c_char, *const u8, i32) -> bool =
                    std::mem::transmute(address);
                u32::from(call(
                    self.public,
                    name.as_ptr(),
                    payload.as_ptr(),
                    payload.len() as i32,
                ))
            }
        } else {
            let address = method(self.private, self.base, 0, 0x8fb410)?;
            // SAFETY: guarded private FileWrite ABI established by the public adapter.
            unsafe {
                let call: unsafe extern "C" fn(
                    *mut c_void,
                    u32,
                    i32,
                    *const c_char,
                    *const u8,
                    i32,
                ) -> u32 = std::mem::transmute(address);
                call(
                    self.private,
                    APP,
                    0,
                    name.as_ptr(),
                    payload.as_ptr(),
                    payload.len() as i32,
                )
            }
        };
        report(
            output,
            serde_json::json!({"event":"live_write", "interface":if public {"public"} else {"private"}, "name":name.to_str()?, "bytes":payload.len(), "result":result}),
        )?;
        if result != 1 {
            return Err("Live fixture write rejected".into());
        }
        Ok(())
    }

    fn read_both(&self, output: &mut File, name: &CStr, payload: &[u8]) -> Result<()> {
        read_fixture(output, self.private, self.base, name, payload)?;
        let address = method(self.public, self.base, 1, 0x7e232c)?;
        let mut bytes = [0_u8; 256];
        // SAFETY: published FileRead signature, writable bounded buffer and checked result.
        let count = unsafe {
            let call: unsafe extern "C" fn(*mut c_void, *const c_char, *mut u8, i32) -> i32 =
                std::mem::transmute(address);
            call(
                self.public,
                name.as_ptr(),
                bytes.as_mut_ptr(),
                bytes.len() as i32,
            )
        };
        let matched = count == payload.len() as i32 && bytes.get(..count as usize) == Some(payload);
        report(
            output,
            serde_json::json!({"event":"public_file_read", "name":name.to_str()?, "bytes":count, "matches_fixture":matched}),
        )?;
        if !matched {
            return Err("Public fixture read did not match".into());
        }
        Ok(())
    }

    fn delete(&self, output: &mut File, name: &CStr) -> Result<()> {
        let address = method(self.private, self.base, 7, 0x8fc0f0)?;
        // SAFETY: proxy serializes AppID/root/name and decodes one-byte bool.
        // Public adapter 0x7e24ec forwards the same arguments to private slot 7.
        let deleted = unsafe {
            let call: unsafe extern "C" fn(*mut c_void, u32, i32, *const c_char) -> bool =
                std::mem::transmute(address);
            call(self.private, APP, 0, name.as_ptr())
        };
        report(
            output,
            serde_json::json!({"event":"private_file_delete", "name":name.to_str()?, "accepted":deleted}),
        )?;
        if !deleted {
            return Err("Fixture deletion rejected".into());
        }
        Ok(())
    }

    fn check_final(&self, output: &mut File, app: &Path) -> Result<()> {
        self.read_both(output, LIVE, UPDATED)?;
        let address = method(self.public, self.base, 13, 0x7e2658)?;
        // SAFETY: published FileExists signature and pinned public adapter.
        let original_exists = unsafe {
            let call: unsafe extern "C" fn(*mut c_void, *const c_char) -> bool =
                std::mem::transmute(address);
            call(self.public, ORIGINAL.as_ptr())
        };
        let address = method(self.public, self.base, 18, 0x7e2784)?;
        // SAFETY: published GetFileCount signature, no additional arguments.
        let count = unsafe {
            let call: unsafe extern "C" fn(*mut c_void) -> i32 = std::mem::transmute(address);
            call(self.public)
        };
        let direct_matches = std::fs::read(app.join("remote").join(LIVE.to_str()?))? == UPDATED;
        let original_on_disk = app.join("remote").join(ORIGINAL.to_str()?).try_exists()?;
        report(
            output,
            serde_json::json!({"event":"live_final_state", "file_count":count, "deleted_file_visible":original_exists, "deleted_file_on_disk":original_on_disk, "direct_read_matches":direct_matches}),
        )?;
        if original_exists || original_on_disk || count != 1 || !direct_matches {
            return Err("Unexpected final live storage state".into());
        }
        Ok(())
    }
}

// This is a predicate for two fixed probe filenames in native fixture output,
// not a general VDF parser or a production cache-format implementation.
fn fixture_record_has(cache: &str, name: &CStr, key: &str, value: &str) -> bool {
    let Some((_, record)) = cache.split_once(&format!("\"{}\"", name.to_str().unwrap())) else {
        return false;
    };
    let Some((record, _)) = record.split_once('}') else {
        return false;
    };
    let words: Vec<_> = record.split_whitespace().collect();
    words
        .windows(2)
        .any(|pair| pair[0].trim_matches('"') == key && pair[1].trim_matches('"') == value)
}
