//! A scripted Steam for the tests of a build's depots and manifests: the
//! licenses, packages and apps the account holds, answered in whatever order
//! Obelisk asks, with every request kept for the test to count.
use std::{convert::Infallible, fmt::Write as _};

use super::*;
use crate::client::Client;
use test_server::TestServer;

const ACCOUNT: u64 = 76_561_198_000_000_000;
const PATIENCE: Duration = Duration::from_secs(30);

/// A host the content directory may name, which Obelisk accepts as a host
/// but cannot turn into a URL: a download finds no CDN address, and nothing
/// is ever sent to a CDN.
pub(crate) const NO_CDN_ADDRESS: &str = "999.999.999.999";
/// A host that is a URL host. Nothing reaches it while a download needs no
/// CDN request.
pub(crate) const UNUSED_CDN: &str = "cdn.invalid";

/// One request Obelisk made of Steam.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Request {
    /// A PICS product query for apps (its access-token query is not kept).
    Apps(Vec<u32>),
    /// A PICS product query for license packages.
    Packages(Vec<u32>),
    DepotKey {
        depot: u32,
        app: u32,
    },
    ManifestCode {
        depot: u32,
        app: u32,
        manifest: u64,
    },
    ContentServers,
    CdnToken {
        depot: u32,
        app: u32,
    },
}

/// What the scripted Steam knows.
pub(crate) struct World {
    pub(crate) apps: Vec<AppSpec>,
    /// The license packages the account holds.
    pub(crate) packages: Vec<PackageSpec>,
    /// The CDN host the content directory lists first, for every app; none
    /// when empty.
    pub(crate) cdn_host: String,
    /// The hosts it lists after, each for the apps named only, in this order.
    pub(crate) restricted_hosts: Vec<(String, Vec<u32>)>,
    /// The depots whose manifests Steam refuses a request code for.
    pub(crate) refused_manifests: Vec<u32>,
}

impl World {
    pub(crate) fn new(apps: Vec<AppSpec>, packages: Vec<PackageSpec>) -> Self {
        Self {
            apps,
            packages,
            cdn_host: NO_CDN_ADDRESS.into(),
            restricted_hosts: Vec::new(),
            refused_manifests: Vec::new(),
        }
    }

    /// Steam describes `app` from now on as given, in place of the app of the
    /// same ID.
    pub(crate) fn replace_app(&mut self, app: AppSpec) {
        let known = self
            .apps
            .iter_mut()
            .find(|known| known.id == app.id)
            .expect("an app of the world");
        *known = app;
    }
}

/// A license package: the apps and depots its license grants.
pub(crate) struct PackageSpec {
    id: u32,
    apps: Vec<u32>,
    depots: Vec<u32>,
}

impl PackageSpec {
    pub(crate) fn new(id: u32, apps: &[u32], depots: &[u32]) -> Self {
        Self {
            id,
            apps: apps.to_vec(),
            depots: depots.to_vec(),
        }
    }

    /// The package as PICS sends it, in binary KeyValues.
    fn buffer(&self) -> Bytes {
        let mut buffer = 1_u32.to_le_bytes().to_vec();
        buffer.push(0);
        buffer.extend(format!("{}\0", self.id).bytes());
        for (name, ids) in [("appids", &self.apps), ("depotids", &self.depots)] {
            buffer.push(0);
            buffer.extend(format!("{name}\0").bytes());
            for (index, id) in ids.iter().enumerate() {
                buffer.push(2);
                buffer.extend(format!("{index}\0").bytes());
                buffer.extend(id.to_le_bytes());
            }
            buffer.push(8);
        }
        buffer.extend([8, 8]);
        buffer.into()
    }
}

/// An app as PICS describes it.
#[derive(Clone)]
pub(crate) struct AppSpec {
    id: u32,
    name: String,
    platforms: String,
    free_to_download: bool,
    launches: Vec<(String, String)>,
    depots: Vec<DepotSpec>,
}

impl AppSpec {
    pub(crate) fn new(id: u32, name: &str) -> Self {
        Self {
            id,
            name: name.into(),
            platforms: String::new(),
            free_to_download: false,
            launches: Vec::new(),
            depots: Vec::new(),
        }
    }

