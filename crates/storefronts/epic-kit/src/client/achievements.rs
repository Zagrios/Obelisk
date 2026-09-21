//! Read-only launcher GraphQL queries, matching Legendary's Epic API contract.
use super::*;
use serde_json::{Value, json};
use std::collections::HashMap;

/// The store GraphQL endpoint answers the launcher component, not the client
/// default in `auth`, so this request identifies itself separately.
const LAUNCHER_AGENT: &str = "EpicGamesLauncher/14.0.8-22004686+++Portal+Release-Live";
const DEFINITIONS: &str = r#"query Achievement($sandboxId: String!, $locale: String!) {
  Achievement { productAchievementsRecordBySandbox(sandboxId: $sandboxId, locale: $locale) {
    sandboxId totalAchievements achievements { achievement {
      name hidden unlockedDisplayName lockedDisplayName unlockedDescription lockedDescription
      unlockedIconLink lockedIconLink
    } }
  } }
}"#;
const PLAYER: &str = r#"query PlayerAchievement($epicAccountId: String!, $sandboxId: String!) {
  PlayerAchievement { playerAchievementGameRecordsBySandbox(epicAccountId: $epicAccountId, sandboxId: $sandboxId) {
    records { playerAchievements { playerAchievement {
      sandboxId epicAccountId unlocked unlockDate achievementName
    } } }
  } }
}"#;

#[derive(Clone, Debug)]
pub struct Achievement {
    pub id: String,
    pub name: String,
    pub description: String,
    pub icon_url: String,
    pub hidden: bool,
    pub unlocked: bool,
    pub unlock_date: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Product {
    sandbox_id: String,
    total_achievements: usize,
    achievements: Vec<DefinitionRecord>,
}
#[derive(Deserialize)]
struct DefinitionRecord {
    achievement: Definition,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Definition {
    name: String,
    hidden: bool,
    unlocked_display_name: Option<String>,
    locked_display_name: Option<String>,
    unlocked_description: Option<String>,
    locked_description: Option<String>,
    unlocked_icon_link: Option<String>,
    locked_icon_link: Option<String>,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct PlayerRecord {
    player_achievements: Vec<UnlockRecord>,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct UnlockRecord {
    player_achievement: Unlock,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Unlock {
    sandbox_id: String,
    epic_account_id: String,
    achievement_name: String,
    unlocked: bool,
    unlock_date: Option<String>,
}

impl Client {
    pub async fn achievements(
        &self,
        namespace: &str,
        locale: &str,
    ) -> Result<Vec<Achievement>, EpicError> {
        let account = self.credentials().await?.account_id;
        let account = account.as_str();
        self.with_access_token(|token| async move {
            let response = query(
                &self.api,
                &token,
                DEFINITIONS,
                json!({"sandboxId":namespace,"locale":locale}),
            )
            .await?;
            let product = product(response, namespace)?;
            let Some(product) = product else {
                return Ok(Vec::new());
            };
            let response = query(
                &self.api,
                &token,
                PLAYER,
                json!({"sandboxId":namespace,"epicAccountId":account}),
            )
            .await?;
            combine(product, response, account)
        })
        .await
    }
}

async fn query(api: &Api, token: &str, query: &str, variables: Value) -> Result<Value, EpicError> {
    let value: Value = api
        .post_json(
            format!("{}/graphql", api.store),
            token,
            LAUNCHER_AGENT,
            &json!({"query":query,"variables":variables}),
        )
        .await?;
    // GraphQL can report denied or partial results with HTTP 200. Never show
    // these as zero unlocked achievements, nor log provider messages/tokens.
    if value
        .get("errors")
        .is_some_and(|errors| !errors.as_array().is_some_and(Vec::is_empty))
    {
        return Err(EpicError::Service {
            message: "backend.epic.achievements_unavailable".into(),
        });
    }
    Ok(value)
}

fn product(mut value: Value, namespace: &str) -> Result<Option<Product>, EpicError> {
    let record = value
        .pointer_mut("/data/Achievement/productAchievementsRecordBySandbox")
        .ok_or(EpicError::Protocol)?;
    if record.is_null() {
        return Ok(None);
    }
    let product: Product =
        serde_json::from_value(record.take()).map_err(|_| EpicError::Protocol)?;
    if product.sandbox_id != namespace || product.total_achievements != product.achievements.len() {
        return Err(EpicError::Protocol);
    }
    if product.achievements.is_empty() {
        return Ok(None);
    }
    Ok(Some(product))
}

fn combine(
    product: Product,
    mut value: Value,
    account: &str,
) -> Result<Vec<Achievement>, EpicError> {
    let records = value
        .pointer_mut("/data/PlayerAchievement/playerAchievementGameRecordsBySandbox/records")
        .ok_or(EpicError::Protocol)?;
    let records: Vec<PlayerRecord> =
        serde_json::from_value(records.take()).map_err(|_| EpicError::Protocol)?;
    // Same keyed join as the Steam side, which also hashes rather than orders.
    let mut unlocked = HashMap::new();
    for record in records {
        for record in record.player_achievements {
            let state = record.player_achievement;
            if state.sandbox_id != product.sandbox_id
                || state.epic_account_id != account
                || unlocked
                    .insert(state.achievement_name.clone(), state)
                    .is_some()
            {
                return Err(EpicError::Protocol);
            }
        }
    }
    let mut ids = HashSet::new();
    let mut result = Vec::new();
    for record in product.achievements {
        let definition = record.achievement;
        let id = definition.name;
        if id.is_empty() || !ids.insert(id.clone()) {
            return Err(EpicError::Protocol);
        }
        // Only an unlocked record carries a date, so drop the others here and
        // let the rest of the loop read `earned` off the remaining state.
        let state = unlocked.remove(&id).filter(|state| state.unlocked);
        let earned = state.is_some();
        let (name, description, icon) = if earned {
            (
                definition.unlocked_display_name,
                definition.unlocked_description,
                definition.unlocked_icon_link,
            )
        } else {
            (
                definition.locked_display_name,
                definition.locked_description,
                definition.locked_icon_link,
            )
        };
        // A hidden achievement the account has not earned exposes its identity
        // and nothing else, so its display text and icon are never resolved.
        let hidden = definition.hidden && !earned;
        let (name, description, icon_url) = if hidden {
            (String::new(), String::new(), String::new())
        } else {
            (
                name.filter(|name| !name.is_empty()).unwrap_or(id.clone()),
                description.unwrap_or_default(),
                icon.and_then(auth::safe_https_url).unwrap_or_default(),
            )
        };
        result.push(Achievement {
            id,
            name,
            description,
            icon_url,
            hidden,
            unlocked: earned,
            unlock_date: state.and_then(|state| state.unlock_date),
        });
    }
    if !unlocked.is_empty() {
        return Err(EpicError::Protocol);
    }
    result.sort_by(|a, b| {
        b.unlocked
            .cmp(&a.unlocked)
            .then_with(|| b.unlock_date.cmp(&a.unlock_date))
            .then_with(|| a.id.cmp(&b.id))
    });
    Ok(result)
}
