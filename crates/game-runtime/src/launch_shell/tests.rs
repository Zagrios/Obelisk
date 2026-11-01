//! The launch shell's scenarios, run for Epic and for GOG: what a build's
//! configuration may be, what a supervisor is handed and what it makes of it,
//! the target it starts, and a launch from an installed receipt to the
//! supervisor's start. A storefront's fixture supplies the rest: its names,
//! its paths and the wire as it spells them. Scenarios of one storefront alone
//! stay with that storefront.
mod guest_locale;

use super::handover::Handover;
use super::request::start;
use super::*;
use crate::{
    Locations,
    installations::{Receipt, tests::eventually},
    runtimes::{
        Runtime, Runtimes,
        tests::{FakeRuntimes, fake_offer, ready, recorded},
    },
    sessions::{self, Admission, BeforeBegin, BoxFuture, Failure, Sessions},
    supervisor::{
        Provider,
        architecture::{
            Architecture,
            tests::{ARM64, INTEL, thin},
        },
        session::{
            Opening,
            tests::{HandedOver, handed_over, write_recording_supervisor},
        },
    },
    wine::tests::{calls, config, installation, pe, record_calls, target as plan},
};
use obelisk_core::transfer::CancelToken;
use serde_json::{Value, json};
use std::{
    collections::HashMap,
    fs,
    io::Read,
    os::unix::fs::PermissionsExt,
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};

type Configuration<S> = <S as Launchable>::Configuration;
type Error<S> = <S as Provider>::Error;
/// What a supervisor makes of a handoff: its session and the Wine it carries.
pub(crate) type Opened<S> = (Opening<S>, Option<wine::Config>);

