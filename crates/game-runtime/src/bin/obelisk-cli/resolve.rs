//! The installations of every storefront account whose receipt book is in
//! Obelisk's library, and the one a command names.
use game_runtime::{
    epic::Epic,
    gog::Gog,
    installations::{Receipt, Storefront},
    launch,
    steam::Steam,
    wine,
};
use std::path::{Path, PathBuf};

/// One installation, as its account's receipt book keeps it.
pub(crate) struct Installed {
    /// The account whose book keeps it: Steam's id, or Epic's or GOG's.
    pub account: String,
    /// The account's receipt book.
    pub book: PathBuf,
    pub receipt: Kept,
}

/// A receipt, of whichever storefront.
pub(crate) enum Kept {
    Steam(Receipt<Steam>),
    Epic(Receipt<Epic>),
    Gog(Receipt<Gog>),
}

/// Runs `$body` with the receipt as `$receipt`, whichever storefront keeps it.
macro_rules! each {
    ($kept:expr, $receipt:ident => $body:expr) => {
        match $kept {
            Kept::Steam($receipt) => $body,
            Kept::Epic($receipt) => $body,
            Kept::Gog($receipt) => $body,
        }
    };
}

impl Installed {
    pub fn store(&self) -> &'static str {
        match self.receipt {
            Kept::Steam(_) => "steam",
            Kept::Epic(_) => "epic",
            Kept::Gog(_) => "gog",
        }
    }

    /// Names the installation in every action, as Obelisk's own.
    pub fn id(&self) -> String {
        each!(&self.receipt, receipt => receipt.id())
    }

    /// The game, as Obelisk names it: Steam's app id, Epic's
    /// `namespace:app`, GOG's product id.
    pub fn game(&self) -> String {
        match &self.receipt {
            Kept::Steam(receipt) => receipt.product.app_id.to_string(),
            Kept::Epic(receipt) => {
                format!("{}:{}", receipt.product.namespace, receipt.product.app_name)
            }
            Kept::Gog(receipt) => receipt.product.product_id.clone(),
        }
    }

    pub fn name(&self) -> &str {
        each!(&self.receipt, receipt => &receipt.name)
    }

    /// Its Wine prefix and how its game runs there, when it is a Windows
    /// build, as a launch resolves them.
    pub fn windows(
        &self,
        locations: &game_runtime::Locations,
    ) -> Result<Option<wine::Installation>, launch::Error> {
        each!(&self.receipt, receipt => receipt.windows(locations))
    }

    /// One line that tells it apart from the others.
    pub fn line(&self) -> String {
        format!("{}\t{}\t{}", self.store(), self.id(), self.name())
    }
}

/// Every installation the receipt books in `library` keep, by book, each
/// once: Neighbours keep the same one in each of their books.
pub(crate) fn installed(library: &Path) -> Vec<Installed> {
    let Ok(entries) = std::fs::read_dir(library) else {
        return Vec::new();
    };
    let mut books: Vec<_> = entries
        .filter_map(|entry| Some(entry.ok()?.path()))
        .collect();
    books.sort();
    let mut found: Vec<Installed> = Vec::new();
    for book in books {
        let Some(name) = book.file_name().and_then(|name| name.to_str()) else {
            continue;
        };
        let Some((store, account)) = account(name) else {
            continue;
        };
        let receipts = match store {
            "steam" => read::<Steam>(&book, Kept::Steam),
            "epic" => read::<Epic>(&book, Kept::Epic),
            _ => read::<Gog>(&book, Kept::Gog),
        };
        for receipt in receipts {
            let installed = Installed {
                account: account.clone(),
                book: book.clone(),
                receipt,
            };
            let seen = found
                .iter()
                .any(|other| other.store() == installed.store() && other.id() == installed.id());
            if !seen {
                found.push(installed);
            }
        }
    }
    found
}

/// The storefront and account of a receipt book named `name`: Steam's
/// `steam-installations-<id>.json`, Epic's and GOG's with the account's id in
/// hexadecimal, as Obelisk names them. Not the book of incomplete
/// installations beside each.
fn account(name: &str) -> Option<(&'static str, String)> {
    let stem = name.strip_suffix(".json")?;
    if stem.ends_with(".incomplete") {
        return None;
    }
    let (store, account) = stem.split_once("-installations-")?;
    let store = ["steam", "epic", "gog"]
        .into_iter()
        .find(|known| *known == store)?;
    if store == "steam" {
        return account
            .parse::<u64>()
            .ok()
            .map(|_| (store, account.to_owned()));
    }
    let bytes = (0..account.len())
        .step_by(2)
        .map(|at| u8::from_str_radix(account.get(at..at + 2)?, 16).ok())
        .collect::<Option<Vec<_>>>()?;
    let account = String::from_utf8(bytes)
        .ok()
        .filter(|account| !account.is_empty())?;
    Some((store, account))
}

/// The receipts the book at `file` keeps; none when it cannot be read.
fn read<S: Storefront>(file: &Path, kept: fn(Receipt<S>) -> Kept) -> Vec<Kept> {
    std::fs::read(file)
        .ok()
        .and_then(|bytes| serde_json::from_slice::<Vec<Receipt<S>>>(&bytes).ok())
        .unwrap_or_default()
        .into_iter()
        .map(kept)
        .collect()
}

