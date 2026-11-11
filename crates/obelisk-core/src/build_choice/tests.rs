use super::*;
use std::sync::Arc;

use tokio::sync::Notify;

#[derive(Clone, Copy, Debug, PartialEq)]
enum FakeError {
    Cancelled,
    Unavailable(Unavailable),
    /// An outage, or anything else asking again may change.
    Failed,
    /// What asking again does not change.
    Refused,
}

impl From<Cancelled> for FakeError {
    fn from(_: Cancelled) -> Self {
        Self::Cancelled
    }
}

impl From<Unavailable> for FakeError {
    fn from(unavailable: Unavailable) -> Self {
        Self::Unavailable(unavailable)
    }
}

/// A program for Apple silicon, then one for Intel, as their first bytes
/// tell.
const ARM64: [u8; 8] = [0xcf, 0xfa, 0xed, 0xfe, 0x0c, 0x00, 0x00, 0x01];
const INTEL: [u8; 8] = [0xcf, 0xfa, 0xed, 0xfe, 0x07, 0x00, 0x00, 0x01];

/// A game whose storefront answers as scripted, and logs what it was asked.
struct FakeGame {
    offered: (Option<&'static str>, Option<&'static str>),
    key: Result<Option<u8>, ()>,
    /// What reading the Mac build's program gives: one for Apple silicon,
    /// with what the build keeps, `None` for one for Intel, or a failure.
    program: Result<Option<&'static str>, FakeError>,
    /// Holds `inspect` until notified.
    gate: Option<Arc<Notify>>,
    /// Stops the pick as `inspect` answers.
    stopped: Option<Arc<Choice>>,
    log: Mutex<Vec<&'static str>>,
}

impl FakeGame {
    fn new(mac: Option<&'static str>, windows: Option<&'static str>) -> Self {
        Self {
            offered: (mac, windows),
            key: Ok(Some(1)),
            program: Ok(Some("read")),
            gate: None,
            stopped: None,
            log: Mutex::new(Vec::new()),
        }
    }

    fn native(key: u8, kept: &'static str) -> Self {
        Self {
            key: Ok(Some(key)),
            program: Ok(Some(kept)),
            ..Self::new(Some("mac"), Some("windows"))
        }
    }

    fn not_native(key: u8) -> Self {
        Self {
            key: Ok(Some(key)),
            program: Ok(None),
            ..Self::new(Some("mac"), Some("windows"))
        }
    }

    fn asked(&self, call: &str) -> usize {
        self.log
            .lock()
            .unwrap()
            .iter()
            .filter(|asked| **asked == call)
            .count()
    }

    fn note(&self, call: &'static str) {
        self.log.lock().unwrap().push(call);
    }
}

impl GameBuilds for FakeGame {
    type Build = &'static str;
    type Key = u8;
    type Kept = &'static str;
    type Error = FakeError;
    type Failure = FakeError;

    async fn builds(&self) -> Result<Offered<&'static str>, FakeError> {
        self.note("builds");
        Ok(Offered {
            mac: self.offered.0,
            windows: self.offered.1,
        })
    }

    async fn key(&self, mac: &&'static str) -> Result<Option<u8>, FakeError> {
        assert_eq!(*mac, self.offered.0.unwrap());
        self.note("key");
        self.key.map_err(|()| FakeError::Failed)
    }

    async fn inspect(
        &self,
        mac: &&'static str,
        _cancel: &CancelToken,
    ) -> Result<Inspection<&'static str>, FakeError> {
        assert_eq!(*mac, self.offered.0.unwrap());
        self.note("inspect");
        if let Some(gate) = &self.gate {
            gate.notified().await;
        }
        if let Some(choice) = &self.stopped {
            choice.cancel();
        }
        let mut listing = Listing::default();
        listing.file("Game", (), ARM64.len() as u64);
        let launch = Launch {
            executable: "Game",
            libraries: &[],
        };
        let read = |(), _| async {
            self.program.map(|kept| match kept {
                Some(_) => ARM64.to_vec(),
                None => INTEL.to_vec(),
            })
        };
        let verdict = Verdict::of(&listing, launch, read).await?;
        Ok(verdict.keeping(|| self.program.unwrap().unwrap()))
    }

    fn is_refusal(failure: &FakeError) -> bool {
        *failure == FakeError::Refused
    }
}

