//! Debug-only authenticated persistence proof. No native transfer controls,
//! FileShare, game launch or synthetic identity/quota are used.
use crate::{Config, Error, Launch, files, process, protocol::Event};
use std::{
    fs,
    io::Write,
    time::{Duration, Instant},
};

impl Launch {
    pub fn probe_storage(&self, config: Config, transfers: bool) -> Result<String, Error> {
        let credentials = self
            .credentials
            .lock()
            .map_err(|_| Error::InvalidSession)?
            .take()
            .ok_or(Error::InvalidSession)?;
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()?;
        let mut cm = runtime.block_on(async {
            let http = steam_kit::client::http_client().map_err(|_| Error::StartFailed)?;
            let mut cm = steam_kit::cm::SteamConnection::connect(http)
                .await
                .map_err(|_| Error::Authentication)?;
            cm.log_on(
                &credentials.account_name,
                credentials.steam_id,
                &credentials.refresh_token,
            )
            .await
            .map_err(|_| Error::Authentication)?;
            Ok::<_, Error>(cm)
        })?;
        let before = runtime.block_on(async {
            let info = cm
                .app_info(config.app_id)
                .await
                .map_err(|_| Error::Unavailable)?;
            let metadata = info.cloud.ok_or(Error::Unavailable)?;
            if metadata.field("savefiles").is_some()
                || metadata
                    .field("appidredirect")
                    .is_some_and(|v| v.text() != Some("0"))
            {
                return Err(Error::Unavailable);
            }
            let quota = cm
                .cloud_quota(config.app_id)
                .await
                .map_err(|_| Error::Unavailable)?;
            if quota.existing_files != Some(0)
                || quota.existing_bytes != Some(0)
                || !quota.max_bytes.is_some_and(|n| n >= 1024)
            {
                return Err(Error::Unavailable);
            }
            let listing = cm
                .cloud_listing(config.app_id)
                .await
                .map_err(|_| Error::Unavailable)?;
            if listing
                .files
                .iter()
                .any(|file| file.persist_state == 0 || file.name != "forge-sdk-persistence.dat")
            {
                return Err(Error::Unavailable);
            }
            Ok(listing)
        })?;
        let mut prepared = files::prepare(&config, &self.cancelled)?;
        let proof_root = config
            .cache
            .join(format!("storage-proof-{}", uuid::Uuid::new_v4()));
        let mut home = prepared.run_directory.join("home");
        let steam = home.join("Library/Application Support/Steam");
        let mut cache = steam.join(format!(
            "userdata/{}/{}",
            credentials.steam_id as u32, config.app_id
        ));
        files::private_directory(&cache.join("remote"))?;
        files::private_directory(&steam.join("appcache"))?;
        // Only app metadata and this verified empty app's cache are writable.
        // No login configuration, another account or another app can be written.
        let extra = format!(
            "\n(allow file-read* (subpath {}))\n(allow file-write* (subpath {}) (subpath {}))\n",
            serde_json::to_string(&home).map_err(|_| Error::Storage)?,
            serde_json::to_string(&cache).map_err(|_| Error::Storage)?,
            serde_json::to_string(&steam.join("appcache")).map_err(|_| Error::Storage)?
        );
        fs::OpenOptions::new()
            .append(true)
            .open(&prepared.profile)?
            .write_all(extra.as_bytes())?;
        let mut events = Vec::new();
        let mut synchronized = 0;
        let mut rust_listing = before.clone();
        let mut native_remote_unchanged = Vec::new();
        let result = (|| -> Result<(), Error> {
            for readback in [false, true] {
                self.check_cancelled()?;
                let mut cloud = if transfers {
                    files::private_directory(&proof_root)?;
                    let cloud_config = crate::cloud::Config {
                        root: proof_root.join(if readback { "receiver" } else { "sender" }),
                        home: config.home.clone(),
                        install: config
                            .steam_api
                            .parent()
                            .ok_or(Error::Storage)?
                            .to_path_buf(),
                        app_id: config.app_id,
                    };
                    let control = std::sync::Arc::new(crate::cloud::Control::new(
                        credentials.steam_id,
                        |_| {},
                    ));
                    let mut session = crate::cloud::Session::new(
                        cloud_config,
                        credentials.clone(),
                        uuid::Uuid::new_v4().to_string(),
                        control,
                    )?;
                    session.probe_prepare()?;
                    let storage = session.native_storage()?.ok_or(Error::Unavailable)?;
                    prepared.enable_storage(&storage)?;
                    home = prepared.home();
                    cache = home.join(format!(
                        "Library/Application Support/Steam/userdata/{}/{}",
                        credentials.steam_id as u32, config.app_id
                    ));
                    session.started()?;
                    Some(session)
                } else {
                    None
                };
                let service = format!(
                    "com.zagrios.forge.discovery.{}",
                    uuid::Uuid::new_v4().simple()
                );
                let mut command = process::Child::host_command(&prepared, &config, &service);
                command.env("FORGE_STORAGE_METADATA", "1");
                let mut host = process::Child::spawn(command)?;
                credentials.write(host.input.as_mut().ok_or(Error::StartFailed)?)?;
                self.wait_for_host(&mut host, config.app_id, &mut events)?;
                let mut command = std::process::Command::new(
                    prepared.directory.join(prepared.architecture.helper()),
                );
                command
                    .arg("check-storage")
                    .arg(&config.steam_api)
                    .arg(prepared.directory.join("steamclient.dylib"))
                    .arg(config.app_id.to_string())
                    .arg(if readback { "read" } else { "write" })
                    .env_clear()
                    .env("PATH", "/usr/bin:/bin")
                    .env("HOME", &home)
                    .env("TMPDIR", prepared.run_directory.join("tmp"))
                    .env("SteamAppId", config.app_id.to_string())
                    .env("SteamGameId", config.app_id.to_string())
                    .env("FORGE_STORAGE_DIRECTORY", &cache)
                    .envs(prepared.consumer_environment(&service))
                    .current_dir(&prepared.directory);
                let mut consumer = process::Child::spawn(command)?;
                consumer
                    .input
                    .take()
                    .ok_or(Error::StartFailed)?
                    .write_all(&credentials.steam_id.to_le_bytes())?;
                let started = Instant::now();
                loop {
                    self.check_cancelled()?;
                    if started.elapsed() > Duration::from_secs(30) {
                        return Err(Error::Timeout);
                    }
                    let event = consumer.next(Duration::from_millis(50))?;
                    if let Some(event) = &event {
                        events.push(event.clone());
                    }
                    match event {
                        Some(Event::StorageVerified {
                            total,
                            readback: observed,
                            ..
                        }) if total > 0 && observed == readback => break,
                        Some(_) => return Err(Error::Sdk),
                        None => (),
                    }
                }
                consumer.finish(Duration::from_secs(2))?;
                let started = Instant::now();
                loop {
                    let persisted =
                        fs::read_to_string(cache.join("remotecache.vdf")).unwrap_or_default();
                    if persisted.contains("forge-sdk-persistence.dat") {
                        break;
                    }
                    if started.elapsed() > Duration::from_secs(5) {
                        return Err(Error::Storage);
                    }
                    std::thread::sleep(Duration::from_millis(50));
                }
                host.stop();
                if host.alive()? {
                    return Err(Error::StartFailed);
                }
                if let Some(cloud) = &mut cloud {
                    let observed = runtime
                        .block_on(cm.cloud_listing(config.app_id))
                        .map_err(|_| Error::Unavailable)?;
                    native_remote_unchanged.push(observed == rust_listing);
                    if observed != rust_listing {
                        return Err(Error::Unavailable);
                    }
                    cloud.probe_finish()?;
                    rust_listing = runtime
                        .block_on(cm.cloud_listing(config.app_id))
                        .map_err(|_| Error::Unavailable)?;
                    let active: Vec<_> = rust_listing
                        .files
                        .iter()
                        .filter(|f| f.persist_state == 0)
                        .collect();
                    if readback {
                        if !active.is_empty() {
                            return Err(Error::Storage);
                        }
                    } else if active.len() != 1 || active[0].name != "forge-sdk-persistence.dat" {
                        return Err(Error::Storage);
                    }
                    synchronized += 1;
                } else if !readback {
                    let paths = steam_kit::cloud::SdkCloudPaths::new(cache.clone(), config.app_id)
                        .map_err(|_| Error::Storage)?;
                    let files = paths.local_files(&[]).map_err(|_| Error::Storage)?;
                    if files.len() != 1 {
                        return Err(Error::Storage);
                    }
                    paths.prepare_index(&files).map_err(|_| Error::Storage)?;
                }
            }
            Ok(())
        })();
        // The closure drops/reaps every native process before observing Cloud.
        let after = runtime
            .block_on(cm.cloud_listing(config.app_id))
            .map_err(|_| Error::Unavailable)?;
        let unchanged = before == after;
        let cleaned = after
            .files
            .iter()
            .all(|f| f.persist_state != 0 && f.name == "forge-sdk-persistence.dat");
        serde_json::to_string_pretty(&serde_json::json!({
            "operation":if transfers { "authenticated_sdk_cloud_roundtrip" } else { "authenticated_sdk_persistence" }, "app_id":config.app_id,
            "status":if result.is_ok() && (if transfers { synchronized == 2 && cleaned } else { unchanged }) { "verified" } else { "failed" },
            "failure":result.err().map(|e|e.to_string()), "remote_unchanged":unchanged,
            "rust_sync_completed":synchronized, "remote_active_files":after.files.iter().filter(|f| f.persist_state == 0).count(),
            "native_remote_unchanged":native_remote_unchanged,
            "private_proof_directory":if transfers { Some(proof_root) } else { None },
            "events":events, "game_launched":false, "native_sync_requested":false
        }))
        .map_err(|_| Error::Storage)
    }
}
