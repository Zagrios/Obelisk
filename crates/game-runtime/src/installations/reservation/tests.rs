use super::*;

/// Nothing holds the installations.
fn idle() -> Reservation<u32> {
    Reservation {
        signed_out: false,
        diagnostic: false,
        maintaining: false,
        recovering: false,
        session: None,
        launching: None,
        uninstalling: None,
        downloading: false,
        transferring: None,
        queued: vec![],
    }
}

fn open(except: Vec<u32>) -> Opening<u32> {
    Opening::Open { except }
}

const TRANSFER: Job = Job::Transfer { automatic: None };

/// The jobs a queued Transfer does, as a case names them.
fn jobs() -> [(&'static str, Job); 3] {
    [
        ("transfer", TRANSFER),
        (
            "automatic update",
            Job::Transfer {
                automatic: Some("620-macos".into()),
            },
        ),
        ("move", Job::Move),
    ]
}

#[test]
fn nothing_held_opens_every_action_and_every_start() {
    let reservation = idle();
    assert_eq!(
        reservation.gate(),
        Gate {
            modify: open(vec![]),
            uninstall: open(vec![]),
            launch: open(vec![]),
            maintain: open(vec![]),
        }
    );
    for (name, job) in jobs().into_iter().chain([("launch", Job::Launch)]) {
        assert!(reservation.starts(&620, &job), "{name}");
    }
    assert_eq!(reservation.moves(), open(vec![]));
}

#[test]
fn a_sign_out_a_diagnostic_maintenance_or_a_recovery_closes_every_action_and_start() {
    let cases = [
        (
            "signed out",
            Reservation {
                signed_out: true,
                ..idle()
            },
        ),
        (
            "diagnostic",
            Reservation {
                diagnostic: true,
                ..idle()
            },
        ),
        (
            "maintenance",
            Reservation {
                maintaining: true,
                ..idle()
            },
        ),
        (
            "recovery",
            Reservation {
                recovering: true,
                ..idle()
            },
        ),
    ];
    for (case, reservation) in cases {
        assert_eq!(
            reservation.gate(),
            Gate {
                modify: Opening::Closed,
                uninstall: Opening::Closed,
                launch: Opening::Closed,
                maintain: Opening::Closed,
            },
            "{case}"
        );
        for (name, job) in jobs() {
            assert!(!reservation.starts(&620, &job), "{case}: {name}");
        }
    }
}

#[test]
fn moves_queue_through_maintenance_and_recovery_but_not_after_a_sign_out_or_a_diagnostic() {
    let signed_out = Reservation {
        signed_out: true,
        ..idle()
    };
    assert_eq!(signed_out.moves(), Opening::Closed);
    let diagnostic = Reservation {
        diagnostic: true,
        ..idle()
    };
    assert_eq!(diagnostic.moves(), Opening::Closed);
    let maintaining = Reservation {
        maintaining: true,
        ..idle()
    };
    assert_eq!(maintaining.moves(), open(vec![]));
    let recovering = Reservation {
        recovering: true,
        ..idle()
    };
    assert_eq!(recovering.moves(), open(vec![]));
    let uninstalling = Reservation {
        uninstalling: Some(620),
        ..idle()
    };
    assert_eq!(uninstalling.moves(), open(vec![620]));
}

#[test]
fn a_game_session_holds_its_games_files_and_every_launch_and_cloud_operation() {
    let reservation = Reservation {
        session: Some(620),
        ..idle()
    };
    assert_eq!(
        reservation.gate(),
        Gate {
            modify: open(vec![620]),
            uninstall: open(vec![620]),
            launch: Opening::Closed,
            maintain: Opening::Closed,
        }
    );
    for (name, job) in jobs() {
        assert!(!reservation.starts(&620, &job), "{name}");
        assert!(reservation.starts(&730, &job), "{name}");
    }
}

#[test]
fn a_transfer_holds_its_game_and_every_uninstall_and_runs_alone() {
    let reservation = Reservation {
        downloading: true,
        transferring: Some(620),
        queued: vec![620],
        ..idle()
    };
    assert_eq!(
        reservation.gate(),
        Gate {
            modify: open(vec![620]),
            uninstall: Opening::Closed,
            launch: open(vec![620]),
            maintain: open(vec![620]),
        }
    );
    for (name, job) in jobs() {
        assert!(!reservation.starts(&730, &job), "{name}");
    }
}

#[test]
fn a_queued_game_queues_once_and_still_launches() {
    let reservation = Reservation {
        queued: vec![730],
        ..idle()
    };
    assert_eq!(
        reservation.gate(),
        Gate {
            modify: open(vec![730]),
            uninstall: open(vec![730]),
            launch: open(vec![]),
            maintain: open(vec![]),
        }
    );
    assert!(reservation.starts(&730, &Job::Move));
}

#[test]
fn an_uninstall_holds_every_start_but_other_games_still_queue() {
    let reservation = Reservation {
        uninstalling: Some(620),
        ..idle()
    };
    assert_eq!(
        reservation.gate(),
        Gate {
            modify: open(vec![620]),
            uninstall: Opening::Closed,
            launch: Opening::Closed,
            maintain: open(vec![620]),
        }
    );
    for (name, job) in jobs() {
        assert!(!reservation.starts(&730, &job), "{name}");
    }
}

#[test]
fn a_launch_still_preparing_holds_uninstalls_launches_and_cloud_operations() {
    let reservation = Reservation {
        launching: Some(620),
        session: Some(620),
        ..idle()
    };
    assert_eq!(
        reservation.gate(),
        Gate {
            modify: open(vec![620]),
            uninstall: Opening::Closed,
            launch: Opening::Closed,
            maintain: Opening::Closed,
        }
    );
    assert!(reservation.starts(&730, &TRANSFER));
}

#[test]
fn a_move_waits_for_its_games_launch_and_a_transfer_does_not() {
    // The launch's Game session already ended, its launch not yet.
    let reservation = Reservation {
        launching: Some(620),
        ..idle()
    };
    assert!(!reservation.starts(&620, &Job::Move));
    assert!(reservation.starts(&620, &TRANSFER));
    assert!(reservation.starts(&730, &Job::Move));
}

#[test]
fn the_excepted_games_keep_the_sessions_then_the_queues_order() {
    let reservation = Reservation {
        session: Some(440),
        queued: vec![730, 620, 730],
        uninstalling: Some(10),
        ..idle()
    };
    let gate = reservation.gate();
    assert_eq!(gate.modify, open(vec![440, 730, 620, 730, 10]));
    assert_eq!(gate.uninstall, Opening::Closed);
    let reservation = Reservation {
        uninstalling: None,
        ..reservation
    };
    assert_eq!(reservation.gate().uninstall, open(vec![440, 730, 620, 730]));
}

#[test]
fn a_job_that_may_start_could_have_been_queued() {
    let games = [None, Some(620), Some(730)];
    let queues: [&[u32]; 4] = [&[], &[620], &[730], &[620, 730]];
    let mut checked = 0;
    for flags in 0..32u32 {
        let flag = |bit: u32| flags & (1 << bit) != 0;
        for session in games {
            for launching in games {
                for uninstalling in games {
                    for transferring in games {
                        for queued in queues {
                            let reservation = Reservation {
                                signed_out: flag(0),
                                diagnostic: flag(1),
                                maintaining: flag(2),
                                recovering: flag(3),
                                downloading: flag(4),
                                session,
                                launching,
                                uninstalling,
                                transferring,
                                queued: queued.to_vec(),
                            };
                            for game in [620, 730] {
                                for (name, job) in [("transfer", TRANSFER), ("move", Job::Move)] {
                                    checked += 1;
                                    if !reservation.starts(&game, &job) {
                                        continue;
                                    }
                                    // Before it was queued, its game was not.
                                    let before = Reservation {
                                        queued: queued
                                            .iter()
                                            .copied()
                                            .filter(|queued| *queued != game)
                                            .collect(),
                                        ..reservation.clone()
                                    };
                                    assert!(
                                        before.gate().modify.allows(&game),
                                        "the {name} of {game} starts in {reservation:?}"
                                    );
                                }
                            }
                        }
                    }
                }
            }
        }
    }
    assert_eq!(checked, 10_368 * 2 * 2);
}
