//! A Windows installation's launch settings across UniFFI: how its game runs
//! and what its player changed, with every engine's offer, so Swift shows the
//! Engine, CPU and GPU choices each engine allows without asking again.
use game_runtime::{
    launch_settings::{
        self, Choices, ENGINES, EngineOffer, GraphicsOffer, GraphicsOption, SteamClient,
        Translator, View,
    },
    wine::{Engine, Graphics},
};

/// How a Windows game runs from its next launch, and what its player can
/// change.
#[derive(Clone, Debug, PartialEq, Eq, uniffi::Record)]
pub struct LaunchSettings {
    pub engine: LaunchEngine,
    pub graphics: LaunchGraphics,
    /// Direct3D 12 offers DirectX Raytracing, on D3DMetal.
    pub ray_tracing: bool,
    /// Metal's Performance HUD shows over the game, whatever renders it.
    pub performance_hud: bool,
    /// Which Steam client a Steam game starts through.
    pub steam_client: LaunchSteamClient,
    /// What the player changed from what Obelisk knows of the game.
    pub choices: LaunchChoices,
    /// The engine the game runs on without choices.
    pub default_engine: LaunchEngine,
    /// The Steam client a Steam game starts through on Rosetta's Wine
    /// without choices.
    pub default_steam_client: LaunchSteamClient,
    /// Every engine, with what it offers and how the game runs there
    /// without choices.
    pub engines: Vec<LaunchEngineOffer>,
}

/// What the player changed; a missing choice follows Obelisk's defaults.
#[derive(Clone, Debug, Default, PartialEq, Eq, uniffi::Record)]
pub struct LaunchChoices {
    /// What the player adds to the game's command line, as typed.
    #[uniffi(default = None)]
    pub arguments: Option<String>,
    #[uniffi(default = None)]
    pub engine: Option<LaunchEngine>,
    #[uniffi(default = None)]
    pub graphics: Option<LaunchGraphics>,
    #[uniffi(default = None)]
    pub ray_tracing: Option<bool>,
    #[uniffi(default = false)]
    pub performance_hud: bool,
    #[uniffi(default = None)]
    pub steam_client: Option<LaunchSteamClient>,
}

/// Which Steam client a Windows Steam game starts through, on Rosetta's
/// Wine.
#[derive(Clone, Copy, Debug, PartialEq, Eq, uniffi::Enum)]
pub enum LaunchSteamClient {
    /// Obelisk's own, through its Steam host.
    Obelisk,
    /// Valve's Windows Steam, in a prefix of its own: VAC-secured servers
    /// accept a Windows game it starts.
    Windows,
}

/// What runs a Windows game's Wine.
#[derive(Clone, Copy, Debug, PartialEq, Eq, uniffi::Enum)]
pub enum LaunchEngine {
    /// Wine for macOS, under Rosetta.
    Rosetta,
    /// Wine for Linux, in voile-lkd's engine.
    Linux,
}

/// What renders a Windows game's Direct3D.
#[derive(Clone, Copy, Debug, PartialEq, Eq, uniffi::Enum)]
pub enum LaunchGraphics {
    D3dMetal,
    Dxmt,
}

/// What translates a game's x86 code on one engine.
#[derive(Clone, Copy, Debug, PartialEq, Eq, uniffi::Enum)]
pub enum LaunchTranslator {
    Rosetta,
    Fex,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, uniffi::Enum)]
pub enum LaunchGraphicsOption {
    RayTracing,
}

/// One engine's offer.
#[derive(Clone, Debug, PartialEq, Eq, uniffi::Record)]
pub struct LaunchEngineOffer {
    pub engine: LaunchEngine,
    pub translator: LaunchTranslator,
    /// The backend the game renders with there without choices.
    pub default_graphics: LaunchGraphics,
    pub graphics: Vec<LaunchGraphicsOffer>,
}

/// One backend's offer on one engine.
#[derive(Clone, Debug, PartialEq, Eq, uniffi::Record)]
pub struct LaunchGraphicsOffer {
    pub graphics: LaunchGraphics,
    pub options: Vec<LaunchGraphicsOption>,
}

impl From<View> for LaunchSettings {
    fn from(view: View) -> Self {
        Self {
            engine: view.settings.engine.into(),
            graphics: view.settings.graphics.into(),
            ray_tracing: view.settings.options.ray_tracing,
            performance_hud: view.settings.performance_hud,
            steam_client: view.settings.steam_client.into(),
            choices: view.choices.into(),
            default_engine: view.default_engine.into(),
            default_steam_client: view.default_steam_client.into(),
            engines: ENGINES
                .iter()
                .zip(view.default_graphics)
                .map(|(offer, default)| engine_offer(offer, default))
                .collect(),
        }
    }
}

