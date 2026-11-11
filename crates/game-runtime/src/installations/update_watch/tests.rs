//! The update watch through its interface, with the instants a test gives
//! it. Steam stands in for every storefront.
use super::*;
use crate::installations::tests::{half, product};
use crate::steam::Steam;
use obelisk_core::Platform;

const MINUTE: Duration = Duration::from_secs(60);
const MILLISECOND: Duration = Duration::from_millis(1);

/// The installation of `app_id`'s `platform`.
fn installed(app_id: u32, platform: Platform) -> Receipt<Steam> {
    Receipt {
        product: product(app_id),
        name: "A game".into(),
        directory: format!("/Games/Steam/{}", Steam::receipt_id(&app_id, platform)),
        platform,
        total_bytes: 100,
        needs_verification: false,
    }
}

fn mac(app_id: u32) -> Receipt<Steam> {
    installed(app_id, Platform::Mac)
}

fn windows(app_id: u32) -> Receipt<Steam> {
    installed(app_id, Platform::Windows)
}

fn ids<'a>(receipts: impl IntoIterator<Item = &'a Receipt<Steam>>) -> Vec<String> {
    receipts.into_iter().map(Receipt::id).collect()
}

/// A watch whose check at `at` learned that each of `newer` has a newer
/// build, all of them shown.
fn knowing(newer: &[Receipt<Steam>], at: Instant) -> UpdateWatch {
    let mut watch = UpdateWatch::default();
    assert!(watch.check(Asked::Routine, at, newer).is_some());
    let answers = newer.iter().map(|receipt| (receipt.clone(), true));
    assert!(!watch.checked(at, answers.collect::<Vec<_>>(), newer));
    watch
}

fn automatic(receipt: &str) -> Job {
    Job::Transfer {
        automatic: Some(receipt.into()),
    }
}

const PLAYERS: Job = Job::Transfer { automatic: None };
const ANYWHERE: Opening<u32> = Opening::Open { except: Vec::new() };

/// What the watch updates on its own now, of `shown`, wherever a Transfer
/// may queue.
fn due(watch: &UpdateWatch, shown: &[Receipt<Steam>]) -> Vec<String> {
    ids(watch.due(shown, &ANYWHERE))
}

#[test]
fn a_routine_check_waits_ten_minutes_after_the_last_one_ended_and_a_reconnection_never_waits() {
    let start = Instant::now();
    let shown = [mac(620)];
    let mut never = UpdateWatch::default();
    assert!(never.check(Asked::Routine, start, &shown).is_some());
    let mut never = UpdateWatch::default();
    assert!(never.check(Asked::Reconnected, start, &shown).is_some());

    // The last check took fifteen seconds, and learned nothing.
    let ended = start + Duration::from_secs(15);
    let cases = [
        (Duration::ZERO, false),
        (MINUTE, false),
        (10 * MINUTE - MILLISECOND, false),
        (10 * MINUTE, true),
        (10 * MINUTE + MILLISECOND, true),
        (600 * MINUTE, true),
    ];
    for (later, routine) in cases {
        for (asked, starts) in [(Asked::Routine, routine), (Asked::Reconnected, true)] {
            let mut watch = UpdateWatch::default();
            assert!(watch.check(Asked::Routine, start, &shown).is_some());
            assert!(!watch.checked(ended, [], &shown));
            assert_eq!(
                watch.check(asked, ended + later, &shown).is_some(),
                starts,
                "{asked:?}, {later:?} after the last check ended"
            );
        }
    }
}

#[test]
fn a_check_asks_about_every_installation_shown_but_those_an_interrupted_transfer_left() {
    let start = Instant::now();
    let shown = [mac(620), half(windows(620)), mac(730)];
    let mut watch = UpdateWatch::default();
    assert_eq!(
        watch.check(Asked::Routine, start, &shown),
        Some(vec![mac(620), mac(730)])
    );

    // With nothing to ask about, the check still runs, and counts.
    let mut watch = UpdateWatch::default();
    assert_eq!(
        watch.check::<Steam>(Asked::Routine, start, &[]),
        Some(vec![])
    );
    assert_eq!(watch.check(Asked::Routine, start, &shown), None);
    assert!(!watch.checked::<Steam>(start, [], &[]));
    assert_eq!(watch.check(Asked::Routine, start + MINUTE, &shown), None);
}

#[test]
fn a_check_asked_for_while_one_asks_starts_none_and_a_reconnection_makes_one_more_follow() {
    use Asked::{Reconnected, Routine};
    let start = Instant::now();
    let shown = [mac(620)];
    let cases: [(&[Asked], bool); 6] = [
        (&[], false),
        (&[Routine], false),
        (&[Routine, Routine], false),
        (&[Reconnected], true),
        (&[Reconnected, Reconnected], true),
        (&[Routine, Reconnected, Routine], true),
    ];
    for (first, meanwhile) in [(Routine, cases), (Reconnected, cases)] {
        for (during, follows) in meanwhile {
            let mut watch = UpdateWatch::default();
            assert!(watch.check(first, start, &shown).is_some());
            for asked in during {
                assert_eq!(
                    watch.check(*asked, start, &shown),
                    None,
                    "{asked:?} during a check"
                );
            }
            assert_eq!(
                watch.checked(start, [], &shown),
                follows,
                "{during:?} during a check"
            );
            if follows {
                // The one that follows starts at once, and nothing follows it.
                assert!(watch.check(Reconnected, start, &shown).is_some());
                assert!(!watch.checked(start, [], &shown));
            }
        }
    }
}

