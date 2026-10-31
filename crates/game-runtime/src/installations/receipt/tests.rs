//! The records every storefront's installations share, and what each
//! storefront adds to them.
mod wine_prefix;

use super::*;
use crate::{
    epic::{self, Epic},
    gog::{self, Gog},
    steam::{self, Steam},
};

/// Where Obelisk installed each game before: an update stays there.
const PREVIOUS: &str = "/library/Previous location";

/// Portal 2's installation of `platform`, with its English depots.
fn portal(platform: Platform) -> Receipt<Steam> {
    Receipt {
        product: steam::Product {
            app_id: 620,
            language: "english".into(),
        },
        name: "Portal 2".into(),
        directory: format!(
            "{PREVIOUS}/{}",
            steam_kit::client::installation_directory_name(620, platform)
        ),
        platform,
        total_bytes: 7,
        needs_verification: false,
    }
}

/// An Epic game's installation of `platform`, with its catalog item.
fn sandbox_game(platform: Platform) -> Receipt<Epic> {
    Receipt {
        product: epic::Product {
            namespace: "sandbox".into(),
            catalog_item_id: "catalog".into(),
            app_name: "game".into(),
        },
        name: "Fixture".into(),
        directory: format!(
            "{PREVIOUS}/epic-hash-{}",
            epic_kit::client::PLATFORM.name(platform)
        ),
        platform,
        total_bytes: 7,
        needs_verification: false,
    }
}

/// The Witcher's installation of `platform`.
fn witcher(platform: Platform) -> Receipt<Gog> {
    Receipt {
        product: gog::Product {
            product_id: "1207658924".into(),
        },
        name: "The Witcher".into(),
        directory: format!(
            "{PREVIOUS}/{}",
            gog_kit::client::installation_directory_name("1207658924", platform)
        ),
        platform,
        total_bytes: 7,
        needs_verification: false,
    }
}

/// The receipts `file` holds, which save back to the same keys.
fn kept<S: Storefront>(file: serde_json::Value) -> Vec<Receipt<S>> {
    let receipts: Vec<Receipt<S>> = serde_json::from_value(file.clone()).unwrap();
    assert_eq!(serde_json::to_value(&receipts).unwrap(), file);
    receipts
}

/// The receipt `written` holds, which saves back as `saved`: in its own
/// storefront's spelling, whichever spelling it was read in.
fn respelled<S: Storefront>(written: serde_json::Value, saved: serde_json::Value) -> Receipt<S> {
    let receipt: Receipt<S> = serde_json::from_value(written).unwrap();
    assert_eq!(serde_json::to_value(&receipt).unwrap(), saved);
    receipt
}