fn engine_offer(offer: &EngineOffer, default: Graphics) -> LaunchEngineOffer {
    LaunchEngineOffer {
        engine: offer.engine.into(),
        translator: match offer.translator {
            Translator::Rosetta => LaunchTranslator::Rosetta,
            Translator::Fex => LaunchTranslator::Fex,
        },
        default_graphics: default.into(),
        graphics: offer.graphics.iter().map(graphics_offer).collect(),
    }
}

fn graphics_offer(offer: &GraphicsOffer) -> LaunchGraphicsOffer {
    LaunchGraphicsOffer {
        graphics: offer.graphics.into(),
        options: offer
            .options
            .iter()
            .map(|option| match option {
                GraphicsOption::RayTracing => LaunchGraphicsOption::RayTracing,
            })
            .collect(),
    }
}

impl From<Choices> for LaunchChoices {
    fn from(choices: Choices) -> Self {
        Self {
            arguments: choices.arguments,
            engine: choices.engine.map(Into::into),
            graphics: choices.graphics.map(Into::into),
            ray_tracing: choices.ray_tracing,
            performance_hud: choices.performance_hud,
            steam_client: choices.steam_client.map(Into::into),
        }
    }
}

impl From<LaunchChoices> for launch_settings::Choices {
    fn from(choices: LaunchChoices) -> Self {
        Self {
            arguments: choices.arguments,
            engine: choices.engine.map(Into::into),
            graphics: choices.graphics.map(Into::into),
            ray_tracing: choices.ray_tracing,
            performance_hud: choices.performance_hud,
            steam_client: choices.steam_client.map(Into::into),
        }
    }
}

impl From<Engine> for LaunchEngine {
    fn from(engine: Engine) -> Self {
        match engine {
            Engine::Rosetta => Self::Rosetta,
            Engine::Linux => Self::Linux,
        }
    }
}

impl From<LaunchEngine> for Engine {
    fn from(engine: LaunchEngine) -> Self {
        match engine {
            LaunchEngine::Rosetta => Self::Rosetta,
            LaunchEngine::Linux => Self::Linux,
        }
    }
}

impl From<SteamClient> for LaunchSteamClient {
    fn from(client: SteamClient) -> Self {
        match client {
            SteamClient::Obelisk => Self::Obelisk,
            SteamClient::Windows => Self::Windows,
        }
    }
}

impl From<LaunchSteamClient> for SteamClient {
    fn from(client: LaunchSteamClient) -> Self {
        match client {
            LaunchSteamClient::Obelisk => Self::Obelisk,
            LaunchSteamClient::Windows => Self::Windows,
        }
    }
}

impl From<Graphics> for LaunchGraphics {
    fn from(graphics: Graphics) -> Self {
        match graphics {
            Graphics::D3DMetal => Self::D3dMetal,
            Graphics::Dxmt => Self::Dxmt,
        }
    }
}

impl From<LaunchGraphics> for Graphics {
    fn from(graphics: LaunchGraphics) -> Self {
        match graphics {
            LaunchGraphics::D3dMetal => Self::D3DMetal,
            LaunchGraphics::Dxmt => Self::Dxmt,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use game_runtime::profile;

    #[test]
    fn a_game_s_settings_cross_with_every_engine_s_offer_and_default_backend() {
        let view = launch_settings::of(
            std::path::Path::new("/nowhere"),
            profile::Game::Steam { app_id: 42 },
            std::path::Path::new("/nowhere"),
        );

        let settings = LaunchSettings::from(view);

        assert_eq!(settings.engine, LaunchEngine::Rosetta);
        assert_eq!(settings.graphics, LaunchGraphics::D3dMetal);
        assert!(settings.ray_tracing);
        assert!(!settings.performance_hud);
        assert_eq!(settings.steam_client, LaunchSteamClient::Obelisk);
        assert_eq!(settings.choices, LaunchChoices::default());
        let engines: Vec<_> = settings
            .engines
            .iter()
            .map(|offer| (offer.engine, offer.translator, offer.default_graphics))
            .collect();
        assert_eq!(
            engines,
            [
                (
                    LaunchEngine::Rosetta,
                    LaunchTranslator::Rosetta,
                    LaunchGraphics::D3dMetal
                ),
                (
                    LaunchEngine::Linux,
                    LaunchTranslator::Fex,
                    LaunchGraphics::Dxmt
                ),
            ]
        );
        assert_eq!(
            settings.engines[0].graphics[0].options,
            [LaunchGraphicsOption::RayTracing]
        );
    }

    #[test]
    fn a_choice_of_windows_steam_crosses_both_ways() {
        let choices = LaunchChoices {
            steam_client: Some(LaunchSteamClient::Windows),
            ..LaunchChoices::default()
        };

        let kept = Choices::from(choices.clone());

        assert_eq!(kept.steam_client, Some(SteamClient::Windows));
        assert_eq!(LaunchChoices::from(kept), choices);
    }
}