#[test]
fn a_check_learns_each_answer_and_the_last_learned_shows_last() {
    let start = Instant::now();
    let shown = [mac(620), windows(620), mac(730)];
    let mut watch = UpdateWatch::default();
    assert!(watch.known().is_empty());

    assert!(watch.check(Asked::Routine, start, &shown).is_some());
    let answers = [(mac(730), true), (mac(620), true), (windows(620), false)];
    assert!(!watch.checked(start, answers, &shown));
    assert_eq!(watch.known(), ["730-macos", "620-macos"]);

    // An installation the storefront could not tell about keeps what was
    // known, and a newer build withdrawn is no longer known.
    assert!(watch.check(Asked::Reconnected, start, &shown).is_some());
    let answers = [(mac(730), true), (windows(620), true)];
    assert!(!watch.checked(start, answers, &shown));
    assert_eq!(watch.known(), ["620-macos", "730-macos", "620-windows"]);
    assert!(watch.check(Asked::Reconnected, start, &shown).is_some());
    assert!(!watch.checked(start, [(mac(620), false)], &shown));
    assert_eq!(watch.known(), ["730-macos", "620-windows"]);
}

#[test]
fn an_answer_about_an_installation_that_no_longer_shows_as_it_was_asked_is_dropped() {
    let start = Instant::now();
    let asked = [mac(620), mac(730)];
    let larger = Receipt {
        total_bytes: 250,
        ..mac(620)
    };
    let cases: [(&str, Vec<Receipt<Steam>>); 3] = [
        ("its Transfer began", vec![half(mac(620)), mac(730)]),
        (
            "its Transfer installed another build",
            vec![larger, mac(730)],
        ),
        ("it was uninstalled", vec![mac(730)]),
    ];
    for (case, shown) in cases {
        // Nothing was known of it: its newer build stays unknown.
        let mut watch = UpdateWatch::default();
        assert_eq!(
            watch.check(Asked::Routine, start, &asked),
            Some(asked.to_vec())
        );
        let answers = [(mac(620), true), (mac(730), true)];
        assert!(!watch.checked(start, answers, &shown));
        assert_eq!(watch.known(), ["730-macos"], "{case}");

        // Its newer build was known: the answer does not withdraw it.
        let mut watch = UpdateWatch::default();
        watch.learned("620-macos".into(), true);
        assert!(watch.check(Asked::Routine, start, &asked).is_some());
        let answers = [(mac(620), false), (mac(730), true)];
        assert!(!watch.checked(start, answers, &shown));
        assert_eq!(watch.known(), ["620-macos", "730-macos"], "{case}");
    }
}

#[test]
fn an_answer_about_an_installation_a_transfer_installed_over_while_the_storefront_was_asked_is_dropped()
 {
    let start = Instant::now();
    let shown = [mac(620), mac(730)];
    let mut watch = UpdateWatch::default();
    assert!(watch.check(Asked::Routine, start, &shown).is_some());
    // Portal 2's Transfer began and ended while the storefront was asked,
    // and left its receipt as it was.
    watch.installed("620-macos");
    let answers = [(mac(620), true), (mac(730), true)];
    assert!(!watch.checked(start, answers, &shown));
    assert_eq!(watch.known(), ["730-macos"]);

    // A Transfer that ended before the storefront is asked drops nothing.
    watch.installed("730-macos");
    assert!(watch.check(Asked::Reconnected, start, &shown).is_some());
    let answers = [(mac(620), true), (mac(730), true)];
    assert!(!watch.checked(start, answers, &shown));
    assert_eq!(watch.known(), ["620-macos", "730-macos"]);
}

#[test]
fn a_newer_build_is_known_until_a_transfer_installs_it_or_its_installation_stops_showing() {
    let start = Instant::now();
    let shown = [mac(620), windows(620), mac(730)];
    let mut watch = knowing(&shown, start);
    assert_eq!(watch.known(), ["620-macos", "620-windows", "730-macos"]);

    watch.installed("620-windows");
    assert_eq!(watch.known(), ["620-macos", "730-macos"]);
    watch.installed("440-macos");
    assert_eq!(watch.known(), ["620-macos", "730-macos"]);

    // Portal 2's disk leaves, then comes back.
    watch.receipts_changed(&[mac(730)]);
    assert_eq!(watch.known(), ["730-macos"]);
    watch.receipts_changed(&shown);
    assert_eq!(watch.known(), ["730-macos"]);

    // A launch asks about its own installation.
    watch.learned("620-macos".into(), true);
    assert_eq!(watch.known(), ["730-macos", "620-macos"]);
    watch.learned("730-macos".into(), false);
    assert_eq!(watch.known(), ["620-macos"]);
}

