//! Whether a Game session still owns a process. The recorded process is the
//! one whose PID, UID and kernel birth time still match: exec can replace a
//! game's executable without ending it, so the executable never decides.
use super::{Identity, StartingGame};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Liveness {
    Dead,
    Owned,
    /// The record claims a running game it cannot name; keep its Reservation.
    Unknown,
}

pub(crate) fn same_process(recorded: &Identity, now: &Identity) -> bool {
    now.pid == recorded.pid
        && now.uid == recorded.uid
        && now.birth_seconds == recorded.birth_seconds
        && now.birth_microseconds == recorded.birth_microseconds
}

/// `inspect` proves absence with `Ok(None)`; a failed inspection is an error,
/// and uncertainty never releases anything.
pub(crate) fn alive<E>(
    recorded: &Identity,
    inspect: impl Fn(u32) -> Result<Option<Identity>, E>,
) -> Result<bool, E> {
    Ok(inspect(recorded.pid)?.is_some_and(|now| same_process(recorded, &now)))
}

/// The game, or the helper that becomes it, is still alive. `running` is the
/// recorded phase, before any terminal state.
pub(crate) fn owns_process<E>(
    running: bool,
    game: Option<&Identity>,
    starting: Option<&StartingGame>,
    inspect: impl Fn(u32) -> Result<Option<Identity>, E>,
) -> Result<Liveness, E> {
    if let Some(game) = game
        && alive(game, &inspect)?
    {
        return Ok(Liveness::Owned);
    }
    match starting {
        Some(StartingGame::Process { identity, .. }) if alive(identity, &inspect)? => {
            Ok(Liveness::Owned)
        }
        None if running && game.is_none() => Ok(Liveness::Unknown),
        _ => Ok(Liveness::Dead),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn process(pid: u32) -> Identity {
        Identity {
            pid,
            uid: 501,
            birth_seconds: 1_700_000_000,
            birth_microseconds: 250,
            executable: "/fixture/launcher".into(),
        }
    }

    fn table(live: Vec<Identity>) -> impl Fn(u32) -> Result<Option<Identity>, ()> {
        move |pid| Ok(live.iter().find(|identity| identity.pid == pid).cloned())
    }

    #[test]
    fn an_exec_replacement_is_still_the_recorded_process_but_a_reused_pid_is_not() {
        let recorded = process(42);
        let mut replaced = recorded.clone();
        replaced.executable = "/fixture/second-binary".into();
        assert!(alive(&recorded, table(vec![replaced])).unwrap());

        for change in 0..3 {
            let mut reused = recorded.clone();
            match change {
                0 => reused.birth_seconds += 1,
                1 => reused.birth_microseconds += 1,
                _ => reused.uid += 1,
            }
            assert!(
                !alive(&recorded, table(vec![reused])).unwrap(),
                "change {change}"
            );
        }
        assert!(!alive(&recorded, table(vec![])).unwrap());
        assert!(alive(&recorded, |_| Err::<Option<Identity>, _>("uncertain")).is_err());
    }

    #[test]
    fn a_game_or_its_starting_helper_keeps_the_session_owned() {
        let game = process(42);
        let starting = StartingGame::Process {
            identity: process(43),
            executable: "/fixture/game".into(),
        };
        let owned = |game, starting, live| owns_process(true, game, starting, table(live));

        assert_eq!(
            owned(Some(&game), None, vec![game.clone()]),
            Ok(Liveness::Owned)
        );
        let mut exec = process(43);
        exec.executable = "/fixture/any-binary".into();
        assert_eq!(
            owned(None, Some(&starting), vec![exec]),
            Ok(Liveness::Owned)
        );
        assert_eq!(
            owned(Some(&game), Some(&starting), vec![]),
            Ok(Liveness::Dead)
        );
        assert_eq!(owned(None, None, vec![]), Ok(Liveness::Unknown));
        assert_eq!(
            owns_process(false, None, None, table(vec![])),
            Ok(Liveness::Dead)
        );
        assert!(
            owns_process(true, Some(&game), None, |_| Err::<Option<Identity>, _>(
                "uncertain"
            ))
            .is_err()
        );
    }
}