    /// Its declared `oslist`, such as `windows,macos`.
    pub(crate) fn platforms(mut self, platforms: &str) -> Self {
        self.platforms = platforms.into();
        self
    }

    pub(crate) fn free_to_download(mut self) -> Self {
        self.free_to_download = true;
        self
    }

    /// A launch option starting `executable` on the platforms of `oslist`.
    pub(crate) fn launching(mut self, executable: &str, oslist: &str) -> Self {
        self.launches.push((executable.into(), oslist.into()));
        self
    }

    pub(crate) fn depot(mut self, depot: DepotSpec) -> Self {
        self.depots.push(depot);
        self
    }

    /// The app as PICS sends it, in text KeyValues.
    fn pics(&self) -> String {
        let mut text = format!(
            r#""appinfo" {{ "appid" "{}" "common" {{ "name" "{}" "oslist" "{}" "freetodownload" "{}" }}"#,
            self.id,
            self.name,
            self.platforms,
            u8::from(self.free_to_download)
        );
        text.push_str(r#" "config" { "launch" {"#);
        for (index, (executable, oslist)) in self.launches.iter().enumerate() {
            write!(
                text,
                r#" "{index}" {{ "executable" "{executable}" "config" {{ "oslist" "{oslist}" }} }}"#
            )
            .unwrap();
        }
        text.push_str(r#" } } "depots" {"#);
        for depot in &self.depots {
            text.push_str(&depot.pics());
        }
        text.push_str(" } }");
        text
    }
}

/// One of an app's depots as PICS describes it.
#[derive(Clone)]
pub(crate) struct DepotSpec {
    id: u32,
    platforms: String,
    arch: String,
    language: String,
    dlc: Option<u32>,
    shared_from: Option<u32>,
    manifest: Option<u64>,
    size: Option<u64>,
}

impl DepotSpec {
    pub(crate) fn new(id: u32) -> Self {
        Self {
            id,
            platforms: String::new(),
            arch: String::new(),
            language: String::new(),
            dlc: None,
            shared_from: None,
            manifest: None,
            size: None,
        }
    }

    pub(crate) fn platforms(mut self, platforms: &str) -> Self {
        self.platforms = platforms.into();
        self
    }

    pub(crate) fn language(mut self, language: &str) -> Self {
        self.language = language.into();
        self
    }

    pub(crate) fn dlc(mut self, app: u32) -> Self {
        self.dlc = Some(app);
        self
    }

    /// The depot comes from `app`'s depot of the same ID.
    pub(crate) fn shared_from(mut self, app: u32) -> Self {
        self.shared_from = Some(app);
        self
    }

    /// Its manifest on the public branch.
    pub(crate) fn manifest(mut self, manifest: u64) -> Self {
        self.manifest = Some(manifest);
        self
    }

    /// The bytes PICS says its public manifest installs.
    pub(crate) fn size(mut self, size: u64) -> Self {
        self.size = Some(size);
        self
    }

    fn pics(&self) -> String {
        let mut text = format!(r#" "{}" {{ "config" {{"#, self.id);
        for (field, value) in [
            ("oslist", &self.platforms),
            ("osarch", &self.arch),
            ("language", &self.language),
        ] {
            if !value.is_empty() {
                write!(text, r#" "{field}" "{value}""#).unwrap();
            }
        }
        text.push_str(" }");
        for (field, value) in [("depotfromapp", self.shared_from), ("dlcappid", self.dlc)] {
            if let Some(value) = value {
                write!(text, r#" "{field}" "{value}""#).unwrap();
            }
        }
        if let Some(manifest) = self.manifest {
            write!(text, r#" "manifests" {{ "public" {{ "gid" "{manifest}""#).unwrap();
            if let Some(size) = self.size {
                write!(text, r#" "size" "{size}""#).unwrap();
            }
            text.push_str(" } }");
        }
        text.push_str(" }");
        text
    }
}

/// The key Steam gives for `depot` when asked under `app`: it names both, so
/// a test can tell whose key a download holds.
pub(crate) fn depot_key(depot: u32, app: u32) -> [u8; 32] {
    let names: Vec<u8> = depot
        .to_le_bytes()
        .into_iter()
        .chain(app.to_le_bytes())
        .collect();
    names.repeat(4).try_into().unwrap()
}

/// Steam, answering one connection from a [`World`]. It answers only while
/// [`during`](Self::during) runs a call.
pub(crate) struct Steam {
    server: TestServer,
    pub(crate) world: World,
    requests: Vec<Request>,
}

impl Steam {
    /// A client logged on as an account holding the packages of `world`.
    pub(crate) async fn account(world: World) -> (Client, Steam) {
        let (connection, server) = test_server::connection(ACCOUNT, true).await;
        let mut steam = Steam {
            server,
            world,
            requests: Vec::new(),
        };
        let licenses = steam
            .world
            .packages
            .iter()
            .map(|package| proto::License {
                package_id: package.id,
                ..Default::default()
            })
            .collect();
        let list = proto::LicenseList {
            result: Some(1),
            licenses,
        };
        steam
            .server
            .send(
                LICENSE_LIST,
                proto::Header::default(),
                &list.encode_to_vec(),
            )
            .await;
        (Client::with_test_connection(connection), steam)
    }

    /// Runs `work`, which asks Steam, while Steam answers.
    pub(crate) async fn during<T>(&mut self, work: impl Future<Output = T>) -> T {
        tokio::select! {
            done = timeout(PATIENCE, work) => done.expect("Steam was never done answering"),
            never = self.serve() => match never {},
        }
    }

    /// Forgets the requests so far.
    pub(crate) fn forget_requests(&mut self) {
        self.requests.clear();
    }

    /// The requests since the start or the last `forget_requests`, in the order they came.
    pub(crate) fn requests(&self) -> &[Request] {
        &self.requests
    }

    /// The apps of each PICS app query, each query's apps sorted.
    pub(crate) fn app_queries(&self) -> Vec<Vec<u32>> {
        self.requests
            .iter()
            .filter_map(|request| match request {
                Request::Apps(apps) => Some(apps.clone()),
                _ => None,
            })
            .collect()
    }

    pub(crate) fn package_queries(&self) -> usize {
        self.requests
            .iter()
            .filter(|request| matches!(request, Request::Packages(_)))
            .count()
    }

    /// The `(depot, app)` of each depot key asked, sorted.
    pub(crate) fn depot_keys(&self) -> Vec<(u32, u32)> {
        let mut keys: Vec<_> = self
            .requests
            .iter()
            .filter_map(|request| match request {
                Request::DepotKey { depot, app } => Some((*depot, *app)),
                _ => None,
            })
            .collect();
        keys.sort_unstable();
        keys
    }

    /// The `(depot, app, manifest)` of each manifest request code asked, sorted.
    pub(crate) fn manifest_codes(&self) -> Vec<(u32, u32, u64)> {
        let mut codes: Vec<_> = self
            .requests
            .iter()
            .filter_map(|request| match request {
                Request::ManifestCode {
                    depot,
                    app,
                    manifest,
                } => Some((*depot, *app, *manifest)),
                _ => None,
            })
            .collect();
        codes.sort_unstable();
        codes
    }

    pub(crate) fn content_server_queries(&self) -> usize {
        self.requests
            .iter()
            .filter(|request| matches!(request, Request::ContentServers))
            .count()
    }

    /// The `(depot, app)` of each CDN authorization asked, sorted.
    pub(crate) fn cdn_tokens(&self) -> Vec<(u32, u32)> {
        let mut tokens: Vec<_> = self
            .requests
            .iter()
            .filter_map(|request| match request {
                Request::CdnToken { depot, app } => Some((*depot, *app)),
                _ => None,
            })
            .collect();
        tokens.sort_unstable();
        tokens
    }

    async fn serve(&mut self) -> Infallible {
        loop {
            let packet = self.server.next().await;
            match packet.kind {
                TOKEN_REQUEST => {
                    let tokens = proto::AccessTokenRequest::decode(packet.body.clone()).unwrap();
                    assert!(tokens.package_ids.is_empty());
                    let response = proto::AccessTokenResponse::default();
                    self.server.reply(&packet, TOKEN_RESPONSE, &response).await;
                }
                PRODUCT_REQUEST => self.products(&packet).await,
                DEPOT_KEY_REQUEST => {
                    let request = proto::DepotKeyRequest::decode(packet.body.clone()).unwrap();
                    self.requests.push(Request::DepotKey {
                        depot: request.depot_id,
                        app: request.app_id,
                    });
                    let response = proto::DepotKeyResponse {
                        result: Some(1),
                        depot_id: request.depot_id,
                        key: depot_key(request.depot_id, request.app_id).to_vec(),
                    };
                    self.server
                        .reply(&packet, DEPOT_KEY_RESPONSE, &response)
                        .await;
                }
                SERVICE_REQUEST => self.service(&packet).await,
                kind => panic!("Obelisk sent message {kind}, which Steam does not answer here"),
            }
        }
    }

    async fn products(&mut self, packet: &Packet) {
        let request = proto::ProductRequest::decode(packet.body.clone()).unwrap();
        let mut apps: Vec<_> = request.apps.iter().map(|app| app.id).collect();
        let mut packages: Vec<_> = request.packages.iter().map(|package| package.id).collect();
        apps.sort_unstable();
        packages.sort_unstable();
        if !apps.is_empty() {
            self.requests.push(Request::Apps(apps.clone()));
        }
        if !packages.is_empty() {
            self.requests.push(Request::Packages(packages.clone()));
        }
        let mut response = proto::ProductResponse::default();
        for id in apps {
            match self.world.apps.iter().find(|app| app.id == id) {
                Some(app) => response.apps.push(proto::ProductInfo {
                    id,
                    buffer: app.pics().into_bytes().into(),
                    ..Default::default()
                }),
                None => response.unknown_apps.push(id),
            }
        }
        for id in packages {
            match self.world.packages.iter().find(|package| package.id == id) {
                Some(package) => response.packages.push(proto::ProductInfo {
                    id,
                    buffer: package.buffer(),
                    ..Default::default()
                }),
                None => response.unknown_packages.push(id),
            }
        }
        self.server.reply(packet, PRODUCT_RESPONSE, &response).await;
    }

    async fn service(&mut self, packet: &Packet) {
        match packet.header.target_name.as_deref() {
            Some("ContentServerDirectory.GetServersForSteamPipe#1") => {
                self.requests.push(Request::ContentServers);
                let everyone = Vec::new();
                let listed = std::iter::once((&self.world.cdn_host, &everyone)).chain(
                    self.world
                        .restricted_hosts
                        .iter()
                        .map(|(host, apps)| (host, apps)),
                );
                let response = proto::ServerResponse {
                    servers: listed
                        .enumerate()
                        .map(|(load, (host, apps))| proto::ServerInfo {
                            server_type: "CDN".into(),
                            weighted_load: load as f32,
                            host: host.clone(),
                            https_support: "mandatory".into(),
                            allowed_app_ids: apps.clone(),
                            ..Default::default()
                        })
                        .collect(),
                };
                self.server.reply(packet, SERVICE_RESPONSE, &response).await;
            }
            Some("ContentServerDirectory.GetManifestRequestCode#1") => {
                let request = proto::ManifestCodeRequest::decode(packet.body.clone()).unwrap();
                self.requests.push(Request::ManifestCode {
                    depot: request.depot_id,
                    app: request.app_id,
                    manifest: request.manifest_id,
                });
                let code = if self.world.refused_manifests.contains(&request.depot_id) {
                    0
                } else {
                    7
                };
                let response = proto::ManifestCodeResponse { code };
                self.server.reply(packet, SERVICE_RESPONSE, &response).await;
            }
            Some("ContentServerDirectory.GetCDNAuthToken#1") => {
                let request = proto::CdnAuthRequest::decode(packet.body.clone()).unwrap();
                self.requests.push(Request::CdnToken {
                    depot: request.depot_id,
                    app: request.app_id,
                });
                let response = proto::CdnAuthResponse {
                    token: "token".into(),
                    expiration: 0,
                };
                self.server.reply(packet, SERVICE_RESPONSE, &response).await;
            }
            other => panic!("Obelisk called {other:?}, which Steam does not answer here"),
        }
    }
}