#[test]
fn a_transfer_installs_a_new_build_updates_one_with_a_newer_build_and_repairs_any_other() {
    let watch = knowing(&[windows(620)], Instant::now());
    let cases = [
        ("no installation", None, queue::Kind::Install),
        ("a newer build", Some(windows(620)), queue::Kind::Update),
        ("its other build's", Some(mac(620)), queue::Kind::Repair),
        ("nothing known", Some(mac(730)), queue::Kind::Repair),
    ];
    for (case, landing, kind) in cases {
        assert_eq!(watch.transfer_kind(landing.as_ref()), kind, "{case}");
    }
}

#[test]
fn automatic_updates_queue_each_newer_build_once_the_player_wants_them_and_the_gate_lets_them() {
    let shown = [mac(730), mac(620), windows(620), mac(440)];
    let mut watch = knowing(&[windows(620), mac(730), mac(620)], Instant::now());
    assert_eq!(due(&watch, &shown), Vec::<String>::new(), "they are off");

    assert_eq!(watch.set_automatic(true, []), Vec::<u64>::new());
    assert_eq!(
        due(&watch, &shown),
        ["730-macos", "620-macos", "620-windows"],
        "in the order the installations show"
    );
    assert_eq!(due(&watch, &[mac(620)]), ["620-macos"]);

    // A Transfer of Portal 2 is queued, or its game runs.
    let held = Opening::Open { except: vec![620] };
    assert_eq!(ids(watch.due(&shown, &held)), ["730-macos"]);
    assert_eq!(
        ids(watch.due(&shown, &Opening::Closed)),
        Vec::<String>::new()
    );

    watch.installed("730-macos");
    assert_eq!(due(&watch, &shown), ["620-macos", "620-windows"]);
}

#[test]
fn an_automatic_update_is_tried_once_then_waits_for_the_next_check() {
    let start = Instant::now();
    let shown = [mac(620), mac(730)];
    let mut watch = knowing(&shown, start);
    assert_eq!(watch.set_automatic(true, []), Vec::<u64>::new());

    // Only Obelisk's own update counts as a try.
    for job in [PLAYERS, Job::Launch, Job::Move] {
        watch.started(&job);
    }
    assert_eq!(due(&watch, &shown), ["620-macos", "730-macos"]);

    // Queued, it is not tried yet; started, it is, though it fails or is
    // stopped and its newer build stays known.
    watch.started(&automatic("620-macos"));
    assert_eq!(due(&watch, &shown), ["730-macos"]);
    assert_eq!(watch.known(), ["620-macos", "730-macos"]);
    assert_eq!(
        watch.check(Asked::Routine, start + MINUTE, &shown),
        None,
        "the last check still stands"
    );
    assert_eq!(due(&watch, &shown), ["730-macos"]);

    // The next check lets it try again, whatever it learns.
    assert!(
        watch
            .check(Asked::Routine, start + 10 * MINUTE, &shown)
            .is_some()
    );
    assert_eq!(due(&watch, &shown), ["730-macos"], "while it asks");
    assert!(!watch.checked(start + 10 * MINUTE, [], &shown));
    assert_eq!(due(&watch, &shown), ["620-macos", "730-macos"]);
}

#[test]
fn turning_automatic_updates_off_withdraws_obelisks_own_updates_and_lets_them_queue_again() {
    let shown = [mac(620), mac(730), mac(440)];
    let mut watch = knowing(&shown, Instant::now());
    assert_eq!(watch.set_automatic(true, []), Vec::<u64>::new());
    // Team Fortress 2's failed and left the queue, Portal 2's is under way,
    // Counter-Strike's waits.
    watch.started(&automatic("440-macos"));
    watch.started(&automatic("620-macos"));
    let (running, waiting) = (automatic("620-macos"), automatic("730-macos"));
    let queued = [
        (1, &running),
        (2, &PLAYERS),
        (3, &Job::Launch),
        (4, &Job::Move),
        (5, &waiting),
    ];
    assert_eq!(due(&watch, &shown), ["730-macos"]);

    assert_eq!(watch.set_automatic(true, queued), Vec::<u64>::new());
    assert_eq!(watch.set_automatic(false, queued), [1, 5]);
    assert_eq!(due(&watch, &shown), Vec::<String>::new());
    assert_eq!(watch.known(), ["620-macos", "730-macos", "440-macos"]);

    // Turned back on, those withdrawn queue again; the failed one waits.
    assert_eq!(watch.set_automatic(true, []), Vec::<u64>::new());
    assert_eq!(due(&watch, &shown), ["620-macos", "730-macos"]);
}
