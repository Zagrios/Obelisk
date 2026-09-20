use crate::{Result, abi::Engine, mach};
use std::{
    ffi::{CStr, CString},
    fs::File,
    io::Read,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};
use steam_runtime::protocol::{Credentials, Event, Failure};
use zeroize::Zeroizing;

pub use crate::events::{emit, event_pipe};

pub fn serve(
    engine_path: &std::path::Path,
    app: u32,
    service: &str,
    output: &mut File,
) -> Result<()> {
    if app == 0 || !service.starts_with("com.zagrios.forge.discovery.") || service.len() > 120 {
        return Err("Invalid native launch".into());
    }
    let credentials = Credentials::read(std::io::stdin().lock())?;
    let expected_id = credentials.steam_id;
    let stop = Arc::new(AtomicBool::new(false));
    let parent_closed = stop.clone();
    std::thread::spawn(move || {
        let mut byte = [0];
        let _ = std::io::stdin().read(&mut byte);
        parent_closed.store(true, Ordering::Release);
    });
    let engine = Engine::open(engine_path)?;
    let (user, pipe) = engine.global();
    if user == 0 || pipe == 0 {
        return Err("Global user unavailable".into());
    }
    let client = engine.user(user, pipe)?;
    let mut token = Zeroizing::new(credentials.refresh_token.as_bytes().to_vec());
    token.push(0);
    let mut account = Zeroizing::new(credentials.account_name.as_bytes().to_vec());
    account.push(0);
    client.login_token(
        CStr::from_bytes_with_nul(&token)?,
        CStr::from_bytes_with_nul(&account)?,
    )?;
    drop(token);
    drop(account);
    drop(credentials);
    client.log_on(expected_id)?;
    let port = mach::ReceivePort::new()?;
    let _registration = port.register(CString::new(service)?)?;
    let executable = std::env::current_exe()?.canonicalize()?;
    use std::os::unix::ffi::OsStrExt;
    let path = executable.as_os_str().as_bytes();
    if path.len() >= 512 {
        return Err("Host path exceeds discovery limit".into());
    }
    let deadline = Instant::now();
    let mut observed_connection = false;
    let mut authenticated = false;
    let mut ready = false;
    let mut disconnected = false;
    let mut last_check = Instant::now() - Duration::from_secs(1);
    let mut paths = 0_u32;
    while !stop.load(Ordering::Acquire) {
        for _ in 0..100 {
            let Some((id, _)) = engine.callback(pipe) else {
                break;
            };
            if id == 101 {
                observed_connection = true;
            }
            if id == 103 {
                observed_connection = false;
            }
        }
        if last_check.elapsed() >= Duration::from_millis(250) {
            last_check = Instant::now();
            let online = engine.connected(user, pipe)
                && engine.logged_on(user, pipe)
                && observed_connection
                && client.steam_id()? == expected_id;
            if online && !authenticated {
                authenticated = true;
                emit(output, &Event::Authenticated {})?;
                client.request_ticket(app)?;
                if std::env::var_os("FORGE_STORAGE_METADATA").is_some() {
                    engine.request_app_info(user, pipe, app)?;
                }
            }
            if online && authenticated && !ready {
                let length = client.ticket_length(app)?;
                if client.subscribed(app)? && length > 0 {
                    ready = true;
                    emit(
                        output,
                        &Event::Ready {
                            app_id: app,
                            host_pid: std::process::id(),
                            ticket_bytes: length,
                        },
                    )?;
                }
            }
            if ready && !online && !disconnected {
                emit(output, &Event::Disconnected {})?;
                disconnected = true;
            }
            if online {
                if disconnected {
                    emit(output, &Event::Reconnected {})?;
                }
                disconnected = false;
            }
        }
        if !ready && deadline.elapsed() > Duration::from_secs(45) {
            emit(
                output,
                &Event::Failed {
                    reason: if authenticated {
                        Failure::Ownership
                    } else {
                        Failure::Authentication
                    },
                },
            )?;
            break;
        }
        let mut request = mach::Message::default();
        let status = mach::receive(
            &mut request,
            port.0,
            std::mem::size_of::<mach::Message>() as u32,
            50,
        );
        if [mach::RECEIVE_TIMED_OUT, mach::RECEIVE_INTERRUPTED].contains(&status) {
            continue;
        }
        if status != 0 {
            return Err("Native discovery receive failed".into());
        }
        if !mach::valid_request(&request) {
            mach::destroy(&mut request);
            continue;
        }
        let mut reply = mach::Message::default();
        reply.header.bits = if request.header.bits & 0xff == 18 {
            18
        } else {
            17
        };
        reply.header.remote = request.header.remote;
        reply.header.id = 104;
        reply.opcode = request.opcode;
        if request.opcode == 100001 {
            reply.header.size = 32;
            reply.value = 104;
        } else {
            reply.header.size = 544;
            if ready {
                reply.value = std::process::id();
                reply.path[..path.len()].copy_from_slice(path);
            }
            paths = paths.saturating_add(1);
            // Bound event traffic; startup and first game discoveries are enough.
            if paths <= 8 {
                emit(output, &Event::Discovery { paths })?;
            }
        }
        if mach::send(&mut reply) != 0 {
            mach::destroy(&mut reply);
        }
    }
    if std::env::var_os("FORGE_STORAGE_METADATA").is_some() {
        let home = std::env::var_os("HOME").ok_or("Native storage HOME missing")?;
        let cache = std::path::PathBuf::from(home).join(format!(
            "Library/Application Support/Steam/userdata/{}/{app}",
            expected_id as u32
        ));
        let started = Instant::now();
        while !steam_runtime::native_cache_checkpoint(cache.clone(), app) {
            if started.elapsed() > Duration::from_millis(500) {
                return Err("Native storage index incomplete".into());
            }
            while engine.callback(pipe).is_some() {}
            std::thread::sleep(Duration::from_millis(10));
        }
    }
    engine.release(user, pipe);
    Ok(())
}