type Choice = BuildChoice<u8, &'static str>;

async fn pick(choice: &Choice, game: &FakeGame) -> Result<Platform, FakeError> {
    choice.pick(async |pick| pick.of(game).await).await
}

async fn shown(choice: &Choice, game: &FakeGame) -> Option<(Platform, &'static str)> {
    let offered = game.builds().await.unwrap();
    choice.shown(game, offered).await
}

/// Keeps the verdict `native` for key 1, as a pick of its build finds it.
async fn judge(choice: &Choice, native: bool) {
    let game = match native {
        true => FakeGame::native(1, "one"),
        false => FakeGame::not_native(1),
    };
    pick(choice, &game).await.unwrap();
}

/// Whether a pick of a build keyed `key` reads it, which leaves its
/// verdict as `native` when it does.
async fn reads(choice: &Choice, key: u8, native: bool) -> bool {
    let game = match native {
        true => FakeGame::native(key, "again"),
        false => FakeGame::not_native(key),
    };
    pick(choice, &game).await.unwrap();
    game.asked("inspect") == 1
}

const MAC: Option<&str> = Some("mac");
const WINDOWS: Option<&str> = Some("windows");
/// A Mac build whose program is for Apple silicon, and what reading it
/// keeps; then one whose program is for Intel.
const NATIVE: Option<&str> = Some("read");
const NOT_NATIVE: Option<&str> = None;

#[tokio::test]
async fn a_download_installs_the_mac_build_when_its_game_runs_natively_else_the_windows_build() {
    use Platform::{Mac, Windows};
    let no_build = Err(FakeError::Unavailable(Unavailable::NoBuild));
    let not_native = Err(FakeError::Unavailable(Unavailable::NotNative));
    // (mac, windows, the Mac build's program, picked, whether it was read)
    let cases = [
        (None, None, NATIVE, no_build, false),
        (None, WINDOWS, NATIVE, Ok(Windows), false),
        (MAC, None, NATIVE, Ok(Mac), true),
        (MAC, WINDOWS, NATIVE, Ok(Mac), true),
        (MAC, WINDOWS, NOT_NATIVE, Ok(Windows), true),
        (MAC, None, NOT_NATIVE, not_native, true),
    ];
    for (mac, windows, program, expected, inspected) in cases {
        let game = FakeGame {
            program: Ok(program),
            ..FakeGame::new(mac, windows)
        };
        let choice = Choice::default();
        let case = (mac, windows, program);
        assert_eq!(pick(&choice, &game).await, expected, "{case:?}");
        assert_eq!(game.asked("inspect"), usize::from(inspected), "{case:?}");
        if mac.is_none() {
            assert_eq!(game.asked("key"), 0, "{case:?}: no Mac build, no key");
        }
    }
}

