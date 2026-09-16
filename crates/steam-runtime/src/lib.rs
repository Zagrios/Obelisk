//! Safe launcher-side supervision. Native ABI code lives in the separate helper.
mod files;
mod process;
pub mod protocol;
use protocol::{Credentials, Event};
use std::{
    path::PathBuf,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};

pub const CLIENT_SHA256: &str = "035722a42c74989fa38c7efe0d735fdb38444e1102c92246652b5d913d019840";
pub fn sha(path: &std::path::Path) -> std::io::Result<String> {
    use sha2::{Digest, Sha256};
    let mut digest = Sha256::new();
    std::io::copy(&mut std::fs::File::open(path)?, &mut digest)?;
    Ok(format!("{:x}", digest.finalize()))
}

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("backend.steam.runtime.cancelled")]
    Cancelled,
    #[error("backend.steam.runtime.invalidSession")]
    InvalidSession,
    #[error("backend.steam.runtime.unavailable")]
    Unavailable,
    #[error("backend.steam.runtime.versionMismatch")]
    VersionMismatch,
    #[error("backend.steam.runtime.startFailed")]
    StartFailed,
    #[error("backend.steam.runtime.authenticationFailed")]
    Authentication,
    #[error("backend.steam.runtime.ownershipFailed")]
    Ownership,
    #[error("backend.steam.runtime.sdkFailed")]
    Sdk,
    #[error("backend.steam.runtime.timeout")]
    Timeout,
    #[error("backend.steam.runtime.storageFailed")]
    Storage,
}
impl From<std::io::Error> for Error {
    fn from(_: std::io::Error) -> Self {
        Self::Storage
    }
}

pub struct Config {
    pub helpers: PathBuf,
    pub source: PathBuf,
    pub cache: PathBuf,
    pub diagnostics: PathBuf,
    pub steam_api: PathBuf,
    pub home: PathBuf,
    pub app_id: u32,
}

