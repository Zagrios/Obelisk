use super::*;
use serde::Deserialize;
use std::sync::Mutex;
use tempfile::TempDir;

/// A storefront these tests script. It names a game `shelf/title`, tells
/// playtimes by title, in minutes, and achievements by shelf, so each rule
/// shows apart from Epic's and GOG's own.
struct Scripted;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
struct ScriptedGame {
    shelf: String,
    title: String,
}

#[derive(Default)]
struct ScriptedClient {
    games: Vec<ScriptedGame>,
    /// Minutes, by title.
    playtimes: BTreeMap<String, u64>,
    /// The shelves whose achievements the storefront does not answer for.
    failing: &'static [&'static str],
    /// Every request the session made, in order.
    asked: Mutex<Vec<String>>,
}

impl ScriptedClient {
    fn ask(&self, request: String) {
        self.asked.lock().unwrap().push(request);
    }
}

impl Rotating for ScriptedClient {
    fn revision(&self) -> u64 {
        1
    }
    async fn next(&self, _: u64) -> Option<u64> {
        None
    }
}

impl WebStore for Scripted {
    type Client = ScriptedClient;
    type Game = ScriptedGame;
    type Product = String;
    type Listed = ScriptedGame;
    type Shown = ScriptedGame;

    const CACHE: &'static str = "scripted";
    const PLAYTIME: PlaytimeUnit = PlaytimeUnit::MINUTES;
    const ACHIEVEMENT_READS: usize = 2;

    fn named(name: &str) -> Option<ScriptedGame> {
        let (shelf, title) = name.split_once('/')?;
        Some(game(shelf, title))
    }
    fn product(offer: GameOffer) -> Option<String> {
        match offer {
            GameOffer::Gog { product_id, .. } => Some(product_id),
            _ => None,
        }
    }
    fn playtime_id(game: &ScriptedGame) -> &str {
        &game.title
    }
    fn achievements_id(game: &ScriptedGame) -> &str {
        &game.shelf
    }
    fn shown(listed: ScriptedGame) -> (ScriptedGame, Value) {
        (listed, serde_json::json!({"returned": "everything else"}))
    }
    fn id(game: &ScriptedGame) -> String {
        format!("{}/{}", game.shelf, game.title)
    }

    async fn sign_in(_: &str) -> Result<ScriptedClient, WebAccountError> {
        Err(WebAccountError::InvalidCode)
    }
    async fn restore(_: WebAccountCredentials) -> Result<ScriptedClient, WebAccountError> {
        Err(WebAccountError::Unauthorized)
    }
    async fn credentials(_: &ScriptedClient) -> Result<WebAccountCredentials, WebAccountError> {
        Ok(WebAccountCredentials {
            account_id: "account".into(),
            name: "Scripted".into(),
            refresh_token: "fixture-only-never-sent".into(),
        })
    }
    async fn games(client: &ScriptedClient) -> Result<Vec<ScriptedGame>, WebAccountError> {
        client.ask("games".into());
        Ok(client.games.clone())
    }
    async fn playtimes(
        client: &ScriptedClient,
        games: &[ScriptedGame],
    ) -> Result<BTreeMap<String, u64>, WebAccountError> {
        client.ask(format!("playtimes of {} games", games.len()));
        Ok(client.playtimes.clone())
    }
    async fn achievements(
        client: &ScriptedClient,
        id: &str,
        locale: &str,
    ) -> Result<Vec<LibraryAchievement>, WebAccountError> {
        client.ask(format!("achievements {id} {locale}"));
        if client.failing.contains(&id) {
            return Err(WebAccountError::Network);
        }
        // The shelf "a" unlocked one of two, any other its only one.
        let unlocked: &[bool] = if id == "a" { &[true, false] } else { &[true] };
        Ok(unlocked
            .iter()
            .enumerate()
            .map(|(index, unlocked)| LibraryAchievement {
                id: index.to_string(),
                name: format!("Achievement {index} of {id}"),
                description: String::new(),
                icon_url: String::new(),
                hidden: false,
                unlocked: *unlocked,
                unlocked_at: None,
            })
            .collect())
    }
    async fn details(
        client: &ScriptedClient,
        product: &String,
        language: &str,
    ) -> Result<WebGameDetails, WebAccountError> {
        client.ask(format!("details {product} {language}"));
        Ok(WebGameDetails {
            platform: Some(GamePlatform::Windows),
            both_builds: false,
            installation_bytes: Some(7),
        })
    }
    async fn disconnect(_: &ScriptedClient) -> Result<(), WebAccountError> {
        Ok(())
    }
    fn shutdown_observers(_: &ScriptedClient) {}
}

