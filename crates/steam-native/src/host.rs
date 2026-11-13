use crate::{
    Result,
    abi::{ClientFriends, ClientUser, Engine},
    lobby, mach, overlay, received,
};
use game_runtime::protocol::{
    Credentials, Event, Failure, HOST_STARTUP_TIMEOUT, HostCommand, OverlayDialog,
};
use std::{
    ffi::{CStr, CString},
    fs::File,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};
use zeroize::Zeroizing;

pub use crate::events::{emit, event_pipe};

pub fn serve(
    engine_path: &std::path::Path,
    app: u32,
    service: &str,
    output: &mut File,
) -> Result<()> {
    if app == 0 || !service.starts_with("com.zagrios.obelisk.discovery.") || service.len() > 120 {
        return Err("Invalid native launch".into());
    }
    let credentials = Credentials::read(std::io::stdin().lock())?;
    let expected_id = credentials.steam_id;
    let stop = Arc::new(AtomicBool::new(false));
    // No persona change until the supervisor has confirmed the game's exec.
    let presence = Arc::new(Mutex::new(None));
    let parent_closed = stop.clone();
    let requested_presence = presence.clone();
    // The player's other commands wait for the loop, which holds the engine.
    let (commands, player) = std::sync::mpsc::sync_channel(16);
    std::thread::spawn(move || {
        let mut input = std::io::stdin().lock();
        while let Ok(Some(command)) = HostCommand::read(&mut input) {
            match command {
                HostCommand::SetPresence { status } => {
                    *requested_presence.lock().unwrap() = Some(HostCommand::persona_state(status));
                }
                command => {
                    if commands.send(command).is_err() {
                        break;
                    }
                }
            }
        }
        parent_closed.store(true, Ordering::Release);
    });
    let engine = Engine::open(engine_path)?;
    let (user, pipe) = engine.global();
    if user == 0 || pipe == 0 {
        return Err("Global user unavailable".into());
    }
    let client = engine.user(user, pipe)?;
    let friends = engine.friends(user, pipe)?;
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
    let mut applied_presence = None;
    let mut last_check = Instant::now() - Duration::from_secs(1);
    let mut paths = 0_u32;
    let mut requests = overlay::Throttle::new(Duration::from_millis(500));
    let mut budget = overlay::Budget::new(8, Duration::from_secs(1));
    let mut lobbies = lobby::Lobbies::new();
    let mut overlay_active = false;
    let mut wanted = vec![(overlay::REQUESTED, overlay::SIZE)];
    wanted.extend(received::CALLBACKS);
    while !stop.load(Ordering::Acquire) {
        for _ in 0..100 {
            let Some((id, payload)) = engine.callback_payload(pipe, &wanted) else {
                break;
            };
            if id == 101 {
                observed_connection = true;
            }
            if id == 103 {
                observed_connection = false;
                applied_presence = None;
            }
            // The supervisor accepts no event before Ready. The engine echoes
            // the host's own overlay state (331) here too: it is not read.
            let (true, Some(payload)) = (ready, payload) else {
                continue;
            };
            if id == overlay::REQUESTED {
                let Some(request) = overlay::request(&payload, app) else {
                    continue;
                };
                // Even a coalesced request names the game's lobby now.
                if let OverlayDialog::LobbyInvite { lobby_id } = request {
                    lobbies.named(lobby_id);
                }
                if requests.admit(request.clone(), Instant::now()) {
                    emit(
                        output,
                        &Event::OverlayRequested {
                            app_id: app,
                            request,
                        },
                    )?;
                }
            } else if let Some(event) = received::event(id, &payload, app)
                && budget.admit(Instant::now())
            {
                emit(output, &event)?;
            }
        }
        // The player's commands, once the game's Steam is ready. A refused
        // call stops nothing: the game keeps its Steam.
        while let Ok(command) = player.try_recv() {
            if ready {
                let _ = perform(&friends, &client, app, &mut overlay_active, command);
            }
        }
        if last_check.elapsed() >= Duration::from_millis(250) {
            last_check = Instant::now();
            let online = engine.connected(user, pipe)
                && engine.logged_on(user, pipe)
                && observed_connection
                && client.steam_id()? == expected_id;
            if online {
                let requested = *presence.lock().unwrap();
                if let Some(wanted) = requested
                    && applied_presence != Some(wanted)
                {
                    friends.set_presence(wanted)?;
                    applied_presence = Some(wanted);
                }
            } else {
                applied_presence = None;
            }
            if online && !authenticated {
                authenticated = true;
                emit(output, &Event::Authenticated {})?;
                client.request_ticket(app)?;
                if std::env::var_os("OBELISK_STORAGE_METADATA").is_some() {
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
                lobbies.forget();
            }
            if online {
                if disconnected {
                    emit(output, &Event::Reconnected {})?;
                }
                disconnected = false;
            }
            let now = Instant::now();
            // A refused read stops its source, never the game's Steam.
            if ready
                && online
                && lobbies.due(now)
                && let Some(lobby_id) = lobbies.read(
                    now,
                    || {
                        let info = friends.game_played(expected_id)?;
                        Ok(info.and_then(|info| lobby::current(&info, app)))
                    },
                    |lobby| engine.in_lobby(app, lobby),
                )
            {
                emit(
                    output,
                    &Event::GameLobby {
                        app_id: app,
                        lobby_id,
                    },
                )?;
            }
        }
        if !ready && deadline.elapsed() > HOST_STARTUP_TIMEOUT {
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
    if let Some(platforms) = std::env::var_os("OBELISK_STORAGE_METADATA") {
        let platforms: u32 = platforms
            .to_str()
            .and_then(|mask| mask.parse().ok())
            .ok_or("Native storage platform invalid")?;
        let home = std::env::var_os("HOME").ok_or("Native storage HOME missing")?;
        let cache = std::path::PathBuf::from(home).join(format!(
            "Library/Application Support/Steam/userdata/{}/{app}",
            expected_id as u32
        ));
        let started = Instant::now();
        while !game_runtime::steam::host::native_cache_checkpoint(cache.clone(), app, platforms) {
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

/// Carries out the player's `command` for game `app`. Steam's overlay always
/// opens before it closes: the game's Steam client drops a close it never saw
/// opened, so a close without an open, or the same state twice, is not sent.
fn perform(
    friends: &ClientFriends,
    client: &ClientUser,
    app: u32,
    overlay_active: &mut bool,
    command: HostCommand,
) -> Result<()> {
    if !command.valid() {
        return Err("Invalid host command".into());
    }
    match command {
        HostCommand::SetPresence { .. } => {}
        HostCommand::OverlayState {
            active,
            user_initiated,
        } => {
            if active != *overlay_active {
                // No overlay process exists: pid 0.
                friends.overlay_state(0, app, active, user_initiated)?;
                *overlay_active = active;
            }
        }
        HostCommand::AcceptLobbyInvite { lobby_id, friend } => {
            friends.lobby_join_requested(app, lobby_id, friend)?;
        }
        HostCommand::AcceptRichInvite { friend, connect } => {
            friends.rich_presence_join_requested(app, friend, &CString::new(connect)?)?;
        }
        HostCommand::AuthorizeMicroTxn { order_id, approve } => {
            client.authorize_microtxn(order_id, approve)?;
        }
    }
    Ok(())
}
