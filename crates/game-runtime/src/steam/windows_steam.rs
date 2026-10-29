//! A Windows Steam game that Valve's Windows Steam starts, as its Launch
//! settings may ask: Steam installs itself in a prefix of its own, the
//! installation joins its library as installed, and Steam starts the game
//! there, hidden. It is the one client whose answer to a Windows game's
//! module signature check Valve's servers accept, which a VAC-secured server
//! asks for: the Mac steamclient checks only Mac programs. Steam signs in
//! with what it keeps in the prefix, and keeps the game's Cloud saves itself.
use crate::{launch::Error, supervisor::Target, wine};
use obelisk_core::launch::LaunchTarget;
use std::{
    fmt::Write as _,
    fs,
    io::Write as _,
    path::Path,
    process::Stdio,
    time::{Duration, Instant},
};

/// Where Steam installs itself in a prefix, and keeps its default library.
const STEAM: &str = "drive_c/Program Files (x86)/Steam";
/// Its program there, as Wine reads it.
const STEAM_EXE: &str = r"C:\Program Files (x86)\Steam\steam.exe";
/// Valve's installer of Windows Steam, which installs it silently with `/S`.
const INSTALLER_URL: &str = "https://cdn.akamai.steamstatic.com/client/installer/SteamSetup.exe";
/// Where the prefix keeps the installer until Steam is installed.
const INSTALLER: &str = "obelisk-steam-setup.exe";
/// The largest installer read: it is about 2.5 MB.
const MAX_INSTALLER_BYTES: u64 = 32 * 1024 * 1024;
/// The prerequisite that runs the installer, once per prefix.
const INSTALL_STEAM: &str = "obelisk-windows-steam";
/// Steamworks Common Redistributables, whose depots, such as DirectX's, a
/// game's installation shares with every other game.
const COMMON_REDISTRIBUTABLES: u32 = 228_980;
/// Its depots, which follow its app ID.
const COMMON_REDISTRIBUTABLE_DEPOTS: std::ops::RangeInclusive<u32> = 228_981..=229_100;
/// What starts Steam and lasts as long as the game.
const LAUNCHER: &str = include_str!("windows_steam.sh");

/// What Windows Steam adds to the prefix of the installation of `app_id` in
/// `install`, whose prefix is `prefix`.
pub(super) struct Prefix<'a> {
    pub install: &'a Path,
    pub app_id: u32,
    pub prefix: &'a Path,
}

impl wine::Storefront for Prefix<'_> {
    /// Steam's installer, until Steam is installed, then the steps of the
    /// installation's install scripts, which Steam would run on its first
    /// launch.
    fn prerequisites(&self) -> Result<Vec<wine::Prerequisite>, Error> {
        let mut prerequisites = Vec::new();
        if !installed(self.prefix) {
            prerequisites.push(wine::Prerequisite {
                id: INSTALL_STEAM.to_owned(),
                programs: vec![(self.prefix.join(INSTALLER), "/S".to_owned())],
            });
        }
        prerequisites.extend(super::windows::install_scripts(self.install)?);
        Ok(prerequisites)
    }

    /// Downloads Steam's installer while Steam is missing, then makes the
    /// installation one of Steam's.
    fn furnish(&self, config: &wine::Config) -> Result<(), Error> {
        if !installed(&config.prefix) {
            let installer = config.prefix.join(INSTALLER);
            if !installer.is_file() {
                download_installer(&installer)?;
            }
        } else {
            let _ = fs::remove_file(config.prefix.join(INSTALLER));
        }
        link(&config.prefix, self.install, self.app_id)
    }
}

/// Whether Steam is installed in `prefix`.
fn installed(prefix: &Path) -> bool {
    prefix.join(STEAM).join("steam.exe").is_file()
}

/// Downloads Steam's installer to `path`, from Valve's CDN as Obelisk's Steam
/// components are. Valve replaces it with each release, so no checksum pins
/// it: it must be a Windows program, of a sane size.
fn download_installer(path: &Path) -> Result<(), Error> {
    let runtime = tokio::runtime::Handle::try_current().map_err(|_| Error::StartFailed)?;
    let bytes = runtime.block_on(async {
        let response = reqwest::Client::new()
            .get(INSTALLER_URL)
            .send()
            .await
            .and_then(reqwest::Response::error_for_status)
            .map_err(|_| Error::StartFailed)?;
        if response
            .content_length()
            .is_some_and(|length| length > MAX_INSTALLER_BYTES)
        {
            return Err(Error::StartFailed);
        }
        response.bytes().await.map_err(|_| Error::StartFailed)
    })?;
    if bytes.len() as u64 > MAX_INSTALLER_BYTES || !bytes.starts_with(b"MZ") {
        return Err(Error::StartFailed);
    }
    let directory = path.parent().ok_or(Error::Storage)?;
    let mut file = tempfile::NamedTempFile::new_in(directory)?;
    file.write_all(&bytes)?;
    file.persist(path).map_err(|_| Error::Storage)?;
    Ok(())
}

