//! Steam's CDN in memory: what it holds of the world's depots, each under
//! the key the scripted Steam gives for it, and the requests it took.
use std::collections::HashMap;

use obelisk_core::testing::{Cdn, Reply};

use crate::{cm::test_steam::depot_key, content::tests::depot_routes};

/// The code the scripted Steam gives for a manifest it does not refuse.
const REQUEST_CODE: u64 = 7;

/// How a program for Apple silicon starts, and one for Intel.
pub(super) const ARM64: [u8; 8] = [0xcf, 0xfa, 0xed, 0xfe, 0x0c, 0x00, 0x00, 0x01];
pub(super) const INTEL: [u8; 8] = [0xcf, 0xfa, 0xed, 0xfe, 0x07, 0x00, 0x00, 0x01];

/// The program the world's games start on a Mac, and the Steam API library
/// a launch loads beside it.
pub(super) const PROGRAM: &str = "Game.app/Contents/MacOS/Game";
pub(super) const LIBRARY: &str = "Game.app/Contents/MacOS/libsteam_api.dylib";

/// A depot as the CDN holds it at one manifest.
pub(super) struct Held {
    depot: u32,
    manifest: u64,
    /// The app Steam gives the depot's key under.
    key_app: u32,
    /// Each file's name and what each of its chunks holds.
    files: Vec<(String, Vec<Vec<u8>>)>,
}

impl Held {
    /// A depot of one data file, which no other depot holds.
    pub(super) fn data(depot: u32, manifest: u64, key_app: u32) -> Self {
        let bytes = format!("depot {depot} at {manifest}").into_bytes();
        Self {
            depot,
            manifest,
            key_app,
            files: vec![(format!("data/{depot}.bin"), vec![bytes])],
        }
    }

    /// A depot of a Mac build's program, which starts with `head` and is cut
    /// in two chunks past the first bytes a pick reads, and of the Steam API
    /// library beside it, which starts alike.
    pub(super) fn program(depot: u32, manifest: u64, key_app: u32, head: [u8; 8]) -> Self {
        let mut start = head.to_vec();
        start.resize(5000, 0);
        Self {
            depot,
            manifest,
            key_app,
            files: vec![
                (
                    PROGRAM.into(),
                    vec![start, b"the rest of the program".to_vec()],
                ),
                (LIBRARY.into(), vec![head.to_vec()]),
            ],
        }
    }

    /// Where the CDN serves the depot's manifest.
    pub(super) fn manifest_path(&self) -> String {
        format!(
            "/depot/{}/manifest/{}/5/{REQUEST_CODE}",
            self.depot, self.manifest
        )
    }

    /// Where the CDN serves the first chunk of the file `name`.
    pub(super) fn first_chunk_path(&self, name: &str) -> String {
        use sha1::{Digest, Sha1};
        let (_, pieces) = self
            .files
            .iter()
            .find(|(file, _)| file == name)
            .expect("a file of the depot");
        let sha = crate::content::hex(&Sha1::digest(&pieces[0]));
        format!("/depot/{}/chunk/{sha}", self.depot)
    }

    /// Each file's name and what an installation holds of it.
    pub(super) fn installed(&self) -> Vec<(&str, Vec<u8>)> {
        self.files
            .iter()
            .map(|(name, pieces)| (name.as_str(), pieces.concat()))
            .collect()
    }

    /// What its files weigh once installed.
    pub(super) fn size(&self) -> u64 {
        self.installed()
            .iter()
            .map(|(_, bytes)| bytes.len() as u64)
            .sum()
    }

    /// What the CDN sends for the depot's manifest, and for all its chunks.
    pub(super) fn sent_bytes(&self) -> (u64, u64) {
        let (manifest, chunks): (Vec<_>, Vec<_>) = self
            .routes()
            .into_iter()
            .partition(|(path, _)| path.contains("/manifest/"));
        let weigh =
            |routes: Vec<(String, Vec<u8>)>| routes.iter().map(|(_, body)| body.len() as u64).sum();
        (weigh(manifest), weigh(chunks))
    }

    fn routes(&self) -> HashMap<String, Vec<u8>> {
        let files: Vec<(&str, Vec<&[u8]>)> = self
            .files
            .iter()
            .map(|(name, pieces)| (name.as_str(), pieces.iter().map(Vec::as_slice).collect()))
            .collect();
        let files: Vec<(&str, &[&[u8]])> = files
            .iter()
            .map(|(name, pieces)| (*name, pieces.as_slice()))
            .collect();
        depot_routes(
            self.depot,
            self.manifest,
            REQUEST_CODE,
            &depot_key(self.depot, self.key_app),
            &files,
        )
    }
}

fn routes(depots: &[Held]) -> HashMap<String, Vec<u8>> {
    depots.iter().flat_map(Held::routes).collect()
}

/// A CDN holding `depots`, and nothing else.
pub(super) fn holding(depots: &[Held]) -> Cdn {
    let routes = routes(depots);
    Cdn::in_memory(move |arrival| match routes.get(arrival.path()) {
        Some(body) => Reply::ok(body.clone()),
        None => Reply::status(404, ""),
    })
}

/// The same, which refuses a request that bears no token.
pub(super) fn holding_behind_tokens(depots: &[Held]) -> Cdn {
    let routes = routes(depots);
    Cdn::in_memory(move |arrival| match routes.get(arrival.path()) {
        Some(_) if arrival.target == arrival.path() => Reply::status(403, ""),
        Some(body) => Reply::ok(body.clone()),
        None => Reply::status(404, ""),
    })
}

/// The path of each request `cdn` took, in the order they came.
pub(super) fn requests(cdn: &Cdn) -> Vec<String> {
    cdn.arrivals()
        .iter()
        .map(|arrival| arrival.path().to_owned())
        .collect()
}

/// The path of each manifest `cdn` was asked for, in the order they came.
pub(super) fn manifests_asked(cdn: &Cdn) -> Vec<String> {
    let mut paths = requests(cdn);
    paths.retain(|path| path.contains("/manifest/"));
    paths
}
