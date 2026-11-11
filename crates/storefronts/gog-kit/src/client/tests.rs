use std::{
    io::{Read, Write},
    net::TcpListener,
    path::Path,
    sync::{
        Mutex,
        atomic::{AtomicBool, Ordering},
    },
    thread,
    time::{Duration, Instant},
};

use md5::{Digest as _, Md5};
use obelisk_core::{
    install::hex,
    testing::{Cdn, Reply as CdnReply, reaching},
};
use serde_json::{Value, json};

use super::*;

/// A client of account 42, signed in with a token that has not lapsed, whose
/// chunks no CDN serves.
pub(super) fn signed_in(api: Api) -> Client {
    signed_in_reaching(api, reaching(&[]))
}

/// The same, whose chunks come through `cdn`, such as a CDN in memory.
fn signed_in_reaching(api: Api, cdn: CdnClient) -> Client {
    let session = Session {
        credentials: Credentials {
            user_id: "42".into(),
            username: "Player".into(),
            refresh_token: "refresh-old".into(),
        },
        access_token: "access-old".into(),
        expires_at: Instant::now() + Duration::from_secs(3600),
    };
    Client {
        downloads: Default::default(),
        tokens: TokenSource::new(api, session),
        cdn,
        avatar_url: None,
        build_choice: Default::default(),
    }
}

/// What GOG answers the next request for `path` that bears `bearer`. In its
/// body, `FIXTURE_BASE` stands for where GOG answers.
struct Reply {
    path: &'static str,
    bearer: Option<&'static str>,
    status: u16,
    body: Value,
}

/// GOG's services and the documents of its content system on a local socket,
/// which answer as scripted and keep the requests they took.
struct Gog {
    api: Api,
    requests: Arc<Mutex<Vec<String>>>,
    stop: Arc<AtomicBool>,
    serving: Option<thread::JoinHandle<()>>,
}

impl Gog {
    fn answering(mut replies: Vec<Reply>) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let requests = Arc::new(Mutex::new(Vec::new()));
        let stop = Arc::new(AtomicBool::new(false));
        let (taken, stopped) = (Arc::clone(&requests), Arc::clone(&stop));
        let answering_at = base.clone();
        let serving = thread::spawn(move || {
            while !stopped.load(Ordering::SeqCst) {
                let Ok((mut socket, _)) = listener.accept() else {
                    thread::sleep(Duration::from_millis(2));
                    continue;
                };
                // An accepted socket can inherit the listener's mode on macOS.
                socket.set_nonblocking(false).unwrap();
                socket
                    .set_read_timeout(Some(Duration::from_secs(2)))
                    .unwrap();
                let mut request = Vec::new();
                let mut buffer = [0_u8; 4096];
                while !request.windows(4).any(|end| end == b"\r\n\r\n") {
                    match socket.read(&mut buffer) {
                        Ok(count) if count > 0 => request.extend_from_slice(&buffer[..count]),
                        _ => break,
                    }
                }
                if request.is_empty() {
                    continue;
                }
                let request = String::from_utf8(request).unwrap();
                let target = request.split_whitespace().nth(1).unwrap();
                let path = target.split('?').next().unwrap();
                let index = replies
                    .iter()
                    .position(|reply| {
                        reply.path == path
                            && reply
                                .bearer
                                .is_none_or(|token| request.contains(&format!("Bearer {token}")))
                    })
                    .unwrap_or_else(|| panic!("no reply left for {path}"));
                let reply = replies.remove(index);
                taken.lock().unwrap().push(request);
                let body = reply
                    .body
                    .to_string()
                    .replace("FIXTURE_BASE", &answering_at);
                let answer = format!(
                    "HTTP/1.1 {} Fixture\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    reply.status,
                    body.len(),
                );
                let _ = socket.write_all(answer.as_bytes());
            }
        });
        let mut api = Api::new().unwrap();
        api.auth = base.clone();
        api.gameplay = base.clone();
        api.embed = base.clone();
        api.content = base.clone();
        api.cdn = base;
        Self {
            api,
            requests,
            stop,
            serving: Some(serving),
        }
    }

    fn requests(&self) -> Vec<String> {
        self.requests.lock().unwrap().clone()
    }
}

impl Drop for Gog {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        self.serving.take().unwrap().join().unwrap();
    }
}

const ACHIEVEMENTS: &str = "/clients/7/users/42/achievements";