pub struct Launch {
    credentials: Mutex<Option<Credentials>>,
    cancelled: Arc<AtomicBool>,
}
impl Launch {
    pub fn new(credentials: Credentials) -> Self {
        Self {
            credentials: Mutex::new(Some(credentials)),
            cancelled: Arc::new(AtomicBool::new(false)),
        }
    }
    pub fn cancel(&self) {
        self.cancelled.store(true, Ordering::Release);
    }
    pub fn prepare(&self, config: Config) -> Result<Runtime, Error> {
        let mut events = Vec::new();
        let result = self.prepare_inner(&config, &mut events);
        if let Err(error) = &result {
            let _ = files::failure_report(&config, error, &events);
        }
        result
    }
    fn prepare_inner(&self, config: &Config, events: &mut Vec<Event>) -> Result<Runtime, Error> {
        self.check_cancelled()?;
        let credentials = self
            .credentials
            .lock()
            .map_err(|_| Error::InvalidSession)?
            .take()
            .ok_or(Error::InvalidSession)?;
        if !credentials.valid() || config.app_id == 0 {
            return Err(Error::InvalidSession);
        }
        let id = credentials.steam_id;
        let prepared = files::prepare(config)?;
        self.check_cancelled()?;
        let service = format!(
            "com.zagrios.forge.discovery.{}",
            uuid::Uuid::new_v4().simple()
        );
        let mut host = process::Child::host(&prepared, config, &service)?;
        credentials
            .write(host.input.as_mut().ok_or(Error::StartFailed)?)
            .map_err(|_| Error::StartFailed)?;
        drop(credentials);
        let ticket_bytes = self.wait_for_host(&mut host, config.app_id, events)?;
        self.check_cancelled()?;
        let mut checker = process::Child::checker(&prepared, config, &service)?;
        use std::io::Write;
        checker
            .input
            .take()
            .ok_or(Error::StartFailed)?
            .write_all(&id.to_le_bytes())
            .map_err(|_| Error::StartFailed)?;
        let checked_at = Instant::now();
        let asynchronous_request = loop {
            self.check_cancelled()?;
            if checked_at.elapsed() > Duration::from_secs(20) {
                return Err(Error::Timeout);
            }
            if !host.alive()? {
                return Err(Error::StartFailed);
            }
            let event = checker.next(Duration::from_millis(50))?;
            if let Some(event) = &event {
                events.push(event.clone());
            }
            match event {
                Some(Event::SdkVerified {
                    app_id,
                    asynchronous_request,
                }) if app_id == config.app_id => break asynchronous_request,
                Some(Event::SdkStage { .. }) => {}
                Some(_) => return Err(Error::Sdk),
                None => {}
            }
        };
        checker.finish(Duration::from_secs(2))?;
        self.check_cancelled()?;
        if !host.alive()? {
            return Err(Error::StartFailed);
        }
        let proof = Proof {
            app_id: config.app_id,
            host_pid: host.id(),
            authenticated: true,
            ticket_bytes,
            sdk_initialized: true,
            account_verified: true,
            license_verified: true,
            asynchronous_request,
            game_pid: None,
            game_exit: None,
            discovery_paths: 0,
            state: "ready".into(),
        };
        let runtime = Runtime {
            inner: Mutex::new(Inner {
                host,
                proof,
                stopped: false,
            }),
            adapter: prepared
                .directory
                .join("libforge_steam_discovery_adapter.dylib"),
            service,
            report: prepared.report.clone(),
            _prepared: prepared,
        };
        runtime.write_report()?;
        Ok(runtime)
    }
    fn wait_for_host(
        &self,
        host: &mut process::Child,
        expected_app: u32,
        events: &mut Vec<Event>,
    ) -> Result<u32, Error> {
        let start = Instant::now();
        let mut authenticated = false;
        loop {
            self.check_cancelled()?;
            if start.elapsed() > Duration::from_secs(50) {
                return Err(Error::Timeout);
            }
            let event = host.next(Duration::from_millis(50))?;
            if let Some(event) = &event {
                events.push(event.clone());
            }
            match event {
                Some(Event::Authenticated {}) if !authenticated => authenticated = true,
                Some(Event::Ready {
                    app_id,
                    host_pid,
                    ticket_bytes,
                }) if authenticated
                    && app_id == expected_app
                    && host_pid == host.id()
                    && ticket_bytes > 0 =>
                {
                    return Ok(ticket_bytes);
                }
                Some(Event::Failed { reason }) => return Err(failure(reason)),
                Some(_) => return Err(Error::StartFailed),
                None => {}
            }
        }
    }
    fn check_cancelled(&self) -> Result<(), Error> {
        if self.cancelled.load(Ordering::Acquire) {
            Err(Error::Cancelled)
        } else {
            Ok(())
        }
    }
}
fn failure(reason: protocol::Failure) -> Error {
    match reason {
        protocol::Failure::Authentication => Error::Authentication,
        protocol::Failure::Ownership => Error::Ownership,
        protocol::Failure::Sdk => Error::Sdk,
        protocol::Failure::Cancelled => Error::Cancelled,
        _ => Error::StartFailed,
    }
}

