//! Process ownership shared by Steam and Epic supervisors.
//!
//! A helper's confirmed exec starts the game; only reaping that child ends it.
//! Inspection supplies durable identities, never permission to abandon a child.
use crate::supervisor::Identity;
use std::{io, path::Path, time::Duration};
use tokio::{io::AsyncReadExt, process::Child, time::Instant};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Execution {
    Started,
    Failed,
    ExitedUnconfirmed,
}

pub(crate) async fn wait_for_exit(child: &mut Child) -> i32 {
    loop {
        match child.wait().await {
            Ok(status) => return status.code().unwrap_or(-1),
            // Losing an observation never releases ownership of a live game.
            Err(_) => tokio::time::sleep(Duration::from_millis(100)).await,
        }
    }
}

/// The signed helper owns a CLOEXEC pipe: EOF confirms replacement by the game,
/// while ordinary preparation/exec failures send an explicit bounded marker.
pub(crate) async fn confirm_execution(
    child: &mut Child,
    executable: &Path,
    inspect: impl Fn(u32) -> Option<Identity>,
) -> Execution {
    let pid = child.id();
    let expected = executable.canonicalize().ok();
    let observing = Instant::now();
    let output = child.stdout.take();
    let read = async move {
        let Some(output) = output else {
            return std::future::pending::<io::Result<Vec<u8>>>().await;
        };
        let mut bytes = Vec::new();
        output.take(64).read_to_end(&mut bytes).await?;
        Ok(bytes)
    };
    tokio::pin!(read);
    let mut received = false;
    loop {
        tokio::select! {
            biased;
            result = &mut read, if !received => {
                received = true;
                if let Some(execution) = execution_reply(result) {
                    return execution;
                }
            }
            result = child.wait() => {
                if result.is_ok() {
                    // Reaping and pipe EOF can become ready together. Consume
                    // the final reply before rejecting a very short game.
                    if !received
                        && let Ok(result) = tokio::time::timeout(
                            Duration::from_millis(100), &mut read,
                        ).await
                        && let Some(execution) = execution_reply(result)
                    {
                        return execution;
                    }
                    return Execution::ExitedUnconfirmed;
                }
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
            _ = tokio::time::sleep(Duration::from_millis(25)) => {
                if observing.elapsed() >= Duration::from_secs(3)
                    && pid.and_then(&inspect).is_some_and(|identity| {
                        Some(Path::new(&identity.executable)) == expected.as_deref()
                    })
                {
                    return Execution::Started;
                }
            }
        }
    }
}

fn execution_reply(result: io::Result<Vec<u8>>) -> Option<Execution> {
    match result {
        Ok(bytes) if bytes.is_empty() => Some(Execution::Started),
        Ok(bytes) if bytes == crate::protocol::EXEC_FAILED => Some(Execution::Failed),
        _ => None,
    }
}

/// Never persist an architecture helper as the running game's identity.
pub(crate) async fn wait_identity(
    child: &mut Child,
    executable: &Path,
    inspect: impl Fn(u32) -> Option<Identity>,
) -> Option<Identity> {
    let pid = child.id()?;
    let expected = executable.canonicalize().ok()?;
    let deadline = Instant::now() + Duration::from_secs(1);
    loop {
        if let Some(identity) = inspect(pid)
            && Path::new(&identity.executable) == expected
        {
            return Some(identity);
        }
        tokio::select! {
            result = child.wait() => {
                if result.is_ok() {
                    return None;
                }
            }
            _ = tokio::time::sleep_until(deadline) => return None,
            _ = tokio::time::sleep(Duration::from_millis(5)) => (),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        process::Stdio,
        sync::{
            Arc,
            atomic::{AtomicBool, Ordering},
        },
    };

    fn child(script: &str, output: bool) -> Child {
        tokio::process::Command::new("/bin/sh")
            .args(["-c", script])
            .stdin(Stdio::null())
            .stdout(if output {
                Stdio::piped()
            } else {
                Stdio::null()
            })
            .stderr(Stdio::null())
            .kill_on_drop(false)
            .spawn()
            .unwrap()
    }

    #[tokio::test]
    async fn pipe_confirmation_precedes_actual_exit_and_keeps_runtime_available() {
        let mut game = child("exec 1>/dev/null; sleep 0.1; exit 7", true);
        assert_eq!(
            confirm_execution(&mut game, Path::new("/bin/sh"), |_| None).await,
            Execution::Started
        );
        assert!(game.try_wait().unwrap().is_none());
        let progressed = Arc::new(AtomicBool::new(false));
        let progress = progressed.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(10)).await;
            progress.store(true, Ordering::Release);
        });
        assert_eq!(wait_for_exit(&mut game).await, 7);
        assert!(progressed.load(Ordering::Acquire));
    }

    #[tokio::test]
    async fn explicit_exec_failure_and_missing_confirmation_remain_distinct() {
        let mut failed = child("printf 'exec-failed\\n'; exit 126", true);
        assert_eq!(
            confirm_execution(&mut failed, Path::new("/bin/sh"), |_| None).await,
            Execution::Failed
        );
        assert_eq!(wait_for_exit(&mut failed).await, 126);
        let mut unknown = child("exit 0", false);
        assert_eq!(
            confirm_execution(&mut unknown, Path::new("/bin/sh"), |_| None).await,
            Execution::ExitedUnconfirmed
        );
        assert_eq!(wait_for_exit(&mut unknown).await, 0);
    }

    #[tokio::test]
    async fn unknown_reply_does_not_release_a_live_child() {
        let mut game = child("printf unknown; exec 1>/dev/null; sleep 0.1; exit 4", true);
        let start = Instant::now();
        assert_eq!(
            confirm_execution(&mut game, Path::new("/bin/sh"), |_| None).await,
            Execution::ExitedUnconfirmed
        );
        assert!(start.elapsed() >= Duration::from_millis(80));
        assert_eq!(wait_for_exit(&mut game).await, 4);
    }

    #[tokio::test]
    async fn helper_identity_is_never_published_as_the_game() {
        let mut game = child("sleep 0.1", false);
        let identity = wait_identity(&mut game, Path::new("/bin/sh"), |pid| {
            Some(Identity {
                pid,
                uid: 0,
                birth_seconds: 1,
                birth_microseconds: 0,
                executable: "/not-the-game".into(),
            })
        })
        .await;
        assert!(identity.is_none());
        assert_eq!(wait_for_exit(&mut game).await, 0);
    }
}
