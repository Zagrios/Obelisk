use super::*;
fn startup() -> Startup {
    Startup::new(Credentials {
        steam_id: 1,
        account_name: "fixture".into(),
        refresh_token: "fixture-token".into(),
    })
}
pub(super) fn child(script: &str) -> process::Child {
    let mut command = std::process::Command::new("/bin/sh");
    command.arg("-c").arg(script);
    process::Child::spawn(command).unwrap()
}
pub(super) fn prepared(root: &std::path::Path) -> files::Prepared {
    files::Prepared {
        storage: None,
        licenses: None,
        directory: root.into(),
        run_directory: root.join("scratch"),
        profile: root.join("profile"),
        report: root.join("report.json"),
        endpoints: [
            ("Steam3Master".into(), "127.0.0.1:1".into()),
            ("SteamClientService".into(), "127.0.0.1:2".into()),
        ],
    }
}
pub(super) fn host(script: &str, root: &std::path::Path) -> Host {
    let host = child(script);
    let wakeup = host.wakeup.clone();
    Host {
        inner: Mutex::new(Inner {
            proof: Proof {
                app_id: 42,
                host_pid: host.id(),
                authenticated: true,
                ticket_bytes: 178,
                host_account_verified: true,
                host_license_verified: true,
                sdk_checked: false,
                game_pid: None,
                game_exit: None,
                discovery_paths: 0,
                host_state: HostState::Online,
                state: "ready".into(),
            },
            host,
            requests: GameRequests::default(),
            sequence: 0,
            stopped: false,
        }),
        wakeup,
        service: "fixture".into(),
        prepared: prepared(root),
    }
}
fn signal(host: &Host) {
    use std::io::Write;
    host.inner
        .lock()
        .unwrap()
        .host
        .input
        .as_mut()
        .unwrap()
        .write_all(b"go\n")
        .unwrap();
}
fn state(state: HostState) -> HostUpdate {
    HostUpdate::new(state)
}
fn changed(host: &Arc<Host>, previous: HostState) -> HostState {
    updated(host, &state(previous)).state
}
fn updated(host: &Arc<Host>, previous: &HostUpdate) -> HostUpdate {
    let (send, receive) = std::sync::mpsc::channel();
    let observer = host.clone();
    let previous = previous.clone();
    let worker = std::thread::spawn(move || {
        let _ = send.send(observer.next_update(&previous));
    });
    let result = receive.recv_timeout(Duration::from_secs(3));
    if result.is_err() {
        host.stopped(None);
    }
    worker.join().unwrap();
    result.expect("host state did not change")
}
#[test]
fn session_distinguishes_connection_recovery_and_host_loss_until_game_exit() {
    let root = tempfile::tempdir().unwrap();
    let host = Arc::new(host(
        r#"read line; printf '%s\n' '{"event":"disconnected"}'; read line; printf '%s\n' '{"event":"reconnected"}'; read line; exit 0"#,
        root.path(),
    ));
    host.started(1234).unwrap();
    signal(&host);
    assert_eq!(changed(&host, HostState::Online), HostState::Disconnected);
    signal(&host);
    assert_eq!(changed(&host, HostState::Disconnected), HostState::Online);
    signal(&host);
    assert_eq!(changed(&host, HostState::Online), HostState::Lost);
    {
        let inner = host.inner.lock().unwrap();
        assert!(!inner.stopped);
        assert_eq!(inner.proof.game_pid, Some(1234));
        assert_eq!(inner.proof.state, "game_started");
    }
    host.stopped(Some(0));
    assert_eq!(
        host.next_update(&state(HostState::Lost)).state,
        HostState::Stopped
    );
    let proof: serde_json::Value =
        serde_json::from_slice(&std::fs::read(root.path().join("report.json")).unwrap()).unwrap();
    assert_eq!(proof["state"], "game_exited");
    assert_eq!(proof["host_state"], "lost");
    assert_eq!(proof["game_exit"], 0);
}
#[test]
fn overlay_requests_of_the_game_wake_the_observer_with_a_new_sequence() {
    let root = tempfile::tempdir().unwrap();
    let lobby = r#"'{"event":"overlay_requested","app_id":42,"request":{"lobby_invite":{"lobby_id":109775244421126053}}}'"#;
    let script = [
        // Another game's request and an individual account as a lobby
        // are ignored; the same lobby asked twice is two requests.
        r#"read line; printf '%s\n' '{"event":"overlay_requested","app_id":7,"request":{"lobby_invite":{"lobby_id":109775244421126053}}}' '{"event":"overlay_requested","app_id":42,"request":{"lobby_invite":{"lobby_id":76561197960265729}}}' LOBBY"#,
        "read line; printf '%s\\n' LOBBY",
        r#"read line; printf '%s\n' '{"event":"overlay_requested","app_id":42,"request":{"connect_invite":{"connect":"+connect 10.0.0.1"}}}'"#,
        r#"read line; printf '%s\n' '{"event":"overlay_requested","app_id":42,"request":{"web_page":{"url":"file:///etc/passwd","modal":false}}}' '{"event":"overlay_requested","app_id":42,"request":{"store":{"app_id":42,"flag":1}}}'"#,
        r#"read line; printf '%s\n' '{"event":"disconnected"}'"#,
        r#"read line; printf '%s\n' '{"event":"reconnected"}'"#,
        "read line; printf '%s\\n' LOBBY",
        "read line",
    ]
    .join("; ")
    .replace("LOBBY", lobby);
    let host = Arc::new(host(&script, root.path()));
    host.started(1234).unwrap();
    let request = |sequence, dialog: &protocol::OverlayDialog| OverlayRequest {
        sequence,
        dialog: dialog.clone(),
    };
    let invite = |sequence, target: &protocol::InviteTarget| InviteRequest {
        sequence,
        target: target.clone(),
    };
    let dialog = protocol::OverlayDialog::LobbyInvite {
        lobby_id: 109_775_244_421_126_053,
    };
    let target = protocol::InviteTarget::Lobby {
        lobby_id: 109_775_244_421_126_053,
    };
    signal(&host);
    let first = updated(&host, &state(HostState::Online));
    assert_eq!(first.state, HostState::Online);
    assert_eq!(first.requests.overlay, Some(request(1, &dialog)));
    assert_eq!(first.requests.invite(), Some(invite(1, &target)));
    signal(&host);
    let second = updated(&host, &first);
    assert_eq!(second.requests.invite(), Some(invite(2, &target)));
    signal(&host);
    let third = updated(&host, &second);
    let connect = protocol::InviteTarget::Connect {
        connect: "+connect 10.0.0.1".into(),
    };
    assert_eq!(third.requests.invite(), Some(invite(3, &connect)));
    signal(&host);
    let store = updated(&host, &third);
    assert_eq!(
        store.requests.overlay,
        Some(request(
            4,
            &protocol::OverlayDialog::Store {
                app_id: 42,
                flag: 1
            }
        )),
        "a file URL is no web page"
    );
    assert_eq!(
        store.requests.invite(),
        None,
        "the store is no invite dialog"
    );
    signal(&host);
    let disconnected = updated(&host, &store);
    assert_eq!(
        disconnected,
        state(HostState::Disconnected),
        "off Steam, the dialog's target is gone"
    );
    signal(&host);
    let reconnected = updated(&host, &disconnected);
    assert_eq!(reconnected, state(HostState::Online), "and it stays gone");
    signal(&host);
    let again = updated(&host, &reconnected);
    assert_eq!(
        again.requests.invite(),
        Some(invite(5, &target)),
        "the session's sequence keeps growing"
    );
    host.stopped(Some(0));
    assert_eq!(
        host.next_update(&again),
        state(HostState::Stopped),
        "nothing outlives the game"
    );
}
const FRIEND: u64 = 76_561_197_960_265_729;
const OTHER: u64 = 76_561_197_960_265_730;
const LOBBY: u64 = 109_775_244_421_126_053;
/// A host that tells `events`, one batch per signal, then records every
/// command it gets into `input`.
fn recording(root: &std::path::Path, events: &[&str]) -> Arc<Host> {
    let mut script: Vec<String> = events
        .iter()
        .map(|batch| format!("read line; printf '%s\\n' {batch}"))
        .collect();
    script.push(format!("cat > '{}'", root.join("input").display()));
    let host = Arc::new(host(&script.join("; "), root));
    host.started(1234).unwrap();
    host
}
fn commands(root: &std::path::Path) -> Vec<protocol::HostCommand> {
    let wire = std::fs::read(root.join("input")).unwrap();
    let mut input = wire.as_slice();
    let mut commands = Vec::new();
    while let Some(command) = protocol::HostCommand::read(&mut input).unwrap() {
        commands.push(command);
    }
    commands
}
/// The first update after `previous` that is `done`: a batch of events
/// may wake the observer more than once.
fn until(
    host: &Arc<Host>,
    previous: &HostUpdate,
    done: impl Fn(&HostUpdate) -> bool,
) -> HostUpdate {
    let mut update = updated(host, previous);
    while !done(&update) {
        update = updated(host, &update);
    }
    update
}
fn received(friend: u64, target: protocol::InviteTarget) -> (u64, protocol::InviteTarget) {
    (friend, target)
}
fn invites(update: &HostUpdate) -> Vec<(u64, protocol::InviteTarget)> {
    update
        .requests
        .invites
        .iter()
        .map(|invite| received(invite.friend, invite.target.clone()))
        .collect()
}
#[test]
fn received_invites_of_the_game_come_newest_first_once_each() {
    let root = tempfile::tempdir().unwrap();
    let lobby = format!(
        r#"'{{"event":"lobby_invite_received","inviter":{FRIEND},"lobby_id":{LOBBY},"app_id":42}}'"#
    );
    let rich = format!(
        r#"'{{"event":"rich_invite_received","friend":{OTHER},"app_id":42,"connect":"+connect 10.0.0.1"}}'"#
    );
    let ignored = [
        format!(r#"'{{"event":"lobby_invite_received","inviter":{FRIEND},"lobby_id":{LOBBY},"app_id":7}}'"#),
        format!(r#"'{{"event":"lobby_invite_received","inviter":{LOBBY},"lobby_id":{LOBBY},"app_id":42}}'"#),
        format!(r#"'{{"event":"lobby_invite_received","inviter":{FRIEND},"lobby_id":{FRIEND},"app_id":42}}'"#),
        format!(r#"'{{"event":"rich_invite_received","friend":{OTHER},"app_id":42,"connect":""}}'"#),
    ]
    .join(" ");
    let many: String = (1..=20)
        .map(|account| {
            format!(
                r#"'{{"event":"rich_invite_received","friend":{},"app_id":42,"connect":"+connect 10.0.0.{account}"}}'"#,
                FRIEND + account
            )
        })
        .collect::<Vec<_>>()
        .join(" ");
    let batches = [
        format!("{lobby} {ignored} {rich}"),
        lobby.clone(),
        r#"'{"event":"disconnected"}'"#.to_owned(),
        r#"'{"event":"reconnected"}'"#.to_owned(),
        many,
    ];
    let host = recording(
        root.path(),
        &batches.iter().map(String::as_str).collect::<Vec<_>>(),
    );
    let lobby = || received(FRIEND, protocol::InviteTarget::Lobby { lobby_id: LOBBY });
    let rich = || {
        received(
            OTHER,
            protocol::InviteTarget::Connect {
                connect: "+connect 10.0.0.1".into(),
            },
        )
    };
    signal(&host);
    let first = until(&host, &state(HostState::Online), |update| {
        update.requests.invites.len() == 2
    });
    assert_eq!(invites(&first), [rich(), lobby()]);
    signal(&host);
    let second = updated(&host, &first);
    assert_eq!(invites(&second), [lobby(), rich()], "again, first and once");
    signal(&host);
    let away = updated(&host, &second);
    assert_eq!(
        away,
        state(HostState::Disconnected),
        "off Steam, none shows"
    );
    signal(&host);
    let back = updated(&host, &away);
    assert_eq!(invites(&back), [lobby(), rich()], "Steam still holds them");
    signal(&host);
    let many = until(&host, &back, |update| {
        update.requests.invites.first().map(|invite| invite.friend) == Some(FRIEND + 20)
    });
    assert_eq!(many.requests.invites.len(), MAX_RECEIVED_INVITES);
    assert_eq!(many.requests.invites[0].friend, FRIEND + 20);
    assert_eq!(
        many.requests.invites[15].friend,
        FRIEND + 5,
        "the oldest go"
    );
    // Accepting one hands it to the game and takes it off the list.
    host.accept_invite(
        FRIEND + 20,
        protocol::InviteTarget::Connect {
            connect: "+connect 10.0.0.20".into(),
        },
    )
    .unwrap();
    let accepted = updated(&host, &many);
    assert_eq!(accepted.requests.invites.len(), MAX_RECEIVED_INVITES - 1);
    assert_eq!(accepted.requests.invites[0].friend, FRIEND + 19);
    // An invitation from Steam's chat, never told by the host.
    host.accept_invite(FRIEND, protocol::InviteTarget::Lobby { lobby_id: LOBBY })
        .unwrap();
    assert!(matches!(
        host.accept_invite(LOBBY, protocol::InviteTarget::Lobby { lobby_id: LOBBY }),
        Err(Error::InvalidSession)
    ));
    // The supervisor tells the player's controls apart the same way.
    assert_eq!(
        host.control(Control::AuthorizeMicroTxn {
            order_id: 13,
            approve: true
        }),
        Err(supervisor::Error::InvalidSession)
    );
    host.stopped(Some(0));
    assert_eq!(host.next_update(&accepted), state(HostState::Stopped));
    assert_eq!(
        commands(root.path()),
        [
            protocol::HostCommand::AcceptRichInvite {
                friend: FRIEND + 20,
                connect: "+connect 10.0.0.20".into(),
            },
            protocol::HostCommand::AcceptLobbyInvite {
                lobby_id: LOBBY,
                friend: FRIEND,
            },
        ]
    );
    assert!(matches!(
        host.overlay_state(true, true),
        Err(Error::Unavailable)
    ));
    assert_eq!(
        host.control(Control::Overlay {
            active: true,
            user_initiated: true
        }),
        Err(supervisor::Error::Unavailable)
    );
}
#[test]
fn only_the_purchase_the_game_waits_on_is_answered_once() {
    let root = tempfile::tempdir().unwrap();
    let host = recording(
        root.path(),
        &[
            r#"'{"event":"micro_txn_requested","order_id":7,"app_id":7}' '{"event":"micro_txn_requested","order_id":11,"app_id":42}'"#,
            r#"'{"event":"micro_txn_dismissed","order_id":12}' '{"event":"micro_txn_requested","order_id":12,"app_id":42}'"#,
            r#"'{"event":"micro_txn_dismissed","order_id":12}'"#,
            r#"'{"event":"micro_txn_requested","order_id":13,"app_id":42}'"#,
        ],
    );
    let pending = |order_id| {
        Some(MicroTxn {
            order_id,
            app_id: 42,
        })
    };
    signal(&host);
    let first = updated(&host, &state(HostState::Online));
    assert_eq!(first.requests.microtxn, pending(11), "only the game's own");
    signal(&host);
    let second = updated(&host, &first);
    assert_eq!(
        second.requests.microtxn,
        pending(12),
        "the latest replaces it"
    );
    signal(&host);
    let dismissed = updated(&host, &second);
    assert_eq!(dismissed.requests.microtxn, None);
    assert!(matches!(
        host.authorize_microtxn(12, true),
        Err(Error::InvalidSession)
    ));
    signal(&host);
    let third = updated(&host, &dismissed);
    assert_eq!(third.requests.microtxn, pending(13));
    assert!(matches!(
        host.authorize_microtxn(11, true),
        Err(Error::InvalidSession)
    ));
    host.authorize_microtxn(13, false).unwrap();
    let answered = updated(&host, &third);
    assert_eq!(answered.requests.microtxn, None, "answered once");
    assert!(matches!(
        host.authorize_microtxn(13, true),
        Err(Error::InvalidSession)
    ));
    host.overlay_state(true, false).unwrap();
    host.overlay_state(false, false).unwrap();
    assert_eq!(
        host.control(Control::AuthorizeMicroTxn {
            order_id: 13,
            approve: true
        }),
        Err(supervisor::Error::InvalidSession)
    );
    host.stopped(Some(0));
    assert_eq!(
        commands(root.path()),
        [
            protocol::HostCommand::AuthorizeMicroTxn {
                order_id: 13,
                approve: false,
            },
            protocol::HostCommand::OverlayState {
                active: true,
                user_initiated: false,
            },
            protocol::HostCommand::OverlayState {
                active: false,
                user_initiated: false,
            },
        ]
    );
    assert_eq!(
        host.control(Control::Overlay {
            active: true,
            user_initiated: true
        }),
        Err(supervisor::Error::Unavailable)
    );
}
#[test]
fn the_engines_lobby_follows_the_game_while_the_host_is_online() {
    let root = tempfile::tempdir().unwrap();
    let script = [
        r#"read line; printf '%s\n' '{"event":"game_lobby","app_id":7,"lobby_id":109775244421126053}' '{"event":"game_lobby","app_id":42,"lobby_id":109775244421126053}'"#,
        r#"read line; printf '%s\n' '{"event":"game_lobby","app_id":42,"lobby_id":109775244423207541}'"#,
        r#"read line; printf '%s\n' '{"event":"game_lobby","app_id":42,"lobby_id":76561197960265729}'"#,
        r#"read line; printf '%s\n' '{"event":"game_lobby","app_id":42,"lobby_id":109775244421126053}'"#,
        r#"read line; printf '%s\n' '{"event":"disconnected"}'"#,
        "read line",
    ]
    .join("; ");
    let host = Arc::new(host(&script, root.path()));
    host.started(1234).unwrap();
    let lobby = |lobby| HostUpdate {
        requests: GameRequests {
            lobby,
            ..GameRequests::default()
        },
        ..state(HostState::Online)
    };
    signal(&host);
    let first = updated(&host, &state(HostState::Online));
    assert_eq!(first, lobby(Some(109_775_244_421_126_053)));
    signal(&host);
    let second = updated(&host, &first);
    assert_eq!(second, lobby(Some(109_775_244_423_207_541)), "a new run");
    signal(&host);
    let third = updated(&host, &second);
    assert_eq!(third, lobby(None), "an account is no lobby");
    signal(&host);
    let fourth = updated(&host, &third);
    assert_eq!(fourth, lobby(Some(109_775_244_421_126_053)));
    signal(&host);
    assert_eq!(
        updated(&host, &fourth),
        state(HostState::Disconnected),
        "off Steam, the game is in no lobby"
    );
    host.stopped(Some(0));
}
#[test]
fn game_exit_wakes_observer_and_immediate_exit_cannot_restore_started_state() {
    let root = tempfile::tempdir().unwrap();
    let host = Arc::new(host("read line", root.path()));
    host.started(1234).unwrap();
    let observer = host.clone();
    let worker = std::thread::spawn(move || observer.next_update(&state(HostState::Online)).state);
    host.stopped(Some(0));
    assert_eq!(worker.join().unwrap(), HostState::Stopped);
    host.started(5678).unwrap();
    assert_eq!(host.inner.lock().unwrap().proof.game_pid, Some(1234));
    assert_eq!(host.inner.lock().unwrap().proof.state, "game_exited");
    host.stopped(Some(9));
    assert_eq!(host.inner.lock().unwrap().proof.game_exit, Some(0));
}
#[test]
fn host_loss_after_spawn_still_records_the_live_game() {
    let root = tempfile::tempdir().unwrap();
    let host = host("read line", root.path());
    host.inner.lock().unwrap().host.stop();
    host.started(1234).unwrap();
    assert_eq!(
        host.next_update(&state(HostState::Online)).state,
        HostState::Lost
    );
    assert!(!host.inner.lock().unwrap().stopped);
    host.stopped(Some(0));
}
#[test]
fn readiness_requires_authentication_matching_app_pid_and_nonempty_ticket() {
    for (prefix, app, pid, ticket) in [
        ("", 1030300, "$$", 178),
        (
            "printf '%s\\n' '{\"event\":\"authenticated\"}';",
            999,
            "$$",
            178,
        ),
        (
            "printf '%s\\n' '{\"event\":\"authenticated\"}';",
            1030300,
            "0",
            178,
        ),
        (
            "printf '%s\\n' '{\"event\":\"authenticated\"}';",
            1030300,
            "$$",
            0,
        ),
    ] {
        let mut host = child(&format!(
            "{prefix} printf '{{\"event\":\"ready\",\"app_id\":{app},\"host_pid\":%s,\"ticket_bytes\":{ticket}}}\\n' \"{pid}\"; read line"
        ));
        assert!(matches!(
            startup().wait_for_host(&mut host, 1030300, &mut Vec::new()),
            Err(Error::StartFailed)
        ));
        host.stop();
        assert!(!host.alive().unwrap());
    }
    let mut host = child(
        "printf '%s\\n' '{\"event\":\"authenticated\"}'; printf '{\"event\":\"ready\",\"app_id\":1030300,\"host_pid\":%s,\"ticket_bytes\":178}\\n' \"$$\"; read line",
    );
    assert_eq!(
        startup()
            .wait_for_host(&mut host, 1030300, &mut Vec::new())
            .unwrap(),
        178
    );
    host.stop();
    assert!(!host.alive().unwrap());
}
#[test]
fn cancellation_interrupts_waiting_for_a_live_helper() {
    let startup = Arc::new(startup());
    let mut host = child("read line");
    let cancel = startup.clone();
    let cancellation = std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(100));
        cancel.cancel();
    });
    let started = Instant::now();
    assert!(matches!(
        startup.wait_for_host(&mut host, 1030300, &mut Vec::new()),
        Err(Error::Cancelled)
    ));
    host.stop();
    cancellation.join().unwrap();
    assert!(!host.alive().unwrap());
    assert!(started.elapsed() < Duration::from_secs(2));
}
#[test]
fn dead_or_malformed_helper_never_becomes_ready() {
    for script in ["exit 1", "printf '%s\\n' 'native token=fixture'"] {
        let mut host = child(script);
        assert!(matches!(
            startup().wait_for_host(&mut host, 1030300, &mut Vec::new()),
            Err(Error::StartFailed)
        ));
    }
}