#[derive(Clone, Debug, serde::Serialize)]
pub struct Proof {
    pub app_id: u32,
    pub host_pid: u32,
    pub authenticated: bool,
    pub ticket_bytes: u32,
    pub sdk_initialized: bool,
    pub account_verified: bool,
    pub license_verified: bool,
    pub asynchronous_request: bool,
    pub game_pid: Option<u32>,
    pub game_exit: Option<i32>,
    pub discovery_paths: u32,
    pub state: String,
}
struct Inner {
    host: process::Child,
    proof: Proof,
    stopped: bool,
}
pub struct Runtime {
    inner: Mutex<Inner>,
    adapter: PathBuf,
    service: String,
    report: PathBuf,
    _prepared: files::Prepared,
}
impl Runtime {
    pub fn environment(&self) -> std::collections::HashMap<String, String> {
        self._prepared.consumer_environment(&self.service)
    }
    pub fn adapter(&self) -> String {
        self.adapter.to_string_lossy().into_owned()
    }
    pub fn service(&self) -> String {
        self.service.clone()
    }
    pub fn diagnostic_path(&self) -> String {
        self.report.to_string_lossy().into_owned()
    }
    pub fn started(&self, pid: u32) -> Result<(), Error> {
        let mut inner = self.inner.lock().map_err(|_| Error::StartFailed)?;
        if !inner.host.alive()? {
            return Err(Error::StartFailed);
        }
        Self::drain(&mut inner);
        inner.proof.game_pid = Some(pid);
        inner.proof.state = "game_started".into();
        files::report(&self.report, &inner.proof)
    }
    pub fn stopped(&self, exit: Option<i32>) {
        if let Ok(mut inner) = self.inner.lock() {
            if inner.stopped {
                return;
            }
            inner.stopped = true;
            Self::drain(&mut inner);
            inner.host.stop();
            inner.proof.game_exit = exit;
            inner.proof.state = if inner.proof.game_pid.is_some() {
                "game_exited"
            } else {
                "stopped"
            }
            .into();
            let _ = files::report(&self.report, &inner.proof);
        }
    }
    fn drain(inner: &mut Inner) {
        while let Some(event) = inner.host.try_event() {
            match event {
                Event::Discovery { paths } => inner.proof.discovery_paths = paths,
                Event::Disconnected {} => inner.proof.state = "disconnected".into(),
                _ => {}
            }
        }
    }
    fn write_report(&self) -> Result<(), Error> {
        files::report(
            &self.report,
            &self.inner.lock().map_err(|_| Error::StartFailed)?.proof,
        )
    }
}
impl Drop for Runtime {
    fn drop(&mut self) {
        self.stopped(None);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn launch() -> Launch {
        Launch::new(Credentials {
            steam_id: 1,
            account_name: "fixture".into(),
            refresh_token: "fixture-token".into(),
        })
    }
    fn child(script: &str) -> process::Child {
        let mut command = std::process::Command::new("/bin/sh");
        command.arg("-c").arg(script);
        process::Child::spawn(command).unwrap()
    }
    #[test]
    fn readiness_requires_authentication_matching_app_pid_and_nonempty_ticket() {
        for (prefix, app, pid, ticket) in [
            ("", 1030300, "$$", 178),
            (
                "printf '%s\\n' '{\"event\":\"authenticated\"}';",
                999,
                "$$",
                178,
            ),
            (
                "printf '%s\\n' '{\"event\":\"authenticated\"}';",
                1030300,
                "0",
                178,
            ),
            (
                "printf '%s\\n' '{\"event\":\"authenticated\"}';",
                1030300,
                "$$",
                0,
            ),
        ] {
            let mut host = child(&format!(
                "{prefix} printf '{{\"event\":\"ready\",\"app_id\":{app},\"host_pid\":%s,\"ticket_bytes\":{ticket}}}\\n' \"{pid}\"; read line"
            ));
            assert!(matches!(
                launch().wait_for_host(&mut host, 1030300, &mut Vec::new()),
                Err(Error::StartFailed)
            ));
            host.stop();
            assert!(!host.alive().unwrap());
        }
        let mut host = child(
            "printf '%s\\n' '{\"event\":\"authenticated\"}'; printf '{\"event\":\"ready\",\"app_id\":1030300,\"host_pid\":%s,\"ticket_bytes\":178}\\n' \"$$\"; read line",
        );
        assert_eq!(
            launch()
                .wait_for_host(&mut host, 1030300, &mut Vec::new())
                .unwrap(),
            178
        );
        host.stop();
        assert!(!host.alive().unwrap());
    }
    #[test]
    fn cancellation_interrupts_waiting_for_a_live_helper() {
        let launch = Arc::new(launch());
        let mut host = child("read line");
        let cancel = launch.clone();
        let cancellation = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(100));
            cancel.cancel();
        });
        let started = Instant::now();
        assert!(matches!(
            launch.wait_for_host(&mut host, 1030300, &mut Vec::new()),
            Err(Error::Cancelled)
        ));
        host.stop();
        cancellation.join().unwrap();
        assert!(!host.alive().unwrap());
        assert!(started.elapsed() < Duration::from_secs(2));
    }
    #[test]
    fn dead_or_malformed_helper_never_becomes_ready() {
        for script in ["exit 1", "printf '%s\\n' 'native token=fixture'"] {
            let mut host = child(script);
            assert!(matches!(
                launch().wait_for_host(&mut host, 1030300, &mut Vec::new()),
                Err(Error::StartFailed)
            ));
        }
    }
}