#[test]
fn each_storefronts_receipt_file_keeps_its_own_keys() {
    let steam: Vec<Receipt<Steam>> = kept(serde_json::json!([
        {
            "appId": 620,
            "name": "Portal 2",
            "directory": "/library/Games/Steam/steam-620-macos",
            "platform": "macos",
            "language": "french",
            "totalBytes": 100,
            "needsVerification": true
        },
        {
            "appId": 620,
            "name": "Portal 2",
            "directory": "/library/Games/Steam/steam-620-windows",
            "platform": "windows",
            "language": "french",
            "totalBytes": 100
        }
    ]));
    assert!(steam[0].needs_verification && !steam[1].needs_verification);
    assert_eq!(steam[0].platform, Platform::Mac);
    assert_eq!(steam[1].platform, Platform::Windows);
    let epic: Vec<Receipt<Epic>> = kept(serde_json::json!([
        {
            "namespace": "sandbox",
            "catalogItemId": "catalog",
            "appName": "game",
            "name": "Fixture",
            "directory": "/library/Games/Epic/epic-hash-Windows",
            "platform": "Windows",
            "totalBytes": 7
        },
        {
            "namespace": "sandbox",
            "catalogItemId": "catalog",
            "appName": "game",
            "name": "Fixture",
            "directory": "/library/Games/Epic/epic-hash-Mac",
            "platform": "Mac",
            "totalBytes": 7
        }
    ]));
    assert_eq!(epic[0].product.catalog_item_id, "catalog");
    assert_eq!(epic[0].platform, Platform::Windows);
    assert_eq!(epic[1].platform, Platform::Mac);
    let gog: Vec<Receipt<Gog>> = kept(serde_json::json!([
        {
            "productId": "1207658924",
            "name": "The Witcher",
            "directory": "/library/Games/GOG/gog-1207658924-windows",
            "platform": "windows",
            "totalBytes": 7
        },
        {
            "productId": "1207658924",
            "name": "The Witcher",
            "directory": "/library/Games/GOG/gog-1207658924-macos",
            "platform": "macos",
            "totalBytes": 7
        }
    ]));
    assert_eq!(gog[0].product.product_id, "1207658924");
    assert_eq!(gog[0].platform, Platform::Windows);
    assert_eq!(gog[1].platform, Platform::Mac);

    // Another storefront's spelling reads as the same build, and saves back
    // in the receipt's own storefront's.
    let steam = |platform| {
        serde_json::json!({
            "appId": 620,
            "name": "Portal 2",
            "directory": "/library/Games/Steam/steam-620-windows",
            "platform": platform,
            "language": "french",
            "totalBytes": 100
        })
    };
    let receipt = respelled::<Steam>(steam("Windows"), steam("windows"));
    assert_eq!(receipt.platform, Platform::Windows);
    let epic = |platform| {
        serde_json::json!({
            "namespace": "sandbox",
            "catalogItemId": "catalog",
            "appName": "game",
            "name": "Fixture",
            "directory": "/library/Games/Epic/epic-hash-Mac",
            "platform": platform,
            "totalBytes": 7
        })
    };
    let receipt = respelled::<Epic>(epic("macos"), epic("Mac"));
    assert_eq!(receipt.platform, Platform::Mac);
    let gog = |platform| {
        serde_json::json!({
            "productId": "1207658924",
            "name": "The Witcher",
            "directory": "/library/Games/GOG/gog-1207658924-windows",
            "platform": platform,
            "totalBytes": 7
        })
    };
    let receipt = respelled::<Gog>(gog("Windows"), gog("windows"));
    assert_eq!(receipt.platform, Platform::Windows);

    // An older Epic file has no catalog item at all.
    let older: Vec<Receipt<Epic>> = serde_json::from_value(serde_json::json!([{
        "namespace": "sandbox",
        "appName": "game",
        "name": "Fixture",
        "directory": "/library/Games/Epic/epic-hash-Mac",
        "platform": "Mac",
        "totalBytes": 7
    }]))
    .unwrap();
    assert_eq!(older[0].product.catalog_item_id, "");
    assert_eq!(older[0].platform, Platform::Mac);
}

#[test]
fn receipts_name_one_installation_per_game_and_platform() {
    assert_eq!(portal(Platform::Mac).id(), "620-macos");
    assert_eq!(sandbox_game(Platform::Windows).id(), "sandbox:game:Windows");
    assert_eq!(witcher(Platform::Windows).id(), "1207658924:windows");
    assert_eq!(portal(Platform::Mac).game(), 620);
    assert_eq!(
        sandbox_game(Platform::Windows).game(),
        epic::Store {
            namespace: "sandbox".into(),
            app_name: "game".into(),
        }
    );
    assert_eq!(witcher(Platform::Windows).game(), "1207658924");
}

/// `receipt`'s update is `expected`, which goes over it.
fn updates<S: Storefront>(receipt: &Receipt<S>, expected: Transfer<S>) {
    assert_eq!(receipt.transfer(), expected);
    assert!(expected.lands_on(receipt));
}

