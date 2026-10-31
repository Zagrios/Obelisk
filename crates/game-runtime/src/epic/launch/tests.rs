//! The Epic launch through the shared lifecycle suite. Its Epic services and
//! EGS Cloud are fakes that follow the suite's script; the Begin gate, the
//! launch plan's target checks and the handoff are the adapter's own.
use super::*;
use crate::launch_shell::{tests::fields, validate};
use crate::supervisor::architecture::tests::{ARM64, thin};
use crate::supervisor::session::tests::{
    HELD, Harness, Script, begin_opens_the_start_gate_once_and_only_for_its_account,
    fixture_directory, hold, inspect, launch, lifecycle_suite, record, script, write_helper,
};
use crate::supervisor::storage;
use crate::supervisor::{client::SessionAlive, session::Operation};
use std::fs;

const ACCOUNT: &str = "0123456789abcdef0123456789abcdef";

pub(crate) struct EpicHarness;

fn config(directory: &Path, sessions: &Path) -> Configuration {
    Configuration {
        root: directory.join("archive"),
        home: directory.into(),
        install: directory.join("install"),
        session_directory: sessions.into(),
        // The Rosetta helper app sits beside `SteamNative`, in the test directory.
        helper: directory.join("SteamNative/obelisk-steam-host"),
        namespace: "namespace".into(),
        catalog_item_id: "catalog".into(),
        app_name: "game".into(),
        platform: Platform::Mac,
        language: "en".into(),
        title: String::new(),
        prefix: None,
    }
}

impl Harness for EpicHarness {
    type P = Epic;
    const NAME: &'static str = "epic";
    fn account() -> String {
        ACCOUNT.into()
    }
    fn other_account() -> String {
        "f".repeat(32)
    }
    fn handoff(
        directory: &Path,
        sessions: &Path,
        id: &str,
        pipe: &mut std::os::unix::net::UnixStream,
    ) -> std::result::Result<(), Core> {
        storage::send(
            pipe,
            &Request::<Epic> {
                config: config(directory, sessions),
                id: id.into(),
                account_id: ACCOUNT.into(),
                credential: ExchangeCode {
                    exchange_code: "fixture-exchange-code".into(),
                },
                environment: crate::native::environment(directory),
                wine: None,
            },
        )
    }
    fn serve(
        input: std::io::StdinLock<'static>,
        output: std::io::StderrLock<'static>,
    ) -> std::result::Result<(), String> {
        lifecycle::serve::<Launch<FakeEpic>>(input, output, inspect)
            .map_err(|error| format!("{error:?}"))
    }
    fn begins() -> bool {
        true
    }
    fn previous_build_record(
        id: &str,
        live: &crate::supervisor::Identity,
        socket: &Path,
    ) -> serde_json::Value {
        serde_json::json!({
            "id": id,
            "account_id": ACCOUNT,
            "namespace": "namespace",
            "app_name": "game",
            "phase": "Running",
            "cloud_phase": "Checking",
            "decision_id": 0,
            "allowed_choices": [],
            "error": null,
            "exit_code": null,
            "revision": 3,
            "supervisor": live,
            "game": live,
            "starting": null,
            "socket": socket,
        })
    }
}

struct FakeEpic;
impl Services for FakeEpic {
    async fn login(_: &str, account_id: &str) -> Result<Arc<Self>> {
        assert_eq!(account_id, ACCOUNT);
        Ok(Arc::new(Self))
    }
    async fn launch_plan(&self, config: &Configuration) -> Result<LaunchPlan> {
        let (directory, script) = fixture_directory();
        record(&directory, "target");
        write_helper(&config.helper, &directory, &script);
        fs::create_dir_all(&config.install).unwrap();
        let executable = config.install.join("game");
        if !script.missing {
            fs::write(&executable, thin(ARM64, true)).unwrap();
        }
        Ok(LaunchPlan {
            app_name: config.app_name.clone(),
            platform: config.platform,
            activity: Some(Activity {
                id: "fixture-activity".into(),
                name: None,
            }),
            ownership_token: None,
            target: obelisk_core::launch::LaunchTarget {
                executable: executable.to_string_lossy().into_owned(),
                arguments: vec![directory.join("game.sh").to_string_lossy().into_owned()],
                working_directory: directory.to_string_lossy().into_owned(),
                is_application_bundle: false,
            },
        })
    }
    fn start_presence(&self, _: Activity) {
        record(&fixture_directory().0, "presence:true");
    }
    fn stop_presence(&self) {
        record(&fixture_directory().0, "presence:false");
    }
    fn release(&self) {}
    async fn cloud(
        self: &Arc<Self>,
        _: Configuration,
        _: String,
        _: String,
        control: Arc<Control>,
    ) -> Result<Box<dyn LaunchCloud<Error>>> {
        let (directory, script) = fixture_directory();
        Ok(Box::new(FakeCloud {
            directory,
            script,
            control,
        }))
    }
    fn rosetta(helper: &Path) -> std::result::Result<bool, Core> {
        Ok(helper.exists())
    }
}

