//! The Epic launch through the shared lifecycle suite. Its Epic services and
//! EGS Cloud are fakes that follow the suite's script; the Begin gate, the
//! launch plan's target checks and the handoff are the adapter's own.
use super::*;
use crate::supervisor::architecture::tests::{ARM64, INTEL, thin};
use crate::supervisor::session::tests::{
    HELD, Harness, Script, begin_opens_the_start_gate_once_and_only_for_its_account,
    fixture_directory, hold, inspect, launch, lifecycle_suite, record, script, write_helper,
};
use std::{collections::HashMap, fs};

const ACCOUNT: &str = "0123456789abcdef0123456789abcdef";

pub(crate) struct EpicHarness;

fn config(directory: &Path, sessions: &Path) -> Configuration {
    Configuration {
        root: directory.join("archive"),
        home: directory.into(),
        install: directory.join("install"),
        session_directory: sessions.into(),
        // The Rosetta helper app sits beside `SteamNative`, in the test directory.
        helper: directory.join("SteamNative/forge-steam-host"),
        namespace: "namespace".into(),
        catalog_item_id: "catalog".into(),
        app_name: "game".into(),
        platform: "Mac".into(),
        language: "en".into(),
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
            &Request {
                config: config(directory, sessions),
                id: id.into(),
                account_id: ACCOUNT.into(),
                exchange_code: "fixture-exchange-code".into(),
                environment: crate::native::environment(directory),
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
            fs::write(
                &executable,
                thin(if script.intel { INTEL } else { ARM64 }, true),
            )
            .unwrap();
        }
        Ok(LaunchPlan {
            app_name: config.app_name.clone(),
            platform: config.platform.clone(),
            activity: Some(Activity {
                id: "fixture-activity".into(),
                name: None,
            }),
            ownership_token: None,
            target: forge_core::launch::LaunchTarget {
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
    async fn disconnect(&self) {}
    async fn cloud(
        self: &Arc<Self>,
        _: Configuration,
        _: String,
        _: String,
        control: Arc<Control>,
    ) -> Result<Box<dyn EpicCloud>> {
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
impl EpicCloud for FakeCloud {
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

fn launch_plan(install: &Path, working: &Path) -> LaunchPlan {
    LaunchPlan {
        app_name: "game".into(),
        platform: "Mac".into(),
        activity: None,
        ownership_token: None,
        target: forge_core::launch::LaunchTarget {
            executable: install.join("game").to_string_lossy().into_owned(),
            arguments: vec![],
            working_directory: working.to_string_lossy().into_owned(),
            is_application_bundle: false,
        },
    }
}

#[test]
fn an_epic_target_checks_the_full_macho_before_repairing_its_permission() {
    let root = tempfile::tempdir().unwrap();
    let directory = root.path().canonicalize().unwrap();
    let config = config(&directory, &directory.join("sessions"));
    fs::create_dir_all(&config.install).unwrap();
    let executable = config.install.join("game");
    let plan = launch_plan(&config.install, &directory);
    fs::write(&executable, b"\xcf\xfa\xed\xfe").unwrap();
    fs::set_permissions(&executable, fs::Permissions::from_mode(0o600)).unwrap();
    assert!(matches!(
        native_target(&plan, &config, &HashMap::new()),
        Err(Error::Unsupported)
    ));
    assert_eq!(
        fs::metadata(&executable).unwrap().permissions().mode() & 0o100,
        0
    );
    for (cpu, expected) in [(INTEL, Architecture::Intel), (ARM64, Architecture::Arm64)] {
        fs::write(&executable, thin(cpu, true)).unwrap();
        let (target, architecture, ownership) =
            native_target(&plan, &config, &HashMap::new()).unwrap();
        assert_eq!(architecture, expected);
        assert_eq!(target.executable, executable.canonicalize().unwrap());
        assert!(ownership.is_none());
        assert_ne!(
            fs::metadata(&executable).unwrap().permissions().mode() & 0o100,
            0
        );
    }
    // Nothing outside the installation is ever a target.
    let outside = launch_plan(&directory, &directory);
    fs::write(directory.join("game"), thin(ARM64, true)).unwrap();
    assert!(matches!(
        native_target(&outside, &config, &HashMap::new()),
        Err(Error::Unsupported)
    ));
}

#[test]
fn an_ownership_token_lives_as_long_as_its_prepared_game() {
    let root = tempfile::tempdir().unwrap();
    let directory = root.path().canonicalize().unwrap();
    let config = config(&directory, &directory.join("sessions"));
    fs::create_dir_all(&config.install).unwrap();
    fs::write(config.install.join("game"), thin(ARM64, true)).unwrap();
    let mut plan = launch_plan(&config.install, &directory);
    plan.ownership_token = Some(b"ownership".to_vec());
    let (target, _, ownership) = native_target(&plan, &config, &HashMap::new()).unwrap();
    let token = PathBuf::from(target.arguments[0].strip_prefix("-epicovt=").unwrap());
    assert_eq!(fs::read(&token).unwrap(), b"ownership");
    let host = Host {
        _ownership: ownership,
    };
    assert!(token.exists());
    drop(host);
    assert!(!token.exists());
}