/// What a storefront's build-level scenarios need: its names, a configuration
/// of its own, and the handoff as the wire spells it. Spelled out here, not
/// derived from the types under test.
pub(crate) trait Setup: 'static {
    type G: Launchable;
    const SUPERVISE: &'static str;
    /// The key of the storefront's `Unsupported`.
    const UNSUPPORTED: &'static str;
    /// The key of the storefront's `Session`.
    const SESSION: &'static str;
    const CANCELLED: &'static str;
    /// The key a configuration with an account the storefront would not name
    /// shows.
    const BAD_ACCOUNT: &'static str;
    /// How the wire spells a Windows build's platform.
    const WINDOWS: &'static str;
    /// The game's name in the store, as its configuration carries it.
    const TITLE: &'static str;
    /// The game as Obelisk's profiles and logs name it.
    const GAME: profile::Game<'static>;
    const LAUNCH_KEYS: [(launch::Error, &'static str); 7];
    const SUPERVISOR_KEYS: [(supervisor::Error, &'static str); 8];

    fn account() -> String;
    fn credential() -> <Self::G as Launchable>::Credential;
    /// The keys and values the credential adds to the wire.
    fn credential_json() -> Value;
    /// A build of `platform` in the folders under `root`.
    fn configuration_in(
        root: &Path,
        platform: Platform,
        prefix: Option<PathBuf>,
    ) -> Configuration<Self::G>;
    /// The configuration as the wire spells it.
    fn wire(configuration: &Configuration<Self::G>) -> Value;
    /// The paths every configuration holds: home, installation, sessions,
    /// helper.
    fn paths(configuration: &mut Configuration<Self::G>) -> [&mut PathBuf; 4];
    /// Configurations the storefront alone refuses, with their account and the
    /// key they show.
    fn refused_by_the_store(root: &Path) -> Vec<(Configuration<Self::G>, String, &'static str)>;
    /// A complete handoff for a Mac build.
    fn handoff() -> Value;
    /// Where its supervisor keeps its session.
    fn session_root() -> &'static str;
    fn store() -> <Self::G as Provider>::Store;
    /// The supervisor's `Launch::open`.
    fn open(input: &mut dyn Read) -> Result<Opened<Self::G>, Error<Self::G>>;
}

/// A request for `configuration` as the shell builds it, run in `wine`.
fn request<F: Setup>(
    configuration: Configuration<F::G>,
    wine: Option<wine::Config>,
) -> Request<F::G> {
    Request::new(configuration, F::account(), F::credential(), wine)
}

/// The key `result` failed with.
fn key<T>(result: Result<T, impl std::fmt::Display>) -> Option<String> {
    result.err().map(|error| error.to_string())
}

/// The names of `value`'s fields, sorted.
pub(crate) fn fields(value: &Value) -> Vec<&str> {
    value
        .as_object()
        .unwrap()
        .keys()
        .map(String::as_str)
        .collect()
}

fn wine_in(root: &Path, prefix: &Path) -> wine::Config {
    wine::Config {
        prefix: prefix.to_owned(),
        ..config(root)
    }
}

pub(crate) fn a_build_runs_natively_or_only_in_the_prefix_its_configuration_names<F: Setup>() {
    let (_directory, root) = installation();
    let prefix = root.join("library/Prefixes/launch");
    let mac = || F::configuration_in(&root, Platform::Mac, None);
    let windows =
        |prefix: &Path| F::configuration_in(&root, Platform::Windows, Some(prefix.to_owned()));
    let account = F::account();
    let refusal =
        |configuration: &Configuration<F::G>| key(validate::<F::G>(configuration, &account));

    assert_eq!(refusal(&mac()), None);
    assert_eq!(refusal(&windows(&prefix)), None);
    // A Mac build runs natively; a Windows build in the prefix it names.
    for refused in [
        F::configuration_in(&root, Platform::Mac, Some(prefix.clone())),
        F::configuration_in(&root, Platform::Windows, None),
        windows(Path::new("relative/prefix")),
        windows(&root.join("library/Prefixes/../launch")),
    ] {
        assert_eq!(refusal(&refused).as_deref(), Some(F::UNSUPPORTED));
    }
    // Every path is plain and absolute.
    for index in 0..4 {
        for path in ["relative", "../up", "/library/../x"] {
            let mut refused = mac();
            *F::paths(&mut refused)[index] = path.into();
            assert_eq!(
                refusal(&refused).as_deref(),
                Some(F::UNSUPPORTED),
                "{index} {path}"
            );
        }
    }

    // Wine runs only the prefix the configuration names, and only a Windows
    // build.
    let wine = wine_in(&root, &prefix);
    let other = wine_in(&root, &root.join("library/Prefixes/other"));
    let (windows, mac) = (windows(&prefix), mac());
    let (windows, mac) = (F::G::build(&windows), F::G::build(&mac));
    assert!(windows.runs_in(Some(&wine)));
    assert!(!windows.runs_in(Some(&other)));
    assert!(!windows.runs_in(None));
    assert!(mac.runs_in(None));
    assert!(!mac.runs_in(Some(&wine)));
    // A build without a prefix, or with one that is not Wine's, runs nowhere.
    for refused in [
        F::configuration_in(&root, Platform::Windows, None),
        F::configuration_in(&root, Platform::Windows, Some("relative/prefix".into())),
    ] {
        assert!(!F::G::build(&refused).runs_in(Some(&wine)));
    }
}

/// Where a started supervisor's handoff was recorded, and the build it was
/// asked to start.
struct Handoff<F: Setup> {
    _directory: tempfile::TempDir,
    root: PathBuf,
    config: Configuration<F::G>,
}

impl<F: Setup> Handoff<F> {
    fn of(platform: Platform) -> Self {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path().canonicalize().unwrap();
        let prefix = (platform == Platform::Windows).then(|| root.join("library/Prefixes/launch"));
        let config = F::configuration_in(&root, platform, prefix);
        write_recording_supervisor(F::G::build(&config).helper, &root.join("handoff"));
        Self {
            _directory: directory,
            root,
            config,
        }
    }

    fn handed_over(&self) -> Option<HandedOver> {
        crate::supervisor::session::tests::handed_over(&self.root.join("handoff"))
    }

    /// Starts the supervisor, which never acknowledges: the start fails after
    /// the handoff.
    fn started(&self, wine: Option<wine::Config>) -> Option<String> {
        key(start::<F::G>(
            self.config.clone(),
            F::account(),
            F::credential(),
            wine,
            CancelToken::new(),
        ))
    }
}

pub(crate) fn a_mac_build_hands_its_supervisor_this_request_and_nothing_more<F: Setup>() {
    let handoff = Handoff::<F>::of(Platform::Mac);

    let started = handoff.started(None);

    assert_eq!(started.as_deref(), Some(F::SESSION));
    let handed = handoff.handed_over().expect("the supervisor was started");
    assert_eq!(handed.arguments, [F::SUPERVISE]);
    assert_eq!(
        handed.environment,
        ["PATH=/usr/bin:/bin"],
        "a supervisor starts without any environment of Obelisk's"
    );
    let request = handed.request;
    let mut expected = vec!["account_id", "config", "environment", "id", "wine"];
    let credential = F::credential_json();
    expected.extend(fields(&credential));
    expected.sort_unstable();
    assert_eq!(fields(&request), expected);
    assert!(uuid::Uuid::parse_str(request["id"].as_str().unwrap()).is_ok());
    assert_eq!(request["account_id"], F::account());
    for (field, value) in credential.as_object().unwrap() {
        assert_eq!(&request[field], value, "{field}");
    }
    assert_eq!(
        request["environment"],
        serde_json::to_value(crate::native::environment(
            F::G::build(&handoff.config).home
        ))
        .unwrap(),
        "built in Obelisk's process, from the game's home"
    );
    assert_eq!(request["wine"], Value::Null);
    assert_eq!(request["config"], F::wire(&handoff.config));
}

pub(crate) fn a_windows_build_hands_its_supervisor_the_wine_that_runs_it_whole<F: Setup>() {
    let handoff = Handoff::<F>::of(Platform::Windows);
    let prefix = F::G::build(&handoff.config).prefix.unwrap().to_owned();
    let wine = wine_in(&handoff.root, &prefix);

    let started = handoff.started(Some(wine.clone()));

    assert_eq!(started.as_deref(), Some(F::SESSION));
    let request = handoff.handed_over().unwrap().request;
    assert_eq!(request["config"], F::wire(&handoff.config));
    assert_eq!(request["config"]["platform"], F::WINDOWS);
    assert_eq!(request["config"]["prefix"], json!(prefix));
    assert_eq!(request["wine"], serde_json::to_value(&wine).unwrap());
    assert_eq!(
        fields(&request["wine"]),
        ["log", "prefix", "runtime", "settings"]
    );
}

pub(crate) fn a_start_that_cannot_run_the_build_spawns_no_supervisor<F: Setup>() {
    let mac = Handoff::<F>::of(Platform::Mac);
    let windows = Handoff::<F>::of(Platform::Windows);
    let wine_of =
        |handoff: &Handoff<F>, prefix: &str| wine_in(&handoff.root, &handoff.root.join(prefix));
    let mut relative = mac.config.clone();
    *F::paths(&mut relative)[0] = "relative".into();
    let refusals = [
        // A Mac build runs natively: Wine has no place in it.
        (
            &mac,
            mac.config.clone(),
            Some(wine_of(&mac, "x")),
            F::account(),
        ),
        // A Windows build runs in the prefix its configuration names, in Wine.
        (&windows, windows.config.clone(), None, F::account()),
        (
            &windows,
            windows.config.clone(),
            Some(wine_of(&windows, "library/Prefixes/other")),
            F::account(),
        ),
        (&mac, relative, None, F::account()),
    ];
    for (handoff, config, wine, account) in refusals {
        let started = key(start::<F::G>(
            config,
            account,
            F::credential(),
            wine,
            CancelToken::new(),
        ));
        assert_eq!(started.as_deref(), Some(F::UNSUPPORTED));
        assert!(handoff.handed_over().is_none());
    }

    // What only the storefront refuses, before any supervisor starts.
    for (config, account, shown) in F::refused_by_the_store(&mac.root) {
        let started = key(start::<F::G>(
            config,
            account,
            F::credential(),
            None,
            CancelToken::new(),
        ));
        assert_eq!(started.as_deref(), Some(shown));
        assert!(mac.handed_over().is_none());
    }

    // A start cancelled before it begins spawns nothing either.
    let cancelled = CancelToken::new();
    cancelled.cancel();
    let started = key(start::<F::G>(
        mac.config.clone(),
        F::account(),
        F::credential(),
        None,
        cancelled,
    ));
    assert_eq!(started.as_deref(), Some(F::CANCELLED));
    assert!(mac.handed_over().is_none());
}

/// `F::open` on a packet holding `handoff`.
fn opened<F: Setup>(handoff: &Value) -> Result<Opened<F::G>, Error<F::G>> {
    let mut packet = serde_json::to_vec(handoff).unwrap();
    packet.push(b'\n');
    F::open(&mut packet.as_slice())
}

pub(crate) fn a_supervisor_opens_the_session_its_handoff_names<F: Setup>() {
    let (opening, wine) = opened::<F>(&F::handoff()).unwrap();

    assert_eq!(opening.root, Path::new(F::session_root()));
    assert_eq!(opening.id, "6f1c1a0e-8a0f-4a3e-9a7b-0d2f6f3a8b11");
    assert_eq!(opening.account_id, F::account());
    assert_eq!(opening.store, F::store());
    assert_eq!(
        opening.helpers,
        Path::new("/Obelisk.app/Contents/Helpers"),
        "the Rosetta helper app sits beside the storefront helpers"
    );
    assert_eq!(wine, None);
}

pub(crate) fn a_supervisor_runs_a_windows_build_in_the_wine_its_handoff_carries<F: Setup>() {
    let wine = config(Path::new("/root"));
    let mut windows = F::handoff();
    windows["config"]["platform"] = F::WINDOWS.into();
    windows["config"]["prefix"] = json!(wine.prefix);
    windows["wine"] = serde_json::to_value(&wine).unwrap();

    let (_, carried) = opened::<F>(&windows).unwrap();

    assert_eq!(carried, Some(wine.clone()));
    // Wine runs only the prefix the configuration names, and only a Windows
    // build.
    let session = |handoff: &Value| key(opened::<F>(handoff));
    windows["wine"]["prefix"] = "/root/another".into();
    assert_eq!(session(&windows).as_deref(), Some(F::SESSION));
    windows["wine"] = Value::Null;
    assert_eq!(session(&windows).as_deref(), Some(F::SESSION));
    let mut mac = F::handoff();
    mac["wine"] = serde_json::to_value(&wine).unwrap();
    assert_eq!(session(&mac).as_deref(), Some(F::SESSION));
}

pub(crate) fn a_supervisor_refuses_a_handoff_it_cannot_trust_with_the_key_its_player_sees<
    F: Setup,
>() {
    let with = |change: &dyn Fn(&mut Value)| {
        let mut handoff = F::handoff();
        change(&mut handoff);
        key(opened::<F>(&handoff))
    };
    // Validation comes first: an unusable account is the storefront's, a path
    // unsupported.
    assert_eq!(
        with(&|h| h["account_id"] = "not-an-account".into()).as_deref(),
        Some(F::BAD_ACCOUNT)
    );
    assert_eq!(
        with(&|h| h["config"]["install"] = "relative".into()).as_deref(),
        Some(F::UNSUPPORTED)
    );
    assert_eq!(
        with(&|h| {
            h["config"]["install"] = "relative".into();
            h["id"] = "not-a-uuid".into();
        })
        .as_deref(),
        Some(F::UNSUPPORTED)
    );
    assert_eq!(
        with(&|h| {
            h["account_id"] = "not-an-account".into();
            h["id"] = "not-a-uuid".into();
        })
        .as_deref(),
        Some(F::BAD_ACCOUNT)
    );
    // Then the session's own facts.
    assert_eq!(
        with(&|h| h["id"] = "not-a-uuid".into()).as_deref(),
        Some(F::SESSION)
    );
    assert_eq!(
        with(&|h| h["config"]["helper"] = "/obelisk-steam-host".into()).as_deref(),
        Some(F::SESSION),
        "a helper beside no folder of helpers"
    );
    // A packet that is not a handoff, or names a field no handoff has in its
    // configuration, is not a session.
    assert_eq!(
        with(&|h| h["config"]["extra"] = true.into()).as_deref(),
        Some(F::SESSION)
    );
    let credential = F::credential_json();
    let required = fields(&credential).into_iter().chain(["account_id"]);
    for field in required {
        assert_eq!(
            with(&|h| h.as_object_mut().unwrap().remove(field).map(drop).unwrap()).as_deref(),
            Some(F::SESSION),
            "{field}"
        );
    }
    let mut garbage: &[u8] = b"not json\n";
    assert_eq!(key(F::open(&mut garbage)).as_deref(), Some(F::SESSION));
}

pub(crate) fn a_handoff_from_before_titles_environments_and_wine_still_opens_and_extra_top_level_fields_are_ignored<
    F: Setup,
>() {
    let mut older = F::handoff();
    let object = older.as_object_mut().unwrap();
    for field in ["environment", "wine"] {
        object.remove(field);
    }
    object["config"].as_object_mut().unwrap().remove("title");
    object["config"].as_object_mut().unwrap().remove("prefix");
    object.insert("added_later".into(), true.into());

    let (opening, wine) = opened::<F>(&older).unwrap();

    assert_eq!(opening.account_id, F::account());
    assert_eq!(wine, None);
}

/// A Mac build whose installation folder exists, and the request that starts
/// it.
fn mac_request<F: Setup>(root: &Path) -> (Request<F::G>, PathBuf) {
    let configuration = F::configuration_in(root, Platform::Mac, None);
    let install = F::G::build(&configuration).install.to_owned();
    fs::create_dir_all(&install).unwrap();
    (request::<F>(configuration, None), install)
}

pub(crate) fn a_mac_target_checks_the_full_macho_before_repairing_its_permission<F: Setup>() {
    let (_directory, root) = installation();
    let (request, install) = mac_request::<F>(&root);
    let executable = install.join("Game");
    let launched = plan(&executable, &[]);
    let mode = || fs::metadata(&executable).unwrap().permissions().mode() & 0o100;
    fs::write(&executable, b"\xcf\xfa\xed\xfe").unwrap();
    fs::set_permissions(&executable, fs::Permissions::from_mode(0o600)).unwrap();

    for bytes in [b"\xcf\xfa\xed\xfe".to_vec(), thin(INTEL, true)] {
        fs::write(&executable, bytes).unwrap();
        assert_eq!(
            key(request.target(&launched, None)).as_deref(),
            Some("backend.runtime.macBuildNotNative")
        );
        assert_eq!(mode(), 0, "a build that cannot run is left as it is");
    }

    fs::write(&executable, thin(ARM64, true)).unwrap();
    let started = request.target(&launched, None).unwrap();
    assert_eq!(started.architecture, Architecture::Arm64);
    assert_eq!(
        started.target.executable,
        executable.canonicalize().unwrap()
    );
    assert!(started.kept.is_none());
    assert_ne!(
        mode(),
        0,
        "an installer that dropped the permission is repaired"
    );

    // Nothing outside the installation is ever a target, nor a missing file.
    fs::write(root.join("Other"), thin(ARM64, true)).unwrap();
    for outside in [root.join("Other"), install.join("Gone")] {
        assert_eq!(
            key(request.target(&plan(&outside, &[]), None)).as_deref(),
            Some(F::UNSUPPORTED)
        );
    }
}

pub(crate) fn a_mac_game_starts_where_its_plan_says_with_its_arguments_and_its_log<F: Setup>() {
    let (_directory, root) = installation();
    let (mut request, install) = mac_request::<F>(&root);
    let executable = install.join("Game");
    fs::write(&executable, thin(ARM64, true)).unwrap();
    let launched = plan(&executable, &["-windowed", "two words"]);
    request.environment = HashMap::from([("LANG".to_owned(), "fr_FR.UTF-8".to_owned())]);

    let started = request.target(&launched, None).unwrap();

    assert_eq!(started.architecture, Architecture::Arm64);
    let target = started.target;
    assert_eq!(target.arguments, ["-windowed", "two words"]);
    assert_eq!(target.working_directory, install);
    assert_eq!(target.environment, request.environment);
    assert_eq!(
        target.log,
        Some(crate::diagnostics::game_log(
            F::G::build(&request.config).home,
            F::GAME.into(),
            Platform::Mac
        ))
    );
    assert_eq!(target.installation, Some(install));
    assert_eq!(target.program, None);
}

pub(crate) fn a_prepared_game_runs_through_its_helpers_game_exec_with_its_targets_environment<
    F: Setup,
>() {
    let (_directory, root) = installation();
    let (mut request, install) = mac_request::<F>(&root);
    fs::write(install.join("Game"), thin(ARM64, true)).unwrap();
    request.environment = HashMap::from([("LANG".to_owned(), "fr_FR.UTF-8".to_owned())]);
    let started = request
        .target(&plan(&install.join("Game"), &[]), None)
        .unwrap();

    let prepared = request.prepared("host", &started.target);

    assert_eq!(prepared.host, "host", "kept until the game exits");
    assert_eq!(prepared.helper, F::G::build(&request.config).helper);
    assert_eq!(prepared.subcommand, "game-exec");
    assert_eq!(prepared.environment, started.target.environment);
    assert_eq!(prepared.environment["LANG"], "fr_FR.UTF-8");
}

/// A Windows build with a program in its installation, and the Wine that runs
/// it in its prefix.
fn windows_request<F: Setup>(root: &Path) -> (Request<F::G>, wine::Config, PathBuf) {
    let prefix = root.join("library/Prefixes/launch");
    let configuration = F::configuration_in(root, Platform::Windows, Some(prefix.clone()));
    let install = F::G::build(&configuration).install.to_owned();
    fs::create_dir_all(install.join("System")).unwrap();
    let program = install.join("System/game.exe");
    fs::write(&program, pe()).unwrap();
    let wine = wine_in(root, &prefix);
    (
        request::<F>(configuration, Some(wine.clone())),
        wine,
        program,
    )
}

pub(crate) fn a_windows_game_starts_in_wine_with_its_arguments_after_its_program<F: Setup>() {
    let (_directory, root) = installation();
    let (mut request, wine, program) = windows_request::<F>(&root);
    let install = F::G::build(&request.config).install.to_owned();
    request.environment = HashMap::from([("LANG".to_owned(), "fr_FR.UTF-8".to_owned())]);

    let started = request
        .target(&plan(&program, &["-skipintro"]), None)
        .unwrap();

    assert_eq!(started.architecture, Architecture::Intel);
    assert!(started.kept.is_none());
    let target = started.target;
    assert_eq!(target.executable, wine.runtime.join("bin/wine"));
    assert_eq!(
        target.arguments,
        [program.to_string_lossy().into_owned(), "-skipintro".into()]
    );
    assert_eq!(target.working_directory, program.parent().unwrap());
    assert_eq!(target.environment["LANG"], "fr_FR.UTF-8");
    assert_eq!(
        target.environment["WINEPREFIX"],
        wine.prefix.to_string_lossy()
    );
    assert_eq!(target.log, Some(wine.log.clone()));
    assert_eq!(target.installation, Some(install.clone()));
    assert_eq!(target.program, Some(program.canonicalize().unwrap()));

    // Wine runs only a Windows program inside the installation.
    fs::write(install.join("System/readme.txt"), "text").unwrap();
    fs::write(root.join("outside.exe"), pe()).unwrap();
    for refused in [install.join("System/readme.txt"), root.join("outside.exe")] {
        assert_eq!(
            key(request.target(&plan(&refused, &[]), None)).as_deref(),
            Some(F::UNSUPPORTED)
        );
    }
}

pub(crate) fn a_windows_build_on_the_linux_engine_takes_its_store_title_in_the_dock<F: Setup>() {
    let (_directory, root) = installation();
    let (mut request, rosetta, program) = windows_request::<F>(&root);
    request.wine = Some(wine::Config {
        settings: crate::launch_settings::Settings {
            engine: wine::Engine::Linux,
            ..Default::default()
        },
        ..rosetta
    });

    let started = request.target(&plan(&program, &[]), None).unwrap();

    assert_eq!(started.architecture, Architecture::Arm64);
    assert_eq!(started.target.executable, Path::new("/bin/bash"));
    assert_eq!(
        started.target.environment[wine::linux_engine::APP_NAME],
        F::TITLE
    );
}

pub(crate) fn a_private_file_lives_as_long_as_its_game_and_is_placed_where_the_engine_reads_it<
    F: Setup,
>() {
    let file = || ArgumentFile {
        argument: "-token",
        folder: "obelisk-argument-file-",
        name: "token.bin",
        bytes: b"secret",
    };
    let (_directory, root) = installation();

    // A Mac build keeps it in a private folder of its own, last of the
    // arguments.
    let (request, install) = mac_request::<F>(&root);
    fs::write(install.join("Game"), thin(ARM64, true)).unwrap();
    let Started { target, kept, .. } = request
        .target(&plan(&install.join("Game"), &["-windowed"]), Some(file()))
        .unwrap();
    assert_eq!(target.arguments[0], "-windowed");
    let path = PathBuf::from(target.arguments[1].strip_prefix("-token=").unwrap());
    assert_eq!(target.arguments.len(), 2);
    assert_eq!(fs::read(&path).unwrap(), b"secret");
    let folder = kept.as_ref().expect("the folder is kept");
    assert_eq!(path.parent().unwrap(), folder.path());
    assert!(
        folder
            .path()
            .file_name()
            .unwrap()
            .to_string_lossy()
            .starts_with("obelisk-argument-file-")
    );
    assert!(folder.path().starts_with(std::env::temp_dir()));
    drop(kept);
    assert!(!path.exists());

    // A Windows build reads it where its engine's Wine does.
    let (mut request, rosetta, program) = windows_request::<F>(&root);
    let linux = wine::Config {
        prefix: root.join("game-linux/prefix"),
        settings: crate::launch_settings::Settings {
            engine: wine::Engine::Linux,
            ..rosetta.settings.clone()
        },
        ..rosetta.clone()
    };
    fs::create_dir_all(linux.prefix.join("drive_c")).unwrap();
    for (wine, architecture) in [(rosetta, Architecture::Intel), (linux, Architecture::Arm64)] {
        request.wine = Some(wine.clone());
        let Started {
            target,
            architecture: used,
            kept,
        } = request
            .target(&plan(&program, &["-skipintro"]), Some(file()))
            .unwrap();
        assert_eq!(used, architecture);
        let before = target
            .arguments
            .iter()
            .position(|each| each == "-skipintro");
        assert_eq!(
            before,
            Some(target.arguments.len() - 2),
            "its arguments come before the file"
        );
        let given = target
            .arguments
            .last()
            .unwrap()
            .strip_prefix("-token=")
            .unwrap();
        let read = match wine.settings.engine {
            wine::Engine::Rosetta => {
                PathBuf::from(given.strip_prefix("Z:").unwrap().replace('\\', "/"))
            }
            wine::Engine::Linux => wine
                .prefix
                .join("drive_c")
                .join(given.strip_prefix("C:\\").unwrap().replace('\\', "/")),
        };
        assert_eq!(fs::read(&read).unwrap(), b"secret");
        drop(kept);
        assert!(!read.exists());
    }
}

pub(crate) fn every_launch_failure_is_shown_with_the_storefronts_key<F: Setup>() {
    for (failure, key) in F::LAUNCH_KEYS {
        assert_eq!(Error::<F::G>::from(failure).to_string(), key, "{failure:?}");
    }
}

pub(crate) fn every_supervisor_failure_is_shown_with_the_storefronts_key<F: Setup>() {
    for (failure, key) in F::SUPERVISOR_KEYS {
        assert_eq!(Error::<F::G>::from(failure).to_string(), key, "{failure:?}");
    }
}

/// Instantiates the build-level scenarios for a storefront's fixture.
macro_rules! setup_suite {
    ($setup:ty) => {
        use $crate::launch_shell::tests as shell;
        #[test]
        fn a_build_runs_natively_or_only_in_the_prefix_its_configuration_names() {
            shell::a_build_runs_natively_or_only_in_the_prefix_its_configuration_names::<$setup>()
        }
        #[test]
        fn a_mac_build_hands_its_supervisor_this_request_and_nothing_more() {
            shell::a_mac_build_hands_its_supervisor_this_request_and_nothing_more::<$setup>()
        }
        #[test]
        fn a_windows_build_hands_its_supervisor_the_wine_that_runs_it_whole() {
            shell::a_windows_build_hands_its_supervisor_the_wine_that_runs_it_whole::<$setup>()
        }
        #[test]
        fn a_start_that_cannot_run_the_build_spawns_no_supervisor() {
            shell::a_start_that_cannot_run_the_build_spawns_no_supervisor::<$setup>()
        }
        #[test]
        fn a_supervisor_opens_the_session_its_handoff_names() {
            shell::a_supervisor_opens_the_session_its_handoff_names::<$setup>()
        }
        #[test]
        fn a_supervisor_runs_a_windows_build_in_the_wine_its_handoff_carries() {
            shell::a_supervisor_runs_a_windows_build_in_the_wine_its_handoff_carries::<$setup>()
        }
        #[test]
        fn a_supervisor_refuses_a_handoff_it_cannot_trust_with_the_key_its_player_sees() {
            shell::a_supervisor_refuses_a_handoff_it_cannot_trust_with_the_key_its_player_sees::<
                $setup,
            >()
        }
        #[test]
        fn a_handoff_from_before_titles_environments_and_wine_still_opens_and_extra_top_level_fields_are_ignored()
         {
            shell::a_handoff_from_before_titles_environments_and_wine_still_opens_and_extra_top_level_fields_are_ignored::<$setup>()
        }
        #[test]
        fn a_mac_target_checks_the_full_macho_before_repairing_its_permission() {
            shell::a_mac_target_checks_the_full_macho_before_repairing_its_permission::<$setup>()
        }
        #[test]
        fn a_mac_game_starts_where_its_plan_says_with_its_arguments_and_its_log() {
            shell::a_mac_game_starts_where_its_plan_says_with_its_arguments_and_its_log::<$setup>()
        }
        #[test]
        fn a_prepared_game_runs_through_its_helpers_game_exec_with_its_targets_environment() {
            shell::a_prepared_game_runs_through_its_helpers_game_exec_with_its_targets_environment::<
                $setup,
            >()
        }
        #[test]
        fn a_windows_game_starts_in_wine_with_its_arguments_after_its_program() {
            shell::a_windows_game_starts_in_wine_with_its_arguments_after_its_program::<$setup>()
        }
        #[test]
        fn a_windows_build_on_the_linux_engine_takes_its_store_title_in_the_dock() {
            shell::a_windows_build_on_the_linux_engine_takes_its_store_title_in_the_dock::<$setup>()
        }
        #[test]
        fn a_private_file_lives_as_long_as_its_game_and_is_placed_where_the_engine_reads_it() {
            shell::a_private_file_lives_as_long_as_its_game_and_is_placed_where_the_engine_reads_it::<
                $setup,
            >()
        }
        #[test]
        fn every_launch_failure_is_shown_with_the_storefronts_key() {
            shell::every_launch_failure_is_shown_with_the_storefronts_key::<$setup>()
        }
        #[test]
        fn every_supervisor_failure_is_shown_with_the_storefronts_key() {
            shell::every_supervisor_failure_is_shown_with_the_storefronts_key::<$setup>()
        }
    };
}
pub(crate) use setup_suite;

/// What a storefront's launch scenarios need of its installations: a library
/// of folders in a temporary directory, a network the test stands in for, and
/// the storefront's `Content`.
pub(crate) trait Library: Setup + Sized + Sync {
    /// A library whose Obelisk bundle lies outside it.
    fn new() -> Self;
    /// A library with Obelisk's bundle inside it, where its helper is a script
    /// that records what a launch hands its supervisor.
    fn bundled() -> Self;
    fn locations(&self) -> &Locations;
    /// The game's installed build of `platform`.
    fn receipt(&self, platform: Platform) -> Receipt<Self::G>;
    /// A Windows receipt whose folder names no prefix.
    fn nameless(&self) -> Receipt<Self::G>;
    /// The game a launch admits.
    fn game() -> <Self::G as sessions::Storefront>::Game;
    fn launch<'a>(
        &'a self,
        receipt: &'a Receipt<Self::G>,
        admission: Admission<Self::G>,
    ) -> BoxFuture<'a, Result<(), Failure>>;
    fn configuration(
        &self,
        receipt: &Receipt<Self::G>,
        prefix: Option<PathBuf>,
    ) -> Configuration<Self::G>;
    fn signing(&self) -> impl SignIn<Self::G>;
    /// What a launch of `receipt` hands its supervisor as its configuration,
    /// as the wire spells it.
    fn wire_config(&self, receipt: &Receipt<Self::G>, prefix: Option<&Path>) -> Value;
    /// The prefix of the game's Windows build.
    fn prefix(&self) -> PathBuf;
    /// Completes the Windows installation: its program, `program`, its receipt
    /// and its prerequisite. Returns the prerequisite's program.
    fn complete_windows_build(&self, program: &[u8]) -> PathBuf;
    /// The command line the prerequisite runs with.
    const PREREQUISITE_ARGUMENTS: &'static str;
    /// The key an installation without its receipt shows.
    const INCOMPLETE: &'static str;
    /// Voile, whose Wine records each call it gets beside the prefix.
    fn install_voile(&self) {
        record_calls(&self.locations().voile.join("current"));
    }
    /// What the network was asked for the launch, without the saves of the
    /// credentials.
    fn asked(&self) -> Vec<String>;
    /// What a launch asks the network for its credential.
    const CREDENTIAL_CALLS: &'static [&'static str];
    /// Makes the credential fail, and returns the key the launch then shows.
    fn refuse_credential(&self) -> &'static str;
    /// Whether the launch saved the credentials the network rotated.
    fn saved(&self) -> bool;
    /// What the recording supervisor of a bundled library was handed.
    fn handed_over(&self) -> Option<HandedOver> {
        handed_over(&self.locations().library.join("handoff"))
    }
}

/// A temporary library of folders. Obelisk's bundle lies outside it, or
/// inside it when `bundled`, where its helper is a script that records what a
/// launch hands its supervisor.
pub(crate) fn folders(bundled: bool) -> (tempfile::TempDir, Locations) {
    let root = tempfile::tempdir().unwrap();
    let library = root.path().canonicalize().unwrap();
    let bundle = match bundled {
        true => library.join("Obelisk.app"),
        false => PathBuf::from("/Applications/Obelisk.app"),
    };
    let locations = Locations::new(&library, &bundle, &library.join("home"));
    if bundled {
        write_recording_supervisor(&locations.helper, &library.join("handoff"));
    }
    (root, locations)
}

/// Sessions with every runtime in place, past recovery.
pub(crate) async fn ready_sessions<L: Library>() -> Sessions<L::G> {
    let sessions = Sessions::<L::G>::new(Box::new(|| Ok(None)), ready());
    eventually(|| !sessions.state().recovering).await;
    sessions
}

/// Sessions whose Voile is older than the release a newer one is offered over.
async fn outdated_sessions<L: Library>() -> Sessions<L::G> {
    let runtimes = FakeRuntimes::new(&[], |_| {});
    runtimes.outdate(Runtime::Voile);
    let sessions = Sessions::<L::G>::new(Box::new(|| Ok(None)), Runtimes::from_source(runtimes));
    eventually(|| !sessions.state().recovering).await;
    sessions
}

pub(crate) async fn a_mac_launch_asks_for_its_credential_then_hands_its_supervisor_a_request_without_wine<
    L: Library,
>() {
    let library = L::bundled();
    let receipt = library.receipt(Platform::Mac);
    let sessions = ready_sessions::<L>().await;
    let admission = sessions.admit(L::game(), L::account()).unwrap();

    let launched = library.launch(&receipt, admission).await;

    // The recording supervisor never acknowledges: the launch ends with the key
    // a lost start shows.
    assert_eq!(launched, Err(Failure::Failed(L::SESSION.into())));
    assert_eq!(library.asked(), L::CREDENTIAL_CALLS);
    let handed = library.handed_over().expect("the supervisor was started");
    assert_eq!(handed.arguments, [L::SUPERVISE]);
    assert_eq!(handed.request["account_id"], L::account());
    for (field, value) in L::credential_json().as_object().unwrap() {
        assert_eq!(&handed.request[field], value, "{field}");
    }
    assert_eq!(handed.request["wine"], Value::Null);
    assert_eq!(
        handed.request["environment"],
        serde_json::to_value(crate::native::environment(&library.locations().home)).unwrap()
    );
    assert_eq!(
        handed.request["config"],
        library.wire_config(&receipt, None)
    );
    assert!(sessions.state().active.is_none());
}

pub(crate) async fn a_windows_launch_readies_its_prefix_before_it_asks_for_a_credential_and_then_hands_over_its_request<
    L: Library,
>() {
    let library = L::bundled();
    library.install_voile();
    let receipt = library.receipt(Platform::Windows);
    let prerequisite = library.complete_windows_build(&pe());
    let sessions = ready_sessions::<L>().await;
    let admission = sessions.admit(L::game(), L::account()).unwrap();

    let launched = library.launch(&receipt, admission).await;

    assert_eq!(launched, Err(Failure::Failed(L::SESSION.into())));
    let locations = library.locations();
    assert_eq!(
        calls(&locations.wine_prefixes),
        [
            "wineboot --init".to_owned(),
            format!("{} {}", prerequisite.display(), L::PREREQUISITE_ARGUMENTS),
        ],
        "the prefix runs the prerequisites its build names"
    );
    assert_eq!(library.asked(), L::CREDENTIAL_CALLS);
    let handed = library.handed_over().expect("the supervisor was started");
    let prefix = library.prefix();
    // The supervisor starts the game its configuration names: the one whose
    // launch settings were resolved.
    assert_eq!(
        L::G::build(&library.configuration(&receipt, Some(prefix.clone()))).game,
        <L::G as installations::Storefront>::profile_game(&receipt.product),
    );
    assert_eq!(handed.request["account_id"], L::account());
    for (field, value) in L::credential_json().as_object().unwrap() {
        assert_eq!(&handed.request[field], value, "{field}");
    }
    assert_eq!(
        handed.request["config"],
        library.wire_config(&receipt, Some(&prefix))
    );
    let settings = crate::launch_settings::of(
        &locations.library,
        L::GAME,
        Path::new(&receipt.directory),
        crate::native::player_locale().as_deref(),
    )
    .settings;
    assert_eq!(
        handed.request["wine"],
        json!({
            "runtime": locations.voile.join("current"),
            "prefix": prefix,
            "log": crate::diagnostics::game_log(&locations.home, L::GAME.into(), Platform::Windows),
            "settings": settings,
        })
    );
}

pub(crate) async fn a_windows_launch_stops_at_the_first_thing_its_prefix_lacks_before_asking_for_a_credential<
    L: Library,
>() {
    let library = L::bundled();
    let receipt = library.receipt(Platform::Windows);
    let sessions = ready_sessions::<L>().await;
    let launch = || async {
        let admission = sessions.admit(L::game(), L::account()).unwrap();
        library.launch(&receipt, admission).await
    };

    // Its receipt and launch command are read first, even without Voile.
    assert_eq!(launch().await, Err(Failure::Failed(L::INCOMPLETE.into())));

    // Then Voile's Wine is looked for.
    library.complete_windows_build(&pe());
    assert_eq!(
        launch().await,
        Err(Failure::Failed("backend.runtime.wineUnavailable".into()))
    );

    // Then the game must be a Windows program of the installation, before
    // anything touches the prefix.
    library.install_voile();
    library.complete_windows_build(b"not a program");
    assert_eq!(launch().await, Err(Failure::Failed(L::UNSUPPORTED.into())));
    let wine_prefixes = &library.locations().wine_prefixes;
    assert!(calls(wine_prefixes).is_empty());
    assert!(!wine_prefixes.exists());

    assert!(
        library.asked().is_empty(),
        "no credential asked for, no supervisor started"
    );
    assert!(library.handed_over().is_none());
    assert!(
        sessions.state().active.is_none(),
        "each refused launch gives its Reservation back"
    );
}

pub(crate) async fn a_refused_credential_ends_a_windows_launch_after_its_prefix_was_readied<
    L: Library,
>() {
    let library = L::bundled();
    library.install_voile();
    let receipt = library.receipt(Platform::Windows);
    library.complete_windows_build(&pe());
    let shown = library.refuse_credential();
    let sessions = ready_sessions::<L>().await;
    let admission = sessions.admit(L::game(), L::account()).unwrap();

    let launched = library.launch(&receipt, admission).await;

    assert_eq!(launched, Err(Failure::Failed(shown.into())));
    assert_eq!(calls(&library.locations().wine_prefixes).len(), 2);
    assert_eq!(library.asked(), L::CREDENTIAL_CALLS);
    assert!(library.handed_over().is_none());
}

pub(crate) async fn a_windows_launch_gets_voile_before_it_is_prepared_and_a_mac_launch_gets_none<
    L: Library,
>() {
    let library = L::new();
    let (events, record) = recorded();
    let runtimes = FakeRuntimes::new(&[], record);
    runtimes.hold();
    let sessions = Sessions::<L::G>::new(
        Box::new(|| Ok(None)),
        Runtimes::from_source(runtimes.clone()),
    );
    eventually(|| !sessions.state().recovering).await;
    let admission = sessions.admit(L::game(), L::account()).unwrap();
    let canceller = admission.canceller();
    let windows = library.receipt(Platform::Windows);

    let (launched, ()) = tokio::join!(library.launch(&windows, admission), async {
        eventually(|| !events.lock().unwrap().is_empty()).await;
        canceller.cancel();
    });

    assert_eq!(launched, Err(Failure::Cancelled), "nothing is prepared");
    assert_eq!(*events.lock().unwrap(), ["voile"]);

    // Downloads no longer wait: one the Mac build got would show.
    runtimes.release();
    // A refused credential stops the Mac launch once prepared.
    let shown = library.refuse_credential();
    let admission = sessions.admit(L::game(), L::account()).unwrap();
    let launched = library
        .launch(&library.receipt(Platform::Mac), admission)
        .await;
    assert_eq!(launched, Err(Failure::Failed(shown.into())));
    assert_eq!(*events.lock().unwrap(), ["voile"], "a Mac build needs none");
}

pub(crate) async fn a_windows_launch_without_a_named_release_is_offered_a_newer_voile_optionally<
    L: Library,
>() {
    let library = L::new();
    let sessions = outdated_sessions::<L>().await;
    let shown = || {
        sessions
            .state()
            .active
            .and_then(|active| active.voile_update)
    };
    let admission = sessions.admit(L::game(), L::account()).unwrap();
    let canceller = admission.canceller();
    let windows = library.receipt(Platform::Windows);

    let (launched, ()) = tokio::join!(library.launch(&windows, admission), async {
        eventually(|| shown().is_some()).await;
        // No profile names a Voile release for this game, so the player may
        // skip it.
        assert_eq!(shown(), Some(fake_offer(false)));
        canceller.cancel();
    });

    assert_eq!(launched, Err(Failure::Cancelled), "nothing is prepared");
    assert!(library.asked().is_empty());
}

pub(crate) async fn hand_off_makes_a_windows_launch_wait_for_the_voile_release_its_game_names<
    L: Library,
>() {
    let library = L::new();
    let sessions = outdated_sessions::<L>().await;
    let shown = || {
        sessions
            .state()
            .active
            .and_then(|active| active.voile_update)
    };
    let receipt = library.receipt(Platform::Windows);
    // A game whose profile names a release.
    let named = crate::launch_settings::View {
        minimum_voile: Some("v26.3.0-18"),
        ..receipt.launch_settings(&library.locations().library)
    };
    let windows = wine::Installation::new(
        library.locations(),
        L::GAME,
        Path::new(&receipt.directory),
        named,
    )
    .unwrap();
    let admission = sessions.admit(L::game(), L::account()).unwrap();
    let canceller = admission.canceller();

    let (launched, ()) = tokio::join!(
        hand_off(
            admission,
            Some(windows),
            library.locations(),
            |prefix| library.configuration(&receipt, prefix),
            library.signing(),
        ),
        async {
            eventually(|| shown().is_some()).await;
            assert_eq!(shown(), Some(fake_offer(true)));
            canceller.cancel();
        }
    );

    assert_eq!(launched, Err(Failure::Cancelled), "nothing is prepared");
    assert!(library.asked().is_empty());
}

pub(crate) async fn a_windows_receipt_whose_launch_settings_cannot_be_resolved_ends_its_launch_with_its_key_before_anything_is_asked<
    L: Library,
>() {
    let library = L::bundled();
    library.install_voile();
    let (events, record) = recorded();
    let runtimes = FakeRuntimes::new(&[], record);
    let sessions = Sessions::<L::G>::new(Box::new(|| Ok(None)), Runtimes::from_source(runtimes));
    eventually(|| !sessions.state().recovering).await;
    let receipt = library.nameless();
    // A folder with no name of its own, such as `..`: no prefix is named after
    // it.
    let failure = receipt
        .windows(library.locations())
        .expect_err("the prefix cannot be named");
    assert_eq!(failure, launch::Error::NotExecutable);
    let admission = sessions.admit(L::game(), L::account()).unwrap();

    let launched = library.launch(&receipt, admission).await;

    // The failure ends the launch as Steam's does, before the runtimes, the
    // credential and any supervisor: the key is the one its error shows.
    assert_eq!(launched, Err(Failure::Failed(L::UNSUPPORTED.into())));
    assert!(
        events.lock().unwrap().is_empty(),
        "no runtime was asked for"
    );
    assert!(
        library.asked().is_empty(),
        "nothing was asked of the network"
    );
    assert!(library.handed_over().is_none());
    // A save runs on a thread of its own, once the launch has moved on.
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert!(!library.saved(), "nothing rotated the credentials");
    let state = sessions.state();
    assert!(
        state.active.is_none() && !state.recovering,
        "the Reservation is back at once"
    );
}

/// A `SignIn` that only notes that its hook ran.
struct Noting(Arc<AtomicBool>);

impl<G: Launchable> SignIn<G> for Noting {
    fn credential(&self) -> BoxFuture<'_, Result<(String, G::Credential), Failure>> {
        Box::pin(async { Err(Failure::Cancelled) })
    }
    fn before_begin(&self) -> BeforeBegin {
        let noted = self.0.clone();
        Box::new(move || noted.store(true, Ordering::SeqCst))
    }
}

pub(crate) fn a_handover_runs_its_sign_ins_hook_once_its_supervisor_holds_the_game<L: Library>() {
    let library = L::new();
    let receipt = library.receipt(Platform::Mac);
    let noted = Arc::new(AtomicBool::new(false));
    let handover = Handover::<L::G, _> {
        sign_in: Noting(noted.clone()),
        configuration: library.configuration(&receipt, None),
        windows: None,
        locations: library.locations().clone(),
    };

    let before_begin = sessions::Handover::<L::G>::before_begin(&handover);

    assert!(!noted.load(Ordering::SeqCst), "not before the supervisor");
    before_begin();
    assert!(noted.load(Ordering::SeqCst));
}

/// Instantiates the launch scenarios for a storefront's library.
macro_rules! library_suite {
    ($library:ty) => {
        use $crate::launch_shell::tests as shell;
        #[tokio::test]
        async fn a_mac_launch_asks_for_its_credential_then_hands_its_supervisor_a_request_without_wine()
         {
            shell::a_mac_launch_asks_for_its_credential_then_hands_its_supervisor_a_request_without_wine::<$library>().await
        }
        #[tokio::test]
        async fn a_windows_launch_readies_its_prefix_before_it_asks_for_a_credential_and_then_hands_over_its_request()
         {
            shell::a_windows_launch_readies_its_prefix_before_it_asks_for_a_credential_and_then_hands_over_its_request::<$library>().await
        }
        #[tokio::test]
        async fn a_windows_launch_stops_at_the_first_thing_its_prefix_lacks_before_asking_for_a_credential()
         {
            shell::a_windows_launch_stops_at_the_first_thing_its_prefix_lacks_before_asking_for_a_credential::<$library>().await
        }
        #[tokio::test]
        async fn a_refused_credential_ends_a_windows_launch_after_its_prefix_was_readied() {
            shell::a_refused_credential_ends_a_windows_launch_after_its_prefix_was_readied::<$library>().await
        }
        #[tokio::test]
        async fn a_windows_launch_gets_voile_before_it_is_prepared_and_a_mac_launch_gets_none() {
            shell::a_windows_launch_gets_voile_before_it_is_prepared_and_a_mac_launch_gets_none::<$library>().await
        }
        #[tokio::test]
        async fn a_windows_launch_without_a_named_release_is_offered_a_newer_voile_optionally() {
            shell::a_windows_launch_without_a_named_release_is_offered_a_newer_voile_optionally::<$library>().await
        }
        #[tokio::test]
        async fn hand_off_makes_a_windows_launch_wait_for_the_voile_release_its_game_names() {
            shell::hand_off_makes_a_windows_launch_wait_for_the_voile_release_its_game_names::<$library>().await
        }
        #[test]
        fn a_handover_runs_its_sign_ins_hook_once_its_supervisor_holds_the_game() {
            shell::a_handover_runs_its_sign_ins_hook_once_its_supervisor_holds_the_game::<$library>(
            )
        }
        #[tokio::test]
        async fn a_windows_receipt_whose_launch_settings_cannot_be_resolved_ends_its_launch_with_its_key_before_anything_is_asked()
         {
            shell::a_windows_receipt_whose_launch_settings_cannot_be_resolved_ends_its_launch_with_its_key_before_anything_is_asked::<$library>().await
        }
    };
}
pub(crate) use library_suite;