struct FakeCloud {
    directory: PathBuf,
    script: Script,
    control: Arc<Control>,
}
impl LaunchCloud<Error> for FakeCloud {
    fn prepare<'a>(&'a mut self, _: &'a SessionAlive<'a>) -> Operation<'a, Result<()>> {
        Box::pin(async move {
            record(&self.directory, "cloud.prepare");
            if self.script.hold_cloud {
                fs::write(self.directory.join("cloud-ready"), b"").unwrap();
                let check = || self.control.check().map_err(Error::from);
                hold(&self.directory, "release-cloud", check).await?;
            }
            if self.script.ask {
                let choice = self
                    .control
                    .ask(crate::cloud::Question::Conflict, None)
                    .await?;
                record(&self.directory, &format!("answered:{choice:?}"));
                self.control.show(CloudPhase::Ready, None);
            }
            Ok(())
        })
    }
    fn started(&mut self) -> Result<()> {
        record(&self.directory, "cloud.started");
        Ok(())
    }
    fn finished(&mut self, confirmed: bool) -> Operation<'_, ()> {
        Box::pin(async move {
            if self.script.hold_finish {
                fs::write(self.directory.join("finish-ready"), b"").unwrap();
                let _ = hold(&self.directory, "release-finish", || Ok::<_, Error>(())).await;
            }
            self.control.show(CloudPhase::Syncing, None);
            record(&self.directory, &format!("cloud.finished:{confirmed}"));
        })
    }
    fn aborted(&mut self) {
        record(&self.directory, "cloud.aborted");
    }
}

mod lifecycle_of_epic {
    super::lifecycle_suite!(super::EpicHarness);
}

#[test]
fn an_epic_launch_waits_for_its_begin() {
    begin_opens_the_start_gate_once_and_only_for_its_account::<EpicHarness>();
}

#[test]
fn the_epic_journal_holds_no_authentication_material() {
    let run = launch::<EpicHarness>(script("exit 0\n"));
    let finished = run.until(Snapshot::finished);
    let record = fs::read_to_string(run.sessions.join(format!("{}.v2.json", finished.id))).unwrap();
    assert!(!record.contains("fixture-exchange-code") && !record.contains("exchange_code"));
    assert!(!record.contains("refresh_token"));
}

#[test]
fn a_running_epic_game_publishes_its_activity() {
    let run = launch::<EpicHarness>(script(HELD));
    run.until(|r| r.phase == Phase::Running);
    run.release("release-game");
    let finished = run.until(Snapshot::finished);
    assert_eq!(finished.phase, Phase::Finished);
    assert_eq!(finished.cloud.allowed_choices, []);
}

fn launch_plan(token: Option<&[u8]>) -> LaunchPlan {
    LaunchPlan {
        app_name: "game".into(),
        platform: Platform::Mac,
        activity: None,
        ownership_token: token.map(<[u8]>::to_vec),
        target: obelisk_core::launch::LaunchTarget {
            executable: "/install/game".into(),
            arguments: vec![],
            working_directory: "/install".into(),
            is_application_bundle: false,
        },
    }
}

#[test]
fn the_ownership_token_is_the_file_the_game_is_given_with_epicovt() {
    let plan = launch_plan(Some(b"ownership"));

    let file = ownership(&plan).expect("the plan has a token");

    assert_eq!(file.argument, "-epicovt");
    assert_eq!(file.folder, "obelisk-epic-ownership-");
    assert_eq!(file.name, "ownership.bin");
    assert_eq!(file.bytes, b"ownership");
    assert!(ownership(&launch_plan(None)).is_none());
}

#[test]
fn an_ownership_token_lives_as_long_as_its_prepared_game() {
    let root = tempfile::tempdir().unwrap();
    let directory = root.path().canonicalize().unwrap();
    let config = config(&directory, &directory.join("sessions"));
    fs::create_dir_all(&config.install).unwrap();
    fs::write(config.install.join("game"), thin(ARM64, true)).unwrap();
    let request = Request::<Epic>::new(
        config.clone(),
        ACCOUNT.into(),
        ExchangeCode {
            exchange_code: String::new(),
        },
        None,
    );
    let mut plan = launch_plan(Some(b"ownership"));
    plan.target.executable = config.install.join("game").to_string_lossy().into_owned();
    plan.target.working_directory = directory.to_string_lossy().into_owned();

    let started = started(&request, &plan).expect("the plan starts");

    let token = PathBuf::from(
        started.target.arguments[0]
            .strip_prefix("-epicovt=")
            .unwrap(),
    );
    assert_eq!(fs::read(&token).unwrap(), b"ownership");
    let host = Host {
        _ownership: started.kept,
    };
    assert!(token.exists());
    drop(host);
    assert!(!token.exists());
}