/// Makes the installation of `app_id` in `install` one of Steam's in
/// `prefix`: a link to it in Steam's default library, named after its
/// folder, and an app manifest naming the depots Obelisk installed. An app
/// manifest Steam already keeps stays, with what Steam learned since.
fn link(prefix: &Path, install: &Path, app_id: u32) -> Result<(), Error> {
    let apps = prefix.join(STEAM).join("steamapps");
    let common = apps.join("common");
    fs::create_dir_all(&common)?;
    let folder = install.file_name().ok_or(Error::NotExecutable)?;
    let link = common.join(folder);
    match fs::read_link(&link) {
        Ok(target) if target == install => {}
        // The games folder moved: the link follows it.
        Ok(_) => {
            fs::remove_file(&link)?;
            std::os::unix::fs::symlink(install, &link)?;
        }
        Err(_) if fs::symlink_metadata(&link).is_ok() => return Err(Error::Storage),
        Err(_) => std::os::unix::fs::symlink(install, &link)?,
    }
    let manifest = apps.join(format!("appmanifest_{app_id}.acf"));
    if manifest.is_file() {
        return Ok(());
    }
    let installed =
        obelisk_core::installation::read_receipt::<steam_kit::client::Installation>(install)
            .map_err(|_| Error::Storage)?
            .filter(|receipt| receipt.app_id == app_id)
            .ok_or(Error::Storage)?;
    let contents = app_manifest(&installed, &folder.to_string_lossy());
    let mut file = tempfile::NamedTempFile::new_in(&apps)?;
    file.write_all(contents.as_bytes())?;
    file.persist(&manifest).map_err(|_| Error::Storage)?;
    Ok(())
}

/// Steam's app manifest of `installed`, whose folder in its library is
/// `folder`: fully installed, with the depots Obelisk installed. It names no
/// build: Steam finds the depots up to date and records the build itself, or
/// downloads what Obelisk left out.
fn app_manifest(installed: &steam_kit::client::Installation, folder: &str) -> String {
    let (shared, own): (Vec<_>, Vec<_>) = installed
        .manifests
        .iter()
        .partition(|depot| COMMON_REDISTRIBUTABLE_DEPOTS.contains(&depot.depot_id));
    let mut manifest = String::new();
    let _ = write!(
        manifest,
        "\"AppState\"\n{{\n\t\"appid\"\t\t\"{app}\"\n\t\"universe\"\t\t\"1\"\n\t\"name\"\t\t\"{name}\"\n\
         \t\"StateFlags\"\t\t\"4\"\n\t\"installdir\"\t\t\"{folder}\"\n\t\"SizeOnDisk\"\t\t\"{size}\"\n\
         \t\"buildid\"\t\t\"0\"\n\t\"InstalledDepots\"\n\t{{\n",
        app = installed.app_id,
        name = vdf_escape(&installed.name),
        folder = vdf_escape(folder),
        size = installed.total_bytes,
    );
    for depot in own {
        let _ = write!(
            manifest,
            "\t\t\"{}\"\n\t\t{{\n\t\t\t\"manifest\"\t\t\"{}\"\n\t\t}}\n",
            depot.depot_id, depot.manifest_id
        );
    }
    manifest.push_str("\t}\n");
    if !shared.is_empty() {
        manifest.push_str("\t\"SharedDepots\"\n\t{\n");
        for depot in shared {
            let _ = writeln!(
                manifest,
                "\t\t\"{}\"\t\t\"{COMMON_REDISTRIBUTABLES}\"",
                depot.depot_id
            );
        }
        manifest.push_str("\t}\n");
    }
    let _ = write!(
        manifest,
        "\t\"UserConfig\"\n\t{{\n\t\t\"language\"\t\t\"{language}\"\n\t}}\n\
         \t\"MountedConfig\"\n\t{{\n\t\t\"language\"\t\t\"{language}\"\n\t}}\n}}\n",
        language = vdf_escape(&installed.language),
    );
    manifest
}