/// The installation `wanted` names: by its id, else by its game's, else by
/// its whole name, else by a part of it, without regard to case. Each way
/// must name one installation only.
pub(crate) fn find<'a>(installed: &'a [Installed], wanted: &str) -> Result<&'a Installed, String> {
    let lowered = wanted.to_lowercase();
    let ways: [&dyn Fn(&Installed) -> bool; 4] = [
        &|found| found.id() == wanted,
        &|found| found.game() == wanted,
        &|found| found.name().to_lowercase() == lowered,
        &|found| found.name().to_lowercase().contains(&lowered),
    ];
    for way in ways {
        let matched: Vec<_> = installed.iter().filter(|found| way(found)).collect();
        match matched.as_slice() {
            [] => continue,
            [one] => return Ok(one),
            several => {
                let lines: Vec<_> = several.iter().map(|found| found.line()).collect();
                return Err(format!(
                    "\"{wanted}\" names several installations; name one by its id:\n{}",
                    lines.join("\n")
                ));
            }
        }
    }
    Err(format!("no installation is named \"{wanted}\""))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Keeps `receipts` in the book `name` of `library`.
    fn book(library: &Path, name: &str, receipts: serde_json::Value) {
        std::fs::write(library.join(name), receipts.to_string()).unwrap();
    }

    /// GOG's book of the account `48756d65`, which reads "Hume".
    fn library() -> tempfile::TempDir {
        let library = tempfile::tempdir().unwrap();
        book(
            library.path(),
            "steam-installations-76561198000000001.json",
            serde_json::json!([
                {"appId": 553850, "language": "english", "name": "HELLDIVERS™ 2",
                 "directory": "/games/Steam/steam-553850-windows", "platform": "windows",
                 "totalBytes": 1},
                {"appId": 1245620, "language": "english", "name": "ELDEN RING",
                 "directory": "/games/Steam/steam-1245620-windows", "platform": "windows",
                 "totalBytes": 1}
            ]),
        );
        book(
            library.path(),
            "gog-installations-48756d65.json",
            serde_json::json!([
                {"productId": "1207658924", "name": "The Witcher: Enhanced Edition",
                 "directory": "/games/GOG/1207658924-windows", "platform": "windows",
                 "totalBytes": 1},
                {"productId": "1207658924", "name": "The Witcher: Enhanced Edition",
                 "directory": "/games/GOG/1207658924-macos", "platform": "macos",
                 "totalBytes": 1},
                {"productId": "1495134320", "name": "The Witcher 3: Wild Hunt",
                 "directory": "/games/GOG/1495134320-windows", "platform": "windows",
                 "totalBytes": 1}
            ]),
        );
        // A Neighbour's book keeps the same installation, and incomplete
        // ones are no installations.
        book(
            library.path(),
            "gog-installations-4f74686572.json",
            serde_json::json!([
                {"productId": "1495134320", "name": "The Witcher 3: Wild Hunt",
                 "directory": "/games/GOG/1495134320-windows", "platform": "windows",
                 "totalBytes": 1}
            ]),
        );
        book(
            library.path(),
            "gog-installations-48756d65.incomplete.json",
            serde_json::json!([
                {"productId": "1", "name": "Unfinished", "directory": "/games/GOG/1-windows",
                 "platform": "windows", "totalBytes": 1}
            ]),
        );
        std::fs::write(library.path().join("epic-installations-zz.json"), "[]").unwrap();
        std::fs::write(
            library
                .path()
                .join("steam-installations-76561198000000002.json"),
            "{",
        )
        .unwrap();
        library
    }

    #[test]
    fn every_book_s_installations_are_found_once_with_their_account() {
        let library = library();

        let installed = installed(library.path());

        let found: Vec<_> = installed
            .iter()
            .map(|found| (found.store(), found.account.as_str(), found.id()))
            .collect();
        assert_eq!(
            found,
            [
                ("gog", "Hume", "1207658924:windows".to_owned()),
                ("gog", "Hume", "1207658924:macos".to_owned()),
                ("gog", "Hume", "1495134320:windows".to_owned()),
                ("steam", "76561198000000001", "553850-windows".to_owned()),
                ("steam", "76561198000000001", "1245620-windows".to_owned()),
            ]
        );
        assert!(installed.iter().all(|found| found.name() != "Unfinished"));
        assert!(super::installed(&library.path().join("missing")).is_empty());
    }

    #[test]
    fn a_game_is_named_by_its_installation_its_store_s_id_or_its_name() {
        let library = library();
        let installed = installed(library.path());
        let id = |wanted: &str| find(&installed, wanted).map(Installed::id);

        assert_eq!(id("1495134320").unwrap(), "1495134320:windows");
        assert_eq!(id("1207658924:macos").unwrap(), "1207658924:macos");
        assert_eq!(id("553850").unwrap(), "553850-windows");
        assert_eq!(id("elden ring").unwrap(), "1245620-windows");
        assert_eq!(id("divers").unwrap(), "553850-windows");
        // The whole name wins over a part of another's.
        assert_eq!(
            id("the witcher 3: wild hunt").unwrap(),
            "1495134320:windows"
        );
        assert!(id("1207658924").unwrap_err().contains("1207658924:macos"));
        assert!(id("witcher").unwrap_err().contains("several"));
        assert!(id("portal").unwrap_err().contains("no installation"));
    }

    #[test]
    fn a_book_is_named_after_its_storefront_and_account() {
        assert_eq!(
            account("gog-installations-48756d65.json"),
            Some(("gog", "Hume".to_owned()))
        );
        assert_eq!(
            account("steam-installations-7.json"),
            Some(("steam", "7".to_owned()))
        );
        for name in [
            "gog-installations-48756d65.incomplete.json",
            "epic-installations-4.json",
            "epic-installations-.json",
            "steam-installations-x.json",
            "itch-installations-48.json",
            "library-cache.json",
        ] {
            assert_eq!(account(name), None, "{name}");
        }
    }
}