#[tokio::test]
async fn a_page_shows_the_mac_build_until_a_pick_found_its_game_is_not_native() {
    use Platform::{Mac, Windows};
    let (shows_mac, shows_windows) = (Some((Mac, "mac")), Some((Windows, "windows")));
    // (mac, windows, the verdict kept for key 1, the key the page gets, shown)
    let cases = [
        (None, None, Some(false), Ok(Some(1)), None),
        (None, WINDOWS, Some(false), Ok(Some(1)), shows_windows),
        (MAC, None, Some(false), Ok(Some(1)), shows_mac),
        (MAC, None, None, Ok(Some(1)), shows_mac),
        (MAC, WINDOWS, None, Ok(Some(1)), shows_mac),
        (MAC, WINDOWS, Some(true), Ok(Some(1)), shows_mac),
        (MAC, WINDOWS, Some(false), Ok(Some(1)), shows_windows),
        (MAC, WINDOWS, Some(false), Ok(Some(2)), shows_mac),
        (MAC, WINDOWS, Some(false), Ok(None), shows_mac),
        (MAC, WINDOWS, Some(false), Err(()), shows_mac),
        (MAC, WINDOWS, Some(true), Err(()), shows_mac),
    ];
    for (mac, windows, verdict, key, expected) in cases {
        let choice = Choice::default();
        if let Some(native) = verdict {
            judge(&choice, native).await;
        }
        let game = FakeGame {
            key,
            ..FakeGame::new(mac, windows)
        };
        let case = (mac, windows, verdict, key);
        assert_eq!(shown(&choice, &game).await, expected, "{case:?}");
        assert_eq!(game.asked("inspect"), 0, "{case:?}: a page reads no file");
        let both = mac.is_some() && windows.is_some();
        assert_eq!(game.asked("key"), usize::from(both), "{case:?}");
    }
}

#[tokio::test]
async fn a_mac_build_is_read_once_per_key() {
    let choice = Choice::default();
    let first = FakeGame::not_native(1);
    assert_eq!(pick(&choice, &first).await, Ok(Platform::Windows));
    assert_eq!(pick(&choice, &first).await, Ok(Platform::Windows));
    assert_eq!(first.asked("inspect"), 1, "the verdict of key 1 is kept");
    assert_eq!(first.asked("key"), 2);

    let other = FakeGame::native(2, "two");
    assert_eq!(pick(&choice, &other).await, Ok(Platform::Mac));
    assert_eq!(other.asked("inspect"), 1, "another key reads its build");

    // A build without a key is read each time and changes nothing kept.
    let unkeyed = FakeGame {
        key: Ok(None),
        ..FakeGame::not_native(0)
    };
    assert_eq!(pick(&choice, &unkeyed).await, Ok(Platform::Windows));
    assert_eq!(pick(&choice, &unkeyed).await, Ok(Platform::Windows));
    assert_eq!(unkeyed.asked("inspect"), 2);
    assert_eq!(shown(&choice, &unkeyed).await, Some((Platform::Mac, "mac")));
    assert_eq!(choice.take(&2), Some("two"));

    // A failed key or read remembers nothing.
    let failing_key = FakeGame {
        key: Err(()),
        ..FakeGame::not_native(3)
    };
    assert_eq!(pick(&choice, &failing_key).await, Err(FakeError::Failed));
    assert_eq!(failing_key.asked("inspect"), 0);
    let failing_read = FakeGame {
        program: Err(FakeError::Failed),
        ..FakeGame::not_native(3)
    };
    assert_eq!(pick(&choice, &failing_read).await, Err(FakeError::Failed));
    let read_again = FakeGame::not_native(3);
    assert_eq!(pick(&choice, &read_again).await, Ok(Platform::Windows));
    assert_eq!(
        read_again.asked("inspect"),
        1,
        "the next pick reads it again"
    );
}

