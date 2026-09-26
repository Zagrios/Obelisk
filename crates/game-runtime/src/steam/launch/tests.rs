//! The Steam launch through the shared lifecycle suite. Its machine and
//! Steam Cloud are fakes that follow the suite's script; the handoff, the
//! native host's interruption and the helper choice are the adapter's own.
use super::*;
use crate::supervisor::session::tests::{
    Harness, Script, fixture_directory, hold, inspect, lifecycle_suite, record, target,
    write_helper,
};
use std::fs;

const ACCOUNT: u64 = 0x0110_0001_0000_002a;

fn credentials() -> Credentials {
    Credentials {
        steam_id: ACCOUNT,
        account_name: "fixture".into(),
        refresh_token: "fixture-token".into(),
    }
}

pub(crate) struct SteamHarness;

impl Harness for SteamHarness {
    type P = Steam;
    const NAME: &'static str = "steam";
    fn account() -> u64 {
        ACCOUNT
    }
    fn other_account() -> u64 {
        ACCOUNT + 1
    }
    fn handoff(
        directory: &Path,
        sessions: &Path,
        id: &str,
        pipe: &mut std::os::unix::net::UnixStream,
    ) -> Result<(), Core> {
        let script: Script =
            serde_json::from_slice(&fs::read(directory.join("script.json")).unwrap()).unwrap();
        credentials()
            .write(&mut *pipe)
            .map_err(|_| Core::StartFailed)?;
        storage::send(
            pipe,
            &Request {
                root: sessions.into(),
                id: id.into(),
                config: config(directory),
                target: target(directory, &script),
                cloud: Some(cloud::Config {
                    root: directory.join("cloud"),
                    home: directory.into(),
                    install: directory.into(),
                    app_id: 42,
                    prefix: None,
                }),
            },
        )
    }
    fn serve(
        input: std::io::StdinLock<'static>,
        output: std::io::StderrLock<'static>,
    ) -> Result<(), String> {
        session::serve::<Launch<FakeMac>>(input, output, inspect)
            .map_err(|error| format!("{error:?}"))
    }
    fn previous_build_record(
        id: &str,
        live: &crate::supervisor::Identity,
        socket: &Path,
    ) -> serde_json::Value {
        serde_json::json!({
            "id": id,
            "app_id": 42,
            "account_id": ACCOUNT,
            "supervisor": live,
            "game": live,
            "starting": null,
            "socket": socket,
            "phase": "Running",
            "host": "online",
            "exit_code": null,
            "error": null,
            "revision": 3,
            "cloud": {"phase": "Idle", "id": 0, "error": null, "allowed_choices": []},
            "rosetta": null,
        })
    }
}

/// The Rosetta helper app sits beside `SteamNative`, in the test directory.
fn config(directory: &Path) -> Config {
    Config {
        helpers: directory.join("SteamNative"),
        package: directory.join("unused.zip"),
        cache: directory.join("runtime"),
        diagnostics: directory.join("diagnostics"),
        steam_api: directory.join("libsteam_api.dylib"),
        wine: None,
        home: directory.into(),
        app_id: 42,
        cloud_save_roots: vec![],
    }
}

struct FakeMac;
impl Machine for FakeMac {
    fn select(_: &Target, _: &Config) -> Result<Option<Architecture>, Error> {
        let (directory, script) = fixture_directory();
        record(&directory, "target");
        Ok(script.intel.then_some(Architecture::Intel))
    }
    fn prepare(
        launch: &crate::Launch,
        config: Config,
        _: Option<NativeStorage>,
    ) -> Result<Runtime, Error> {
        launch.check_cancelled()?;
        let (directory, script) = fixture_directory();
        record(&directory, "host.prepare");
        // Both helpers are the suite's; the adapter chooses between them.
        write_helper(&config.helpers.join(package::HOST), &directory, &script);
        write_helper(&config.cache.join(package::SDK_INTEL), &directory, &script);
        let runtime = crate::tests::runtime("cat >/dev/null", &config.cache);
        fs::write(
            directory.join("host-pid"),
            runtime.inner.lock().unwrap().host.id().to_string(),
        )
        .unwrap();
        Ok(runtime)
    }
    fn publish(_: &Runtime, active: bool) {
        record(&fixture_directory().0, &format!("presence:{active}"));
    }
    fn cloud(
        _: cloud::Config,
        _: Credentials,
        _: String,
        control: Arc<crate::cloud::Control>,
    ) -> Result<Box<dyn SteamCloud>, Error> {
        let (directory, script) = fixture_directory();
        Ok(Box::new(FakeCloud {
            directory,
            script,
            control,
        }))
    }
    fn rosetta(helper: &Path) -> Result<bool, Core> {
        Ok(helper.exists())
    }
}