#[test]
fn an_update_goes_over_its_installation_in_the_folder_it_sits_in() {
    updates(
        &portal(Platform::Mac),
        Transfer {
            product: steam::Product {
                app_id: 620,
                language: "english".into(),
            },
            name: "Portal 2".into(),
            platform: Some(Platform::Mac),
            directory: PREVIOUS.into(),
        },
    );
    updates(
        &sandbox_game(Platform::Windows),
        Transfer {
            product: epic::Product {
                namespace: "sandbox".into(),
                catalog_item_id: "catalog".into(),
                app_name: "game".into(),
            },
            name: "Fixture".into(),
            platform: Some(Platform::Windows),
            directory: PREVIOUS.into(),
        },
    );
    updates(
        &witcher(Platform::Windows),
        Transfer {
            product: gog::Product {
                product_id: "1207658924".into(),
            },
            name: "The Witcher".into(),
            platform: Some(Platform::Windows),
            directory: PREVIOUS.into(),
        },
    );
}

#[test]
fn a_transfer_lands_on_its_games_installation_of_its_platform_or_any_while_the_storefront_picks() {
    let (mac, windows) = (portal(Platform::Mac), portal(Platform::Windows));
    let picked = Transfer {
        platform: None,
        ..mac.transfer()
    };
    assert!(picked.lands_on(&mac) && picked.lands_on(&windows));
    let chosen = windows.transfer();
    assert!(chosen.lands_on(&windows) && !chosen.lands_on(&mac));
    let mut other = picked.clone();
    other.product.app_id = 730;
    assert!(!other.lands_on(&mac) && !other.lands_on(&windows));

    let (mac, windows) = (witcher(Platform::Mac), witcher(Platform::Windows));
    let chosen = windows.transfer();
    assert!(chosen.lands_on(&windows) && !chosen.lands_on(&mac));
    let mut other = chosen.clone();
    other.product.product_id = "1495134320".into();
    assert!(!other.lands_on(&windows) && !other.lands_on(&mac));

    // A new installation's Transfer starts from an offer, whose build the
    // storefront picks.
    let picked = Offer::<Steam> {
        product: portal(Platform::Mac).product,
        name: "Portal 2".into(),
    }
    .transfer("/library/Games/Steam".into());
    assert!(picked.lands_on(&portal(Platform::Mac)) && picked.lands_on(&portal(Platform::Windows)));
}

/// Uninstalls `windows` and `mac`, two builds of one game, whose folders are
/// gone: the Windows build's prefix goes with it.
fn removes_its_prefix<S: Storefront>(windows: Receipt<S>, mac: Receipt<S>) {
    let (_directory, root) = crate::wine::tests::installation();
    let locations = crate::wine::tests::locations(&root);
    let folder = Path::new(&windows.directory).file_name().unwrap();
    let prefix = locations.wine_prefixes.join(folder);
    std::fs::create_dir_all(prefix.join("drive_c")).unwrap();

    assert_eq!(windows.remove_prefix(&locations), Some(Ok(())));
    assert!(!prefix.exists());
    assert_eq!(mac.remove_prefix(&locations), None);
}

#[test]
fn an_uninstalled_windows_build_loses_its_prefix_and_a_mac_build_has_none() {
    removes_its_prefix(portal(Platform::Windows), portal(Platform::Mac));
    removes_its_prefix(sandbox_game(Platform::Windows), sandbox_game(Platform::Mac));
    removes_its_prefix(witcher(Platform::Windows), witcher(Platform::Mac));
}

/// Portal 2's Windows receipt, as Steam's app `app_id`.
fn steam_app(app_id: u32) -> Receipt<Steam> {
    let portal = portal(Platform::Windows);
    Receipt {
        product: steam::Product {
            app_id,
            ..portal.product.clone()
        },
        ..portal
    }
}

#[test]
fn a_windows_receipt_hands_its_profile_s_minimum_voile_to_its_launch() {
    let (_directory, root) = crate::wine::tests::installation();
    let locations = crate::wine::tests::locations(&root);
    let minimum_voile = |receipt: Receipt<Steam>| {
        let windows = receipt.windows(&locations).unwrap().unwrap();
        windows.minimum_voile()
    };

    assert_eq!(minimum_voile(steam_app(252950)), Some("v26.3.0-18"));
    assert_eq!(minimum_voile(portal(Platform::Windows)), None);
}