#[tokio::test]
async fn a_native_pick_hands_what_it_read_to_that_build_s_download_once() {
    let choice = Choice::default();
    let one = FakeGame::native(1, "one");
    assert_eq!(pick(&choice, &one).await, Ok(Platform::Mac));
    assert_eq!(
        choice.take(&2),
        None,
        "another build's download reads its own"
    );
    assert_eq!(choice.take(&1), Some("one"));
    assert_eq!(choice.take(&1), None, "a manifest starts one download");

    // A cache hit leaves what is kept alone.
    let two = FakeGame::native(2, "two");
    assert_eq!(pick(&choice, &two).await, Ok(Platform::Mac));
    assert_eq!(pick(&choice, &one).await, Ok(Platform::Mac));
    assert_eq!(one.asked("inspect"), 1);
    // So does a build without a key.
    let unkeyed = FakeGame {
        key: Ok(None),
        ..FakeGame::native(0, "unkeyed")
    };
    assert_eq!(pick(&choice, &unkeyed).await, Ok(Platform::Mac));
    assert_eq!(choice.take(&2), Some("two"));

    // A native pick of another build replaces it.
    assert_eq!(
        pick(&choice, &FakeGame::native(3, "three")).await,
        Ok(Platform::Mac)
    );
    assert_eq!(
        pick(&choice, &FakeGame::native(4, "four")).await,
        Ok(Platform::Mac)
    );
    assert_eq!(choice.take(&3), None);
    assert_eq!(choice.take(&4), Some("four"));

    // A keyed pick found not native clears it.
    assert_eq!(
        pick(&choice, &FakeGame::native(5, "five")).await,
        Ok(Platform::Mac)
    );
    assert_eq!(
        pick(&choice, &FakeGame::not_native(6)).await,
        Ok(Platform::Windows)
    );
    assert_eq!(choice.take(&5), None);
}

#[tokio::test]
async fn a_mac_build_its_storefront_refuses_gives_way_to_the_windows_build_and_keeps_that_verdict()
{
    let choice = Choice::default();
    assert_eq!(
        pick(&choice, &FakeGame::native(5, "five")).await,
        Ok(Platform::Mac)
    );
    let refused = FakeGame {
        program: Err(FakeError::Refused),
        ..FakeGame::native(1, "one")
    };
    assert_eq!(pick(&choice, &refused).await, Ok(Platform::Windows));
    assert_eq!(pick(&choice, &refused).await, Ok(Platform::Windows));
    assert_eq!(refused.asked("inspect"), 1, "the verdict of key 1 is kept");
    // As for a game that does not run natively: its page shows the Windows
    // build, and no download starts from what another build's pick read.
    assert_eq!(
        shown(&choice, &refused).await,
        Some((Platform::Windows, "windows"))
    );
    assert_eq!(choice.take(&5), None);
    assert_eq!(choice.take(&1), None);

    // Without a Windows build to give way to, the game cannot be installed.
    let mac_only = FakeGame {
        key: Ok(Some(2)),
        program: Err(FakeError::Refused),
        ..FakeGame::new(MAC, None)
    };
    assert_eq!(
        pick(&choice, &mac_only).await,
        Err(FakeError::Unavailable(Unavailable::NotNative))
    );
}

#[tokio::test]
async fn a_failure_that_is_no_refusal_fails_the_pick_and_keeps_no_verdict() {
    let choice = Choice::default();
    assert_eq!(
        pick(&choice, &FakeGame::native(5, "five")).await,
        Ok(Platform::Mac)
    );
    // Falling back on an outage could install the Windows build of a game
    // that runs natively.
    let unreachable = FakeGame {
        program: Err(FakeError::Failed),
        ..FakeGame::native(1, "one")
    };
    assert_eq!(pick(&choice, &unreachable).await, Err(FakeError::Failed));
    assert_eq!(
        shown(&choice, &unreachable).await,
        Some((Platform::Mac, "mac")),
        "the page still shows the Mac build"
    );
    assert_eq!(choice.take(&5), Some("five"), "what was kept is left alone");
    assert!(
        reads(&choice, 1, true).await,
        "the next pick reads it again"
    );
}

#[tokio::test]
async fn a_refusal_answered_as_the_pick_is_stopped_ends_cancelled() {
    let choice = Arc::new(Choice::default());
    let refused = FakeGame {
        program: Err(FakeError::Refused),
        stopped: Some(Arc::clone(&choice)),
        ..FakeGame::native(1, "one")
    };
    assert_eq!(pick(&choice, &refused).await, Err(FakeError::Cancelled));
    assert!(
        reads(&choice, 1, true).await,
        "a stopped pick keeps no verdict"
    );
}