struct FakeCloud {
    directory: PathBuf,
    script: Script,
    control: Arc<crate::cloud::Control>,
}
impl SteamCloud for FakeCloud {
    fn native_storage(&self) -> Result<Option<NativeStorage>, Error> {
        Ok(None)
    }
    fn prepare<'a>(&'a mut self, _: &'a SessionAlive<'a>) -> Operation<'a, Result<(), Error>> {
        Box::pin(async move {
            record(&self.directory, "cloud.prepare");
            let check = || {
                self.control
                    .check()
                    .map_err(|stop| Error::from(cloud::Failure::from(stop)))
            };
            if self.script.hold_cloud {
                fs::write(self.directory.join("cloud-ready"), b"").unwrap();
                hold(&self.directory, "release-cloud", check).await?;
            }
            if self.script.ask {
                let choice = self
                    .control
                    .ask(crate::cloud::Question::Conflict, None)
                    .await
                    .map_err(cloud::Failure::from)?;
                record(&self.directory, &format!("answered:{choice:?}"));
                self.control.show(crate::cloud::Phase::Ready, None);
            }
            Ok(())
        })
    }
    fn started(&mut self) -> Result<(), Error> {
        record(&self.directory, "cloud.started");
        Ok(())
    }
    fn finished(&mut self, confirmed: bool) -> Operation<'_, ()> {
        Box::pin(async move {
            if self.script.hold_finish {
                fs::write(self.directory.join("finish-ready"), b"").unwrap();
                let _ = hold(&self.directory, "release-finish", || Ok::<_, Error>(())).await;
            }
            self.control.show(crate::cloud::Phase::Syncing, None);
            record(&self.directory, &format!("cloud.finished:{confirmed}"));
        })
    }
    fn aborted(&mut self) {
        record(&self.directory, "cloud.aborted");
    }
}

mod lifecycle {
    super::lifecycle_suite!(super::SteamHarness);
}

use crate::supervisor::{
    Phase,
    session::tests::{HELD, launch, script},
};

#[test]
fn a_steam_game_gets_its_app_ids_and_the_host_environment() {
    let run = launch::<SteamHarness>(script(
        "printf '%s %s %s' \"$SteamAppId\" \"$SteamGameId\" \"$Steam3Master\" > environment\nexit 0\n",
    ));
    run.until(Snapshot::finished);
    assert_eq!(
        fs::read_to_string(run.directory.join("environment")).unwrap(),
        "42 42 127.0.0.1:1"
    );
}

#[test]
fn a_lost_steam_host_keeps_the_game_reserved() {
    let run = launch::<SteamHarness>(script(HELD));
    run.until(|r| r.phase == Phase::Running);
    let host = fs::read_to_string(run.directory.join("host-pid")).unwrap();
    assert!(
        std::process::Command::new("/bin/kill")
            .args(["-TERM", &host])
            .status()
            .unwrap()
            .success()
    );
    let lost = run.until(|r| r.store.host == HostState::Lost);
    assert_eq!(
        lost.phase,
        Phase::Running,
        "host loss is not the game's exit"
    );
    assert!(!lost.finished());
    run.release("release-game");
    let finished = run.until(Snapshot::finished);
    assert_eq!(finished.exit_code, Some(3));
    assert_eq!(finished.store.host, HostState::Stopped);
}

#[test]
fn the_steam_journal_holds_no_credentials() {
    let run = launch::<SteamHarness>(script("exit 0\n"));
    let finished = run.until(Snapshot::finished);
    let record = fs::read_to_string(run.sessions.join(format!("{}.v2.json", finished.id))).unwrap();
    assert!(!record.contains("fixture-token") && !record.contains("refresh_token"));
}

fn launch_target(executable: &Path, bundle: bool) -> sailover_core::launch::LaunchTarget {
    sailover_core::launch::LaunchTarget {
        executable: executable.to_string_lossy().into_owned(),
        arguments: vec![],
        working_directory: String::new(),
        is_application_bundle: bundle,
    }
}

#[test]
fn the_steam_api_library_is_found_inside_a_bundle_or_beside_its_executable() {
    let root = tempfile::tempdir().unwrap();
    let root = root.path().canonicalize().unwrap();
    let bundle = root.join("Game.app");
    let library = bundle.join("Contents/Frameworks/libsteam_api.dylib");
    fs::create_dir_all(library.parent().unwrap()).unwrap();
    fs::write(&library, b"library").unwrap();
    assert_eq!(steam_api(&launch_target(&bundle, true)).unwrap(), library);

    let game = root.join("standalone/game");
    fs::create_dir_all(game.parent().unwrap()).unwrap();
    fs::write(&game, b"game").unwrap();
    fs::write(root.join("standalone/libsteam_api.dylib"), b"library").unwrap();
    assert_eq!(
        steam_api(&launch_target(&game, false)).unwrap(),
        root.join("standalone/libsteam_api.dylib")
    );
}

#[test]
fn the_steam_api_search_never_follows_links_or_hidden_folders() {
    let root = tempfile::tempdir().unwrap();
    let root = root.path().canonicalize().unwrap();
    let outside = tempfile::tempdir().unwrap();
    fs::write(outside.path().join("libsteam_api.dylib"), b"library").unwrap();
    let bundle = root.join("Game.app");
    fs::create_dir_all(bundle.join("Contents/.hidden")).unwrap();
    fs::write(
        bundle.join("Contents/.hidden/libsteam_api.dylib"),
        b"library",
    )
    .unwrap();
    std::os::unix::fs::symlink(outside.path(), bundle.join("Contents/Linked")).unwrap();
    std::os::unix::fs::symlink(
        outside.path().join("libsteam_api.dylib"),
        bundle.join("Contents/libsteam_api.dylib"),
    )
    .unwrap();
    assert!(matches!(
        steam_api(&launch_target(&bundle, true)),
        Err(Error::ApiMissing)
    ));
}
