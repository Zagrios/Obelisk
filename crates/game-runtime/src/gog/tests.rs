use super::*;
use crate::{launch_shell, supervisor::Provider as _};

fn configuration(platform: Platform, prefix: Option<&str>) -> Configuration {
    Configuration {
        home: "/Users/player".into(),
        install: "/library/Games/GOG/gog-1207658924-windows".into(),
        session_directory: "/library/GOGSessions".into(),
        helper: "/Obelisk.app/Contents/Helpers/SteamNative/obelisk-steam-host".into(),
        product_id: "1207658924".into(),
        platform,
        title: "The Witcher".into(),
        prefix: prefix.map(Into::into),
    }
}

#[test]
fn a_configuration_keeps_gog_s_spelling() {
    let mac = serde_json::to_value(configuration(Platform::Mac, None)).unwrap();
    assert_eq!(mac["platform"], "macos");
    let windows = configuration(
        Platform::Windows,
        Some("/library/Prefixes/gog-1207658924-windows"),
    );
    let written = serde_json::to_value(&windows).unwrap();
    assert_eq!(written["platform"], "windows");
    assert_eq!(
        serde_json::from_value::<Configuration>(written).unwrap(),
        windows
    );
}

#[test]
fn a_configuration_from_before_titles_still_loads() {
    let saved = serde_json::json!({
        "home": "/Users/player",
        "install": "/library/Games/GOG/gog-1207658924-windows",
        "session_directory": "/library/GOGSessions",
        "helper": "/Obelisk.app/Contents/Helpers/SteamNative/obelisk-steam-host",
        "product_id": "1207658924",
        "platform": "windows",
        "prefix": "/library/Prefixes/gog-1207658924-windows",
    });
    let loaded: Configuration = serde_json::from_value(saved).unwrap();
    assert_eq!(
        loaded,
        Configuration {
            title: String::new(),
            ..configuration(
                Platform::Windows,
                Some("/library/Prefixes/gog-1207658924-windows")
            )
        }
    );
}

#[test]
fn a_cancelled_gog_launch_ends_silently() {
    assert!(Gog::cancelled(&Error::Cancelled));
    assert!(Gog::cancelled(&Error::Gog(gog_kit::GogError::Cancelled)));
    assert!(!Gog::cancelled(&Error::Session));
}

const ACCOUNT: &str = "48628349957132247";

#[test]
fn a_gog_configuration_is_unsupported_unless_its_ids_are_valid() {
    let account = ACCOUNT;
    let mac = || configuration(Platform::Mac, None);
    for product_id in ["../1207658924", "", "1207658924x", &"1".repeat(21)] {
        let refused = Configuration {
            product_id: product_id.into(),
            ..mac()
        };
        let error = launch_shell::validate::<Gog>(&refused, account).unwrap_err();
        assert!(matches!(error, Error::Unsupported), "{refused:?}");
        assert_eq!(error.to_string(), "backend.gog.unsupported");
    }
    for account in [
        "not-a-user",
        "",
        "48628349957132247x",
        "-1",
        &"4".repeat(33),
    ] {
        let error = launch_shell::validate::<Gog>(&mac(), account).unwrap_err();
        assert!(matches!(error, Error::Unsupported), "{account}");
    }
    // The longest ids still pass.
    let longest = Configuration {
        product_id: "1".repeat(20),
        ..mac()
    };
    launch_shell::validate::<Gog>(&longest, &"4".repeat(32)).unwrap();
    let windows = configuration(
        Platform::Windows,
        Some("/library/Prefixes/gog-1207658924-windows"),
    );
    launch_shell::validate::<Gog>(&windows, account).unwrap();
}

fn snapshot(cloud: crate::cloud::Status) -> Snapshot {
    Snapshot {
        version: crate::supervisor::journal::VERSION,
        id: uuid::Uuid::new_v4().to_string(),
        account_id: ACCOUNT.into(),
        store: Store {
            product_id: "1207658924".into(),
        },
        phase: Phase::Finished,
        cloud,
        exit_code: None,
        played: None,
        error: None,
        failure_reason: None,
        revision: 1,
        rosetta: None,
        reconnecting: false,
        supervisor: crate::supervisor::Identity {
            pid: 1,
            uid: 1,
            birth_seconds: 0,
            birth_microseconds: 0,
            executable: "supervisor".into(),
        },
        game: None,
        starting: None,
        socket: "/library/GOGSessions/socket".into(),
    }
}

#[test]
fn a_gog_session_settles_its_cloud_as_unsupported_and_offers_no_choice() {
    use crate::supervisor::session::Launch as _;
    for (phase, choices) in [
        (crate::cloud::Phase::Idle, vec![]),
        (
            crate::cloud::Phase::Conflict,
            vec![crate::cloud::Choice::Local, crate::cloud::Choice::Cloud],
        ),
    ] {
        let mut record = snapshot(crate::cloud::Status {
            phase,
            allowed_choices: choices,
            ..Default::default()
        });

        launch::Launch::terminal(&mut record);

        assert_eq!(record.cloud.phase, crate::cloud::Phase::Unsupported);
        assert_eq!(record.cloud.allowed_choices, []);
    }
}

#[test]
fn a_gog_record_names_a_real_account_product_and_socket_and_an_orphan_offers_no_choice() {
    let valid = snapshot(Default::default());
    assert!(Gog::valid(&valid));
    for broken in [
        Snapshot {
            account_id: "not-a-user".into(),
            ..valid.clone()
        },
        Snapshot {
            store: Store {
                product_id: "../1".into(),
            },
            ..valid.clone()
        },
        Snapshot {
            socket: "relative.sock".into(),
            ..valid.clone()
        },
    ] {
        assert!(!Gog::valid(&broken));
    }

    let mut asked = snapshot(crate::cloud::Status {
        allowed_choices: vec![crate::cloud::Choice::Cancel],
        ..Default::default()
    });
    Gog::orphaned(&mut asked, true);
    assert!(
        asked.reconnecting,
        "a game that still runs is reconnected to"
    );
    assert_eq!(asked.cloud.allowed_choices, []);
    let mut gone = valid;
    Gog::orphaned(&mut gone, false);
    assert!(!gone.reconnecting);
}