#[tokio::test]
async fn a_request_gog_rejects_is_asked_again_with_a_renewed_token_until_the_account_signs_out() {
    let gog = Gog::answering(vec![
        Reply {
            path: ACHIEVEMENTS,
            bearer: Some("access-old"),
            status: 401,
            body: json!({}),
        },
        Reply {
            path: "/token",
            bearer: None,
            status: 200,
            body: json!({"access_token": "access-new", "refresh_token": "refresh-new",
                "user_id": "42", "expires_in": 3600}),
        },
        Reply {
            path: ACHIEVEMENTS,
            bearer: Some("access-new"),
            status: 200,
            body: json!({"items": [{"achievement_key": "kill_boss", "name": "Boss"}]}),
        },
    ]);
    let client = signed_in(gog.api.clone());
    let achievements = client.achievements("7", "en-US").await.unwrap();
    assert_eq!(achievements.len(), 1);
    assert_eq!(achievements[0].id, "kill_boss");

    // The host learns of the rotated credentials, which keep the account's name.
    assert_eq!(client.credentials_revision(), 2);
    let credentials = client.credentials().await.unwrap();
    assert_eq!(credentials.refresh_token, "refresh-new");
    assert_eq!(credentials.user_id, "42");
    assert_eq!(credentials.username, "Player");
    let requests = gog.requests();
    assert_eq!(requests.len(), 3);
    assert!(requests[1].contains("grant_type=refresh_token"));
    assert!(requests[1].contains("refresh_token=refresh-old"));
    assert!(!requests[1].to_lowercase().contains("authorization:"));

    client.disconnect().await.unwrap();
    assert!(matches!(
        client.achievements("7", "en-US").await,
        Err(GogError::Unauthorized)
    ));
    assert!(matches!(
        client.credentials().await,
        Err(GogError::Unauthorized)
    ));
    assert!(matches!(
        client.prepare().await,
        Err(GogError::Unauthorized)
    ));
    assert_eq!(gog.requests().len(), 3, "a signed-out account asks nothing");
}

const BUILDS: &str = "/products/7/os/osx/builds";
const WINDOWS_BUILDS: &str = "/products/7/os/windows/builds";
const BUILD_MANIFEST: &str = "/build";
const DEPOT_MANIFEST: &str = "/content-system/v2/meta/01/23/0123456789abcdef0123456789abcdef";
const OWNED: &str = "/user/data/games";
const SECURE_LINK: &str = "/products/7/secure_link";

const INFO: &str = "goggame-7.info";
const PROGRAM: &str = "Game.app/Contents/MacOS/Game";

/// A chunk holding `bytes`: how a depot's manifest describes it, where a
/// CDN serves it under `/store`, and what the CDN serves there.
fn chunk(bytes: &[u8]) -> (Value, String, Vec<u8>) {
    let mut encoder = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::default());
    encoder.write_all(bytes).unwrap();
    let packed = encoder.finish().unwrap();
    let (md5, packed_md5) = (hex(&Md5::digest(bytes)), hex(&Md5::digest(&packed)));
    let described = json!({"md5": md5, "size": bytes.len(),
        "compressedMd5": packed_md5, "compressedSize": packed.len()});
    let path = format!(
        "/store/{}",
        crate::manifest::galaxy_path(&packed_md5).unwrap()
    );
    (described, path, packed)
}

/// Product 7's Mac build 52: one depot of the file that names its play task
/// and of the program that task starts, cut in two chunks past the first
/// bytes a pick reads.
struct MacBuild {
    /// What the installation holds of the info file and of the program.
    info: Vec<u8>,
    program: Vec<u8>,
    /// Where the CDN serves the info file's chunk and the program's first.
    info_path: String,
    head_path: String,
    depot_manifest: Value,
    cdn: Cdn,
}

fn mac_build() -> MacBuild {
    let info = json!({"playTasks": [{"type": "FileTask", "isPrimary": true, "path": PROGRAM}]})
        .to_string()
        .into_bytes();
    let mut head = vec![0xcf, 0xfa, 0xed, 0xfe, 0x0c, 0x00, 0x00, 0x01];
    head.resize(5000, 0);
    let rest = b"the rest of the program".to_vec();
    let (info_chunk, info_path, info_served) = chunk(&info);
    let (head_chunk, head_path, head_served) = chunk(&head);
    let (rest_chunk, rest_path, rest_served) = chunk(&rest);
    let depot_manifest = json!({"depot": {"items": [
        {"type": "DepotFile", "path": INFO, "chunks": [info_chunk]},
        {"type": "DepotFile", "path": PROGRAM, "chunks": [head_chunk, rest_chunk],
         "flags": ["executable"]},
    ]}});
    let served = [
        (info_path.clone(), info_served),
        (head_path.clone(), head_served),
        (rest_path, rest_served),
    ];
    let cdn = Cdn::in_memory(move |arrival| {
        match served.iter().find(|(path, _)| path == arrival.path()) {
            Some((_, body)) => CdnReply::ok(body.clone()),
            None => CdnReply::status(404, ""),
        }
    });
    MacBuild {
        info,
        program: [head, rest].concat(),
        info_path,
        head_path,
        depot_manifest,
        cdn,
    }
}