/// `text` as a quoted value of Valve's KeyValues text.
fn vdf_escape(text: &str) -> String {
    text.replace('\\', "\\\\").replace('"', "\\\"")
}

/// The Windows game `target` names, which Steam starts, hidden, with the
/// arguments its profile and its player add: Steam adds its own launch
/// options, so the storefront's are left out.
pub(super) fn target(
    config: &wine::Config,
    game: &wine::Game<'_>,
    target: &LaunchTarget,
    app_id: u32,
    home: &Path,
) -> Result<Target, Error> {
    let settings = &config.settings;
    let after: Vec<String> = settings
        .profile_arguments
        .iter()
        .chain(&settings.arguments)
        .cloned()
        .collect();
    wine::launcher_target(
        config,
        game,
        target,
        LAUNCHER,
        "obelisk-windows-steam",
        vec![STEAM_EXE.to_owned(), app_id.to_string()],
        after,
        crate::native::environment(home),
    )
}

/// Where Steam keeps the accounts it signs in to, in its folder.
const LOGIN_USERS: &str = "config/loginusers.vdf";
/// Where Steam keeps their login tokens, encrypted for the Windows user
/// CrossOver's Wine runs as.
const LOGIN_TOKENS: &str = "drive_c/users/crossover/AppData/Local/Steam/local.vdf";
/// Steam's DevTools, on the Mac's loopback while `-cef-enable-debugging`
/// asks for them.
const DEVTOOLS: &str = "http://127.0.0.1:8080";
/// The page of Steam's own interface whose script holds `SteamClient`.
const SHARED_CONTEXT: &str = "SharedJSContext";
/// Where Steam's interface pages load from.
const STEAM_INTERFACE: &str = "https://steamloopback.host/";
/// Steam's first start in a prefix updates it, which can take minutes under
/// Rosetta.
const START_TIMEOUT: Duration = Duration::from_secs(15 * 60);
/// How long Steam gets to keep the login it signed in with.
const KEEP_TIMEOUT: Duration = Duration::from_secs(120);
/// `EResult::OK`, as `SetLoginToken` answers it.
const SIGNED_IN: i64 = 1;

/// The account Valve's Windows Steam in `prefix` signs in to by itself: one
/// whose login it remembers, with a login token kept. The account Steam last
/// signed in to comes first.
pub(crate) fn account(prefix: &Path) -> Option<String> {
    if !prefix.join(LOGIN_TOKENS).is_file() {
        return None;
    }
    let users = fs::read_to_string(prefix.join(STEAM).join(LOGIN_USERS)).ok()?;
    remembered_account(&users)
}

/// The account `users`, Steam's loginusers.vdf, remembers the login of,
/// the most recent first.
fn remembered_account(users: &str) -> Option<String> {
    let mut accounts = Vec::new();
    let (mut name, mut remembered, mut recent) = (None, false, false);
    for line in users.lines() {
        let fields: Vec<&str> = line.split('"').skip(1).step_by(2).collect();
        match fields.as_slice() {
            ["AccountName", value] => name = Some((*value).to_owned()),
            ["RememberPassword", value] => remembered = *value == "1",
            ["MostRecent", value] => recent = *value == "1",
            _ if line.trim() == "}" => {
                if let (Some(account), true) = (name.take(), remembered) {
                    accounts.push((recent, account));
                }
                (remembered, recent) = (false, false);
            }
            _ => {}
        }
    }
    accounts.sort_by_key(|(recent, _)| !recent);
    accounts.into_iter().next().map(|(_, account)| account)
}