fn game(shelf: &str, title: &str) -> ScriptedGame {
    ScriptedGame {
        shelf: shelf.into(),
        title: title.into(),
    }
}

fn names(names: &[&str]) -> Vec<String> {
    names.iter().map(|name| (*name).to_owned()).collect()
}

async fn session(client: ScriptedClient, library: Option<Arc<LibraryCache>>) -> Session<Scripted> {
    Session::with(client, library).await.unwrap()
}

fn asked(session: &Session<Scripted>) -> Vec<String> {
    session.client.asked.lock().unwrap().clone()
}

#[tokio::test]
async fn a_session_keeps_the_library_it_lists_and_tells_playtimes_in_seconds() {
    let directory = TempDir::new().unwrap();
    let cache = LibraryCache::new(directory.path().to_str().unwrap().to_owned());
    let listed = vec![game("a", "one"), game("b", "one"), game("a", "two")];
    let session = session(
        ScriptedClient {
            games: listed.clone(),
            playtimes: BTreeMap::from([("one".into(), 90), ("unlisted".into(), 5)]),
            ..Default::default()
        },
        Some(cache.clone()),
    )
    .await;

    assert_eq!(session.games().await.unwrap(), listed);
    let played = session
        .playtimes(names(&["a/one", "b/one", "a/two", "names no game"]))
        .await
        .unwrap();
    assert_eq!(
        played,
        HashMap::from([("a/one".into(), 5400), ("b/one".into(), 5400)]),
        "minutes cross as seconds, for every game the storefront tells by one name"
    );
    assert_eq!(asked(&session), ["games", "playtimes of 3 games"]);

    cache.flush();
    let file = std::fs::read(
        directory
            .path()
            .join(obelisk_core::library_cache::FILE_NAME),
    )
    .unwrap();
    let file: Value = serde_json::from_slice(&file).unwrap();
    let kept = &file["scripted"]["account"];
    assert_eq!(kept[0]["id"], "a/one");
    assert_eq!(
        kept[0]["playtimeMinutes"], 90,
        "the file keeps what the storefront told, in its unit"
    );
    assert_eq!(kept[1]["playtimeMinutes"], 90);
    assert!(kept[2].get("playtimeMinutes").is_none(), "never played");
    assert_eq!(kept[2]["metadata"]["returned"], "everything else");
    assert_eq!(ScriptedGame::deserialize(&kept[2]).unwrap(), listed[2]);
}

#[tokio::test]
async fn achievements_are_read_by_the_name_the_storefront_reads_them_by_once_each() {
    let session = session(
        ScriptedClient {
            failing: &["c"],
            ..Default::default()
        },
        None,
    )
    .await;

    let shares = session
        .achievement_shares(names(&["a/one", "b/one", "a/two", "c/one"]), "fr")
        .await
        .unwrap();
    assert_eq!(
        shares,
        HashMap::from([
            ("a/one".into(), 0.5),
            ("a/two".into(), 0.5),
            ("b/one".into(), 1.0),
        ]),
        "every game of a shelf shows its share, a failed shelf's none"
    );
    let mut reads = asked(&session);
    reads.sort();
    assert_eq!(
        reads,
        [
            "achievements a fr",
            "achievements b fr",
            "achievements c fr"
        ]
    );

    let list = session.achievements("a/two", "fr", true).await.unwrap();
    assert_eq!(list.len(), 2);
    assert_eq!(asked(&session).last().unwrap(), "achievements a fr");
    assert_eq!(
        session.achievements("names no game", "fr", false).await,
        Err(WebAccountError::InvalidResponse)
    );
    assert_eq!(
        session.achievement_shares(names(&["c/one"]), "fr").await,
        Err(WebAccountError::Network),
        "it fails once no game could be told"
    );
}

#[tokio::test]
async fn details_ask_for_the_product_an_offer_names_and_refuse_another_storefronts() {
    let session = session(ScriptedClient::default(), None).await;
    let details = session
        .details(
            GameOffer::Gog {
                product_id: "1207658924".into(),
                title: "Fixture".into(),
            },
            "fr-FR",
        )
        .await
        .unwrap();
    assert_eq!(details.installation_bytes, Some(7));
    assert_eq!(asked(&session), ["details 1207658924 fr-FR"]);

    let steam = GameOffer::Steam {
        app_id: 620,
        name: "Portal 2".into(),
        language: "french".into(),
    };
    assert_eq!(
        session.details(steam, "fr-FR").await,
        Err(WebAccountError::InvalidResponse)
    );
    assert_eq!(asked(&session).len(), 1, "the storefront is not asked");
}