#[test]
fn a_windows_build_keeps_its_cloud_saves_in_its_wine_prefix_and_a_mac_build_in_its_home() {
    let root = tempfile::tempdir().unwrap();
    let directory = root.path().canonicalize().unwrap();
    let mut config = config(&directory, &directory.join("sessions"));
    assert_eq!(
        config.cloud_platform(),
        epic_kit::cloud::CloudPlatform::Mac {
            home: directory.clone()
        }
    );
    config.prefix = Some(directory.join("prefix"));
    config.platform = Platform::Windows;
    assert_eq!(
        config.cloud_platform(),
        epic_kit::cloud::CloudPlatform::Windows {
            profile: directory.join("prefix/drive_c/users/crossover"),
        }
    );
}

const STRANGER: &str = "fedcba9876543210fedcba9876543210";

#[test]
fn an_epic_configuration_is_unsupported_unless_its_root_is_plain_and_absolute_and_its_limits_hold()
{
    let root = tempfile::tempdir().unwrap();
    let directory = root.path().canonicalize().unwrap();
    let mac = || config(&directory, &directory.join("sessions"));
    let refused = [
        Configuration {
            root: "relative".into(),
            ..mac()
        },
        Configuration {
            root: directory.join("../archive"),
            ..mac()
        },
        Configuration {
            catalog_item_id: String::new(),
            ..mac()
        },
        Configuration {
            catalog_item_id: "c".repeat(257),
            ..mac()
        },
        Configuration {
            language: "l".repeat(129),
            ..mac()
        },
    ];
    for refused in refused {
        let error = validate::<Epic>(&refused, ACCOUNT).unwrap_err();
        assert!(matches!(error, Error::Unsupported), "{refused:?}");
        assert_eq!(error.to_string(), "backend.epic.cloud.unsupported");
    }
    // The limits themselves still pass.
    let limits = Configuration {
        catalog_item_id: "c".repeat(256),
        language: "l".repeat(128),
        ..mac()
    };
    validate::<Epic>(&limits, ACCOUNT).unwrap();
}

#[test]
fn an_epic_account_namespace_or_app_name_epic_would_not_name_is_refused_as_a_save_scope_before_any_path()
 {
    let root = tempfile::tempdir().unwrap();
    let directory = root.path().canonicalize().unwrap();
    let mac = || config(&directory, &directory.join("sessions"));
    let relative = || Configuration {
        home: "relative".into(),
        ..mac()
    };
    let scopes = [
        (mac(), "not-an-account"),
        // Epic names accounts in lowercase.
        (mac(), "0123456789ABCDEF0123456789ABCDEF"),
        (mac(), &"a".repeat(31)),
        (
            Configuration {
                namespace: "../namespace".into(),
                ..mac()
            },
            ACCOUNT,
        ),
        (
            Configuration {
                app_name: String::new(),
                ..mac()
            },
            ACCOUNT,
        ),
        (
            Configuration {
                app_name: "a".repeat(257),
                ..mac()
            },
            ACCOUNT,
        ),
        // The scope comes before the paths: a bad path does not hide it.
        (relative(), "not-an-account"),
    ];
    for (config, account) in scopes {
        let error = validate::<Epic>(&config, account).unwrap_err();
        assert!(matches!(error, Error::Vault(_)), "{config:?} {account}");
        assert_eq!(error.to_string(), "backend.epic.cloud.storage");
    }
    // A scope that is fine leaves the paths to refuse it.
    assert!(matches!(
        validate::<Epic>(&relative(), ACCOUNT),
        Err(Error::Unsupported)
    ));
    let named = Configuration {
        namespace: "name-space_1.0".into(),
        ..mac()
    };
    validate::<Epic>(&named, STRANGER).unwrap();
}

#[test]
fn an_epic_configuration_keeps_the_names_its_cloud_record_persists_and_still_loads_without_the_later_ones()
 {
    let root = tempfile::tempdir().unwrap();
    let directory = root.path().canonicalize().unwrap();
    let config = Configuration {
        title: "Rocket League".into(),
        platform: Platform::Windows,
        prefix: Some(directory.join("prefix")),
        ..config(&directory, &directory.join("sessions"))
    };

    let written = serde_json::to_value(&config).unwrap();

    assert_eq!(
        fields(&written),
        [
            "app_name",
            "catalog_item_id",
            "helper",
            "home",
            "install",
            "language",
            "namespace",
            "platform",
            "prefix",
            "root",
            "session_directory",
            "title",
        ]
    );
    assert_eq!(written["platform"], "Windows");
    assert_eq!(
        serde_json::from_value::<Configuration>(written.clone()).unwrap(),
        config
    );
    // A record from before titles and prefixes names neither, and the
    // configuration refuses any field it does not know.
    let mut older = written.clone();
    for field in ["title", "prefix"] {
        older.as_object_mut().unwrap().remove(field);
    }
    older["platform"] = "Mac".into();
    let loaded: Configuration = serde_json::from_value(older.clone()).unwrap();
    assert_eq!((loaded.title.as_str(), loaded.prefix), ("", None));
    older["added_later"] = true.into();
    assert!(serde_json::from_value::<Configuration>(older).is_err());
}
