//! The GOG launch through the supervisor's interface, with nothing replaced
//! but process inspection: a real installation folder holding its receipt and
//! play task, GOG's own supervisor, and the shared exec helper. GOG's launch
//! has no fake services, so its handoff is written here as the wire carries
//! it. The shared lifecycle scenarios script a Cloud sync and presence that a
//! GOG launch does not have, so none of them runs for it.
use super::*;
use crate::gog::Gog;
use crate::supervisor::architecture::tests::{ARM64, INTEL, thin};

const ACCOUNT: &str = "48628349957132247";
const PRODUCT: &str = "1207658924";

pub(crate) struct GogHarness;

/// The installation folder of the Mac build, in the test directory.
fn install(directory: &Path) -> PathBuf {
    directory.join("install")
}

/// A completed Mac installation whose primary play task starts `Game`: an
/// arm64 program, an Intel one for an `intel` script, none for a `missing`
/// one.
fn installed(directory: &Path, script: &Script) {
    let install = install(directory);
    fs::create_dir_all(&install).unwrap();
    if !script.missing {
        let cpu = if script.intel { INTEL } else { ARM64 };
        fs::write(install.join("Game"), thin(cpu, true)).unwrap();
    }
    let info = serde_json::json!({"playTasks": [
        {"category": "game", "isPrimary": true, "path": "Game", "type": "FileTask"},
    ]});
    fs::write(
        install.join(format!("goggame-{PRODUCT}.info")),
        serde_json::to_vec(&info).unwrap(),
    )
    .unwrap();
    let receipt = gog_kit::client::Installation {
        product_id: PRODUCT.into(),
        user_id: ACCOUNT.into(),
        directory: install.to_string_lossy().into_owned(),
        platform: obelisk_core::Platform::Mac,
        total_bytes: 1,
        build_id: "1".into(),
        version_name: "1.0".into(),
        language: "en-US".into(),
        content_root: String::new(),
        script_interpreter: false,
        products: Vec::new(),
        dependencies: Vec::new(),
    };
    fs::write(
        install.join(obelisk_core::install::RECEIPT_FILE),
        serde_json::to_vec(&receipt).unwrap(),
    )
    .unwrap();
}

impl Harness for GogHarness {
    type P = Gog;
    const NAME: &'static str = "gog";
    fn account() -> String {
        ACCOUNT.into()
    }
    fn other_account() -> String {
        unimplemented!("only the lifecycle scenarios ask for another account")
    }
    fn handoff(
        directory: &Path,
        sessions: &Path,
        id: &str,
        pipe: &mut std::os::unix::net::UnixStream,
    ) -> Result<(), Error> {
        let script: Script =
            serde_json::from_slice(&fs::read(directory.join("script.json")).unwrap()).unwrap();
        installed(directory, &script);
        // The Rosetta helper app sits beside `SteamNative`, in the test directory.
        let helper = directory.join("SteamNative/obelisk-steam-host");
        write_helper(&helper, directory, &script);
        storage::send(
            pipe,
            &serde_json::json!({
                "config": {
                    "home": directory,
                    "install": install(directory),
                    "session_directory": sessions,
                    "helper": helper,
                    "product_id": PRODUCT,
                    "platform": "macos",
                    "title": "The Witcher",
                    "prefix": null,
                },
                "id": id,
                "account_id": ACCOUNT,
                "environment": crate::native::environment(directory),
                "wine": null,
            }),
        )
    }
    fn serve(
        input: std::io::StdinLock<'static>,
        output: std::io::StderrLock<'static>,
    ) -> Result<(), String> {
        crate::gog::serve(input, output, inspect).map_err(|error| format!("{error:?}"))
    }
    fn previous_build_record(_: &str, _: &Identity, _: &Path) -> serde_json::Value {
        unimplemented!("only the lifecycle scenarios read a previous build's record")
    }
}

#[test]
fn a_gog_mac_game_runs_in_the_folder_its_play_task_names_and_its_session_has_no_cloud() {
    let run = launch::<GogHarness>(script(QUICK));

    let finished = run.until(Record::finished);

    assert_eq!(finished.phase, Phase::Finished);
    assert_eq!(finished.exit_code, Some(5));
    assert!(finished.failure_reason().is_none());
    assert_eq!(finished.account_id, ACCOUNT);
    assert_eq!(finished.store.product_id, PRODUCT);
    assert_eq!(finished.cloud.phase, crate::cloud::Phase::Unsupported);
    assert_eq!(finished.cloud.allowed_choices, []);
    assert!(
        install(&run.directory).join("ran").exists(),
        "a play task without a folder starts its game in its program's"
    );
    assert_eq!(run.exit_code(), "0");
}

#[test]
fn a_running_gog_game_shows_no_cloud_until_its_session_ends_and_publishes_no_presence() {
    let run = launch::<GogHarness>(script(HELD));

    let running = run.until(|record| record.phase == Phase::Running);

    assert!(running.game.is_some() && running.starting.is_none());
    assert_eq!(running.cloud.phase, crate::cloud::Phase::Idle);
    assert_eq!(running.cloud.allowed_choices, []);
    assert!(run.presence().is_empty(), "GOG publishes no presence");
    fs::write(install(&run.directory).join("release-game"), b"").unwrap();
    let finished = run.until(Record::finished);
    assert_eq!(
        (finished.phase, finished.exit_code),
        (Phase::Finished, Some(3))
    );
    assert_eq!(finished.cloud.phase, crate::cloud::Phase::Unsupported);
}

#[test]
fn an_intel_gog_mac_build_never_runs_and_its_session_fails_with_the_launch_key() {
    let run = launch::<GogHarness>(Script {
        intel: true,
        ..script(QUICK)
    });

    let failed = run.until(Record::finished);

    assert_eq!(failed.phase, Phase::Failed);
    assert_eq!(
        failed.error.as_deref(),
        Some("backend.runtime.macBuildNotNative")
    );
    assert_eq!(
        failed.failure_reason(),
        Some(crate::supervisor::FailureReason::Failed)
    );
    assert!(!install(&run.directory).join("ran").exists());
    assert_eq!(failed.cloud.phase, crate::cloud::Phase::Unsupported);
}

#[test]
fn a_gog_game_whose_program_is_gone_fails_its_session_with_the_gog_key_and_runs_nothing() {
    let run = launch::<GogHarness>(Script {
        missing: true,
        ..script(QUICK)
    });

    let failed = run.until(Record::finished);

    assert_eq!(failed.phase, Phase::Failed);
    assert_eq!(
        failed.error.as_deref(),
        Some("backend.gog.launch.fileMissing")
    );
    assert!(!install(&run.directory).join("ran").exists());
    assert_eq!(failed.cloud.phase, crate::cloud::Phase::Unsupported);
}