#[test]
fn epic_tells_playtimes_by_app_name_in_seconds_and_achievements_by_sandbox() {
    let (names, games) = named::<epic::Epic>(names(&[
        "first:shared",
        "second:shared",
        "first:other",
        "no-colon",
    ]));
    assert_eq!(names, ["first:shared", "second:shared", "first:other"]);
    let told = BTreeMap::from([("shared".into(), 600), ("unlisted".into(), 5)]);
    assert_eq!(
        played::<epic::Epic>(names, &games, &told),
        HashMap::from([("first:shared".into(), 600), ("second:shared".into(), 600)]),
        "Epic counts an app name's time for each of its entries, in seconds"
    );
    assert_eq!(achievement_ids::<epic::Epic>(&games), ["first", "second"]);
}

#[test]
fn gog_tells_playtimes_and_achievements_by_product_and_counts_minutes() {
    let (names, games) = named::<gog::Gog>(names(&["1", "2", "3", "../4"]));
    assert_eq!(names, ["1", "2", "3"]);
    let told = BTreeMap::from([("2".into(), 600), ("3".into(), u64::MAX)]);
    assert_eq!(
        played::<gog::Gog>(names, &games, &told),
        HashMap::from([("2".into(), 36_000), ("3".into(), u64::MAX)]),
        "minutes cross as seconds, and an absurd count saturates"
    );
    assert_eq!(achievement_ids::<gog::Gog>(&games), ["1", "2", "3"]);
}

#[test]
fn a_kits_failure_crosses_as_the_one_swift_tells_apart() {
    use epic_kit::EpicError;
    use gog_kit::GogError;
    let service = |message: &str| WebAccountError::Service {
        message: message.into(),
    };
    for (epic, gog, crossed) in [
        (
            EpicError::Network,
            GogError::Network,
            WebAccountError::Network,
        ),
        (
            EpicError::Unauthorized,
            GogError::Unauthorized,
            WebAccountError::Unauthorized,
        ),
        (
            EpicError::InvalidCode,
            GogError::InvalidCode,
            WebAccountError::InvalidCode,
        ),
        (
            EpicError::Protocol,
            GogError::Protocol,
            WebAccountError::InvalidResponse,
        ),
        (
            EpicError::Service {
                message: "said".into(),
            },
            GogError::Service {
                message: "said".into(),
            },
            service("said"),
        ),
    ] {
        assert_eq!(WebAccountError::from(epic), crossed);
        assert_eq!(WebAccountError::from(gog), crossed);
    }
    // Any other failure crosses under its own localization key.
    assert_eq!(
        WebAccountError::from(EpicError::Cancelled),
        service(&EpicError::Cancelled.to_string())
    );
    assert_eq!(
        WebAccountError::from(GogError::Cancelled),
        service(&GogError::Cancelled.to_string())
    );
}

#[test]
fn credentials_cross_under_one_record_whatever_the_kit_calls_them() {
    let crossed = WebAccountCredentials {
        account_id: "id".into(),
        name: "Name".into(),
        refresh_token: "fixture-only-never-sent".into(),
    };
    let epic = epic_kit::auth::Credentials::from(crossed.clone());
    assert_eq!(
        (epic.account_id.as_str(), epic.display_name.as_str()),
        ("id", "Name")
    );
    let gog = gog_kit::auth::Credentials::from(crossed);
    assert_eq!(
        (gog.user_id.as_str(), gog.username.as_str()),
        ("id", "Name")
    );
    for back in [
        WebAccountCredentials::from(epic),
        WebAccountCredentials::from(gog),
    ] {
        assert_eq!(
            (
                back.account_id.as_str(),
                back.name.as_str(),
                back.refresh_token.as_str()
            ),
            ("id", "Name", "fixture-only-never-sent")
        );
    }
}

#[tokio::test]
async fn steam_signs_in_through_its_own_session() {
    let refused = WebAccountSession::sign_in(SessionStorefront::Steam, "code".into(), None).await;
    assert!(matches!(refused, Err(WebAccountError::Service { .. })));
}
