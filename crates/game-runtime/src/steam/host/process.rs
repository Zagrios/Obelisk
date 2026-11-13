use super::{Config, files::Prepared};
use crate::{protocol::Event, steam::Error};
use std::{
    io::{BufRead, BufReader, Read},
    process::{ChildStdin, Command, Stdio},
    sync::{
        Arc, Condvar, Mutex,
        mpsc::{self, Receiver, RecvTimeoutError},
    },
    thread::JoinHandle,
    time::{Duration, Instant},
};

/// Generation-based notifications avoid losing an event between drain and wait.
#[derive(Default)]
pub(crate) struct Wake {
    generation: Mutex<u64>,
    changed: Condvar,
}
impl Wake {
    pub fn generation(&self) -> u64 {
        *self.generation.lock().unwrap()
    }
    pub fn notify(&self) {
        *self.generation.lock().unwrap() += 1;
        self.changed.notify_all();
    }
    pub fn wait(&self, previous: u64) {
        drop(
            self.changed
                .wait_while(self.generation.lock().unwrap(), |g| *g == previous)
                .unwrap(),
        );
    }
}

pub(crate) struct Child {
    child: std::process::Child,
    pub input: Option<ChildStdin>,
    events: Receiver<Event>,
    reader: Option<JoinHandle<()>>,
    reaped: bool,
    pub wakeup: Arc<Wake>,
}
impl Child {
    pub fn host(prepared: &Prepared, config: &Config, service: &str) -> Result<Self, Error> {
        Self::spawn(Self::host_command(prepared, config, service))
    }
    pub(crate) fn host_command(prepared: &Prepared, config: &Config, service: &str) -> Command {
        let mut command = Command::new("/usr/bin/sandbox-exec");
        command
            .arg("-f")
            .arg(&prepared.profile)
            .arg(prepared.directory.join(crate::package::HOST))
            .arg("engine")
            .arg(prepared.directory.join("steamclient.dylib"))
            .arg(config.app_id.to_string())
            .arg(service)
            // This host has no Steam friends UI to apply persona changes.
            .arg("-nofriendsui")
            .current_dir(&prepared.directory)
            .env_clear()
            .env("PATH", "/usr/bin:/bin")
            .env("HOME", prepared.home())
            .env("TMPDIR", prepared.run_directory.join("tmp"));
        // Host and game must connect to the same private IPC.
        command.envs(prepared.endpoints.iter().map(|(key, value)| (key, value)));
        // The host checks the SDK projection under the game's platform.
        if let Some(storage) = &prepared.storage {
            command.env("OBELISK_STORAGE_METADATA", storage.platforms.to_string());
        }
        command
    }
    pub(crate) fn spawn(mut command: Command) -> Result<Self, Error> {
        let mut child = command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|_| Error::StartFailed)?;
        let input = child.stdin.take();
        let output = child.stdout.take().ok_or(Error::StartFailed)?;
        let (sender, events) = mpsc::sync_channel(64);
        let wakeup = Arc::new(Wake::default());
        let changed = wakeup.clone();
        let reader = std::thread::spawn(move || {
            let mut reader = BufReader::new(output);
            loop {
                let mut bytes = Vec::new();
                match (&mut reader).take(4097).read_until(b'\n', &mut bytes) {
                    Ok(0) | Err(_) => break,
                    Ok(length) if length > 4096 => break,
                    Ok(_) => {}
                }
                let Ok(event) = serde_json::from_slice(&bytes) else {
                    break;
                };
                // Do not block process teardown when the game sends many lookups.
                if sender.try_send(event).is_err() {
                    break;
                }
                changed.notify();
            }
            // Disconnect first, then notify: observers must see EOF on wakeup.
            drop(sender);
            changed.notify();
        });
        Ok(Self {
            child,
            input,
            events,
            reader: Some(reader),
            reaped: false,
            wakeup,
        })
    }
    pub fn id(&self) -> u32 {
        self.child.id()
    }
    pub fn alive(&mut self) -> Result<bool, Error> {
        if self.reaped {
            return Ok(false);
        }
        if self
            .child
            .try_wait()
            .map_err(|_| Error::StartFailed)?
            .is_some()
        {
            self.reaped = true;
            return Ok(false);
        }
        Ok(true)
    }
    pub fn next(&mut self, timeout: Duration) -> Result<Option<Event>, Error> {
        match self.events.recv_timeout(timeout) {
            Ok(event) => Ok(Some(event)),
            Err(RecvTimeoutError::Timeout) if self.alive()? => Ok(None),
            Err(_) => Err(Error::StartFailed),
        }
    }
    pub fn try_event(&self) -> Option<Event> {
        self.events.try_recv().ok()
    }
    pub fn stop(&mut self) {
        self.input.take();
        let start = Instant::now();
        while !self.reaped && start.elapsed() < Duration::from_millis(700) {
            if !self.alive().unwrap_or(false) {
                break;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        if !self.reaped {
            let _ = self.child.kill();
            let _ = self.child.wait();
            self.reaped = true;
        }
        if let Some(reader) = self.reader.take() {
            let _ = reader.join();
        }
    }
}
impl Drop for Child {
    fn drop(&mut self) {
        self.stop();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn host_and_game_share_the_private_ipc_endpoints_and_discovery() {
        let temporary = tempfile::tempdir().unwrap();
        let prepared = Prepared {
            storage: None,
            licenses: None,
            directory: temporary.path().join("engine"),
            run_directory: temporary.path().join("run"),
            profile: temporary.path().join("profile"),
            report: temporary.path().join("report"),
            endpoints: [
                ("Steam3Master".into(), "127.0.0.1:31001".into()),
                ("SteamClientService".into(), "127.0.0.1:31002".into()),
            ],
        };
        let config = Config {
            cloud_save_roots: vec![],
            helpers: temporary.path().into(),
            package: temporary.path().join("runtime.zip"),
            cache: temporary.path().into(),
            diagnostics: temporary.path().into(),
            steam_api: Some(temporary.path().join("libsteam_api.dylib")),
            wine: None,
            home: temporary.path().into(),
            app_id: 1030300,
        };
        let service = "com.zagrios.obelisk.discovery.fixture";
        let host = Child::host_command(&prepared, &config, service);
        assert_eq!(
            host.get_args().last(),
            Some(std::ffi::OsStr::new("-nofriendsui"))
        );
        let game = prepared.consumer_environment(service);
        for key in ["Steam3Master", "SteamClientService"] {
            let expected = std::ffi::OsStr::new(&game[key]);
            assert_eq!(
                host.get_envs()
                    .find(|(name, _)| *name == key)
                    .and_then(|(_, value)| value),
                Some(expected)
            );
        }
        assert_eq!(game["OBELISK_PRIVATE_STEAM_DISCOVERY"], service);
        assert_eq!(
            game["DYLD_INSERT_LIBRARIES"],
            prepared
                .directory
                .join(crate::package::ADAPTER)
                .to_string_lossy()
        );
    }
}