impl MacBuild {
    /// What GOG answers a download as it prepares: the build it lists, the
    /// build's manifest and the products the account owns.
    fn listed(&self) -> Vec<Reply> {
        let build = json!({"items": [{"build_id": "52", "generation": 2,
            "version_name": "1.0", "link": "FIXTURE_BASE/build"}]});
        let manifest = json!({"baseProductId": "7", "buildId": "52",
            "depots": [{"productId": "7", "languages": ["*"], "size": self.program.len(),
                "manifest": "0123456789abcdef0123456789abcdef"}],
            "products": [{"productId": "7", "name": "Game"}]});
        vec![
            public(BUILDS, 200, build),
            public(BUILD_MANIFEST, 200, manifest),
            of_the_account(OWNED, json!({"owned": [7]})),
        ]
    }

    /// The items of the build's depot.
    fn items(&self) -> Reply {
        public(DEPOT_MANIFEST, 200, self.depot_manifest.clone())
    }

    /// The secure link that names the CDN in memory for the product's chunks.
    fn secure_link(&self) -> Reply {
        let store = format!("{}/store", self.cdn.base());
        of_the_account(
            SECURE_LINK,
            json!({"urls": [{"url": store, "priority": 1}]}),
        )
    }

    /// The paths of the requests the CDN took, in the order they came.
    fn chunks_asked(&self) -> Vec<String> {
        self.cdn
            .arrivals()
            .iter()
            .map(|arrival| arrival.path().to_owned())
            .collect()
    }

    /// Asserts that `installation` is of the build, and holds its files.
    fn assert_installed(&self, installation: &Installation) {
        assert_eq!(installation.product_id, "7");
        assert_eq!(installation.user_id, "42");
        assert_eq!(installation.platform, Platform::Mac);
        assert_eq!(installation.build_id, "52");
        assert_eq!(installation.version_name, "1.0");
        assert_eq!(installation.language, "en-US");
        let size = (self.info.len() + self.program.len()) as u64;
        assert_eq!(installation.total_bytes, size);
        let folder = Path::new(&installation.directory);
        assert!(folder.ends_with("gog-7-macos"));
        assert_eq!(std::fs::read(folder.join(INFO)).unwrap(), self.info);
        assert_eq!(std::fs::read(folder.join(PROGRAM)).unwrap(), self.program);
    }
}

/// An answer to a request that bears no account credential.
fn public(path: &'static str, status: u16, body: Value) -> Reply {
    Reply {
        path,
        bearer: None,
        status,
        body,
    }
}

/// An answer to a request the account's token signs.
fn of_the_account(path: &'static str, body: Value) -> Reply {
    Reply {
        path,
        bearer: Some("access-old"),
        status: 200,
        body,
    }
}

#[tokio::test]
async fn a_mac_build_whose_program_holds_an_arm64_slice_is_picked_and_its_download_starts_from_the_items_read()
 {
    let build = mac_build();
    let mut replies = build.listed();
    // GOG has no Windows build of the game.
    replies.push(public(WINDOWS_BUILDS, 404, json!({})));
    replies.extend([build.items(), build.secure_link()]);
    // The download asks for the build again, but not for its depot's items.
    replies.extend(build.listed());
    replies.push(build.secure_link());
    let gog = Gog::answering(replies);
    let client = signed_in_reaching(gog.api.clone(), reaching(&[&build.cdn]));

    let platform = client.platform("7", "en-US").await.unwrap();
    assert_eq!(platform, Platform::Mac);
    // The file that names the play task, then the first chunk of its program.
    assert_eq!(
        build.chunks_asked(),
        [build.info_path.clone(), build.head_path.clone()]
    );
    assert_eq!(gog.requests().len(), 6);

    let parent = tempfile::tempdir().unwrap();
    let installation = client
        .download("7", Platform::Mac, "en-US", parent.path())
        .await
        .unwrap();
    build.assert_installed(&installation);
    assert_eq!(build.chunks_asked().len(), 5, "every chunk, once more");
    assert_eq!(gog.requests().len(), 10);
    let progress = client.progress().unwrap();
    assert_eq!(progress.state, TransferState::Complete);
}

#[tokio::test]
async fn a_download_without_a_pick_reads_its_depots_items_and_takes_their_chunks_through_its_cdn() {
    let build = mac_build();
    let mut replies = build.listed();
    replies.extend([build.items(), build.secure_link()]);
    let gog = Gog::answering(replies);
    let client = signed_in_reaching(gog.api.clone(), reaching(&[&build.cdn]));
    let parent = tempfile::tempdir().unwrap();
    let installation = client
        .download("7", Platform::Mac, "en-US", parent.path())
        .await
        .unwrap();
    build.assert_installed(&installation);
    assert_eq!(build.chunks_asked().len(), 3);
    let requests = gog.requests();
    assert_eq!(requests.len(), 5);
    // Only the account's own requests bear its token.
    for request in &requests {
        let signed = request.contains("authorization: Bearer access-old");
        let of_the_account = [OWNED, SECURE_LINK]
            .iter()
            .any(|path| request.contains(path));
        assert_eq!(signed, of_the_account, "{request}");
    }
}