#[tokio::test]
async fn a_build_keeps_what_its_download_starts_from_only_when_its_game_runs_natively() {
    let mut listing = Listing::default();
    listing.file("Game.app/Contents/MacOS/Game", ARM64, ARM64.len() as u64);
    listing.file("Game.app/Contents/Frameworks/libapi.dylib", INTEL, 8);
    let read = |head: [u8; 8], _| async move { Ok::<_, FakeError>(head.to_vec()) };
    let launch = |libraries| Launch {
        executable: "Game.app/Contents/MacOS/Game",
        libraries,
    };
    let alone = Verdict::of(&listing, launch(&[]), read).await.unwrap();
    assert_eq!(alone.keeping(|| "kept").into_kept(), Some("kept"));
    // The library a launch pairs with the program must run natively too.
    let paired = Verdict::of(&listing, launch(&["libapi.dylib"]), read)
        .await
        .unwrap();
    let kept =
        paired.keeping(|| -> &str { panic!("nothing is kept of a build that is not picked") });
    assert_eq!(kept.into_kept(), None);
    assert_eq!(Inspection::<&str>::no_program().into_kept(), None);

    let failing = |_: [u8; 8], _| async { Err::<Vec<u8>, _>(FakeError::Failed) };
    let failed = Verdict::of(&listing, launch(&[]), failing).await;
    assert_eq!(failed.map(|_| ()), Err(FakeError::Failed));
}

#[tokio::test]
async fn cancel_stops_a_pick_and_forgets_what_it_read() {
    let choice = Choice::default();
    // Harmless with nothing in flight.
    choice.cancel();

    assert_eq!(
        pick(&choice, &FakeGame::not_native(1)).await,
        Ok(Platform::Windows)
    );
    assert_eq!(
        pick(&choice, &FakeGame::native(2, "two")).await,
        Ok(Platform::Mac)
    );

    let gated = FakeGame {
        gate: Some(Arc::new(Notify::new())),
        ..FakeGame::native(3, "three")
    };
    let cancelling = async {
        while gated.asked("inspect") == 0 {
            tokio::task::yield_now().await;
        }
        choice.cancel();
    };
    let (picked, ()) = tokio::join!(pick(&choice, &gated), cancelling);
    assert_eq!(picked, Err(FakeError::Cancelled));
    assert_eq!(choice.take(&2), None, "a cancel forgets what was read");
    assert!(!reads(&choice, 1, false).await, "verdicts survive a cancel");
    assert!(!reads(&choice, 2, true).await);

    // The next pick runs on a fresh token, and reads the stopped build again.
    let again = FakeGame::native(3, "three");
    assert_eq!(pick(&choice, &again).await, Ok(Platform::Mac));
    assert_eq!(again.asked("inspect"), 1, "a stopped read keeps no verdict");
    assert_eq!(choice.take(&3), Some("three"));
}

#[tokio::test]
async fn a_pick_runs_inside_what_the_storefront_wraps_around_it() {
    let choice = Choice::default();
    let failing = FakeGame {
        program: Err(FakeError::Failed),
        ..FakeGame::native(1, "one")
    };
    let gated = FakeGame {
        gate: Some(Arc::new(Notify::new())),
        ..FakeGame::native(1, "one")
    };
    // As a token funnel retries its call: the second attempt runs under the
    // same pick, which a cancel still stops.
    let attempts = choice.pick(async |pick| {
        assert_eq!(pick.of(&failing).await, Err(FakeError::Failed));
        pick.of(&gated).await
    });
    let cancelling = async {
        while gated.asked("inspect") == 0 {
            tokio::task::yield_now().await;
        }
        choice.cancel();
    };
    let (picked, ()) = tokio::join!(attempts, cancelling);
    assert_eq!(picked, Err(FakeError::Cancelled));
    assert_eq!(failing.asked("inspect"), 1);
    assert_eq!(gated.asked("inspect"), 1);
    assert!(
        reads(&choice, 1, true).await,
        "neither attempt kept a verdict"
    );
}