/// Signs Valve's Windows Steam in config's readied prefix in to
/// `account_name`'s account with `refresh_token`, a SteamClient login token
/// Obelisk signed in for it, as Steam's own sign-in window would: Steam
/// starts hidden with its DevTools on the loopback, its interface's script
/// takes the token, and Steam keeps the login once signed in, encrypted its
/// own way. Steam's window cannot draw in Voile's Wine, so it never shows.
/// The token reaches no page but Steam's own interface, and no log.
pub(super) async fn sign_in(
    config: &wine::Config,
    home: &Path,
    account_name: &str,
    refresh_token: &str,
) -> Result<(), super::Error> {
    let mut steam = wine::command(config, home)?
        .args([
            STEAM_EXE,
            "-silent",
            "-nofriendsui",
            "-nochatui",
            "-cef-disable-gpu",
            "-cef-disable-gpu-compositing",
            "-cef-enable-debugging",
        ])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()?;
    let signed_in = async {
        let shared = shared_context(START_TIMEOUT).await?;
        set_login_token(&shared, account_name, refresh_token).await?;
        // Steam writes the account and its token once its logon succeeds.
        let deadline = Instant::now() + KEEP_TIMEOUT;
        while account(&config.prefix).as_deref() != Some(account_name) {
            if Instant::now() >= deadline {
                return Err(super::Error::WindowsSteamSignIn);
            }
            tokio::time::sleep(Duration::from_secs(1)).await;
        }
        Ok(())
    }
    .await;
    shut_down(config, home).await;
    let _ = steam.kill();
    let _ = steam.wait();
    wine::stop(config);
    signed_in
}

/// The DevTools address of Steam's interface script once Steam serves it,
/// within `timeout`.
async fn shared_context(timeout: Duration) -> Result<String, super::Error> {
    #[derive(serde::Deserialize)]
    struct Page {
        title: String,
        url: String,
        #[serde(rename = "webSocketDebuggerUrl")]
        socket: Option<String>,
    }
    let client = reqwest::Client::new();
    let deadline = Instant::now() + timeout;
    loop {
        let pages = async {
            client
                .get(format!("{DEVTOOLS}/json"))
                .timeout(Duration::from_secs(5))
                .send()
                .await?
                .json::<Vec<Page>>()
                .await
        }
        .await
        .unwrap_or_default();
        // Only Steam's own page on the loopback ever receives the token.
        let found = pages.into_iter().find_map(|page| {
            (page.title == SHARED_CONTEXT && page.url.starts_with(STEAM_INTERFACE))
                .then_some(page.socket?)
                .filter(|socket| socket.starts_with("ws://127.0.0.1:8080/"))
        });
        if let Some(socket) = found {
            return Ok(socket);
        }
        if Instant::now() >= deadline {
            return Err(super::Error::WindowsSteamSignIn);
        }
        tokio::time::sleep(Duration::from_secs(2)).await;
    }
}

/// Hands Steam's interface script `refresh_token`, as Steam's sign-in page
/// does once its own sign-in succeeds, and waits for Steam's answer.
async fn set_login_token(
    socket: &str,
    account_name: &str,
    refresh_token: &str,
) -> Result<(), super::Error> {
    use futures_util::{SinkExt, StreamExt};
    use tokio_tungstenite::tungstenite::Message;
    let expression = format!(
        "SteamClient.Auth.SetLoginToken({}, {}).then(answer => ({{ result: answer.result }}))",
        serde_json::to_string(refresh_token).map_err(|_| super::Error::WindowsSteamSignIn)?,
        serde_json::to_string(account_name).map_err(|_| super::Error::WindowsSteamSignIn)?,
    );
    let request = serde_json::json!({
        "id": 1,
        "method": "Runtime.evaluate",
        "params": { "expression": expression, "awaitPromise": true, "returnByValue": true },
    });
    let answer = tokio::time::timeout(Duration::from_secs(60), async {
        let (mut stream, _) = tokio_tungstenite::connect_async(socket).await.ok()?;
        stream
            .send(Message::Text(request.to_string().into()))
            .await
            .ok()?;
        while let Some(Ok(message)) = stream.next().await {
            let Message::Text(text) = message else {
                continue;
            };
            let reply: serde_json::Value = serde_json::from_str(&text).ok()?;
            if reply["id"] == 1 {
                let _ = stream.close(None).await;
                return reply["result"]["result"]["value"]["result"].as_i64();
            }
        }
        None
    })
    .await
    .ok()
    .flatten();
    match answer {
        Some(SIGNED_IN) => Ok(()),
        _ => Err(super::Error::WindowsSteamSignIn),
    }
}

/// Asks Steam to shut down, as its menu does, and gives it time to.
async fn shut_down(config: &wine::Config, home: &Path) {
    let Ok(mut command) = wine::command(config, home) else {
        return;
    };
    let _ = command
        .args([STEAM_EXE, "-shutdown"])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
    tokio::time::sleep(Duration::from_secs(10)).await;
}

#[cfg(test)]
mod tests;
