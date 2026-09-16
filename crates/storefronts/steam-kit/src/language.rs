//! Host BCP-47 locale to Steam depot language code.
//! https://partner.steamgames.com/doc/store/localization/languages

const SIMPLE: &[(&str, &str)] = &[
    ("ar", "arabic"),
    ("bg", "bulgarian"),
    ("cs", "czech"),
    ("da", "danish"),
    ("nl", "dutch"),
    ("en", "english"),
    ("fi", "finnish"),
    ("fr", "french"),
    ("de", "german"),
    ("el", "greek"),
    ("hu", "hungarian"),
    ("id", "indonesian"),
    ("it", "italian"),
    ("ja", "japanese"),
    ("ko", "koreana"),
    ("ms", "malay"),
    ("no", "norwegian"),
    ("nb", "norwegian"),
    ("nn", "norwegian"),
    ("pl", "polish"),
    ("ro", "romanian"),
    ("ru", "russian"),
    ("sv", "swedish"),
    ("th", "thai"),
    ("tr", "turkish"),
    ("uk", "ukrainian"),
    ("vi", "vietnamese"),
];

pub fn from_locale(locale: &str) -> &'static str {
    let normalized = locale.to_ascii_lowercase().replace('_', "-");
    let parts: Vec<_> = normalized.split(['-', '.', '@']).collect();
    match parts.first().copied().unwrap_or_default() {
        "zh" if parts.contains(&"hans") => "schinese",
        "zh" if parts
            .iter()
            .any(|part| matches!(*part, "hant" | "tw" | "hk" | "mo")) =>
        {
            "tchinese"
        }
        "zh" => "schinese",
        "pt" if parts.contains(&"br") => "brazilian",
        "pt" => "portuguese",
        "es" if parts.iter().any(|part| {
            matches!(
                *part,
                "419"
                    | "ar"
                    | "bo"
                    | "bz"
                    | "cl"
                    | "co"
                    | "cr"
                    | "cu"
                    | "do"
                    | "ec"
                    | "gt"
                    | "hn"
                    | "mx"
                    | "ni"
                    | "pa"
                    | "pe"
                    | "pr"
                    | "py"
                    | "sv"
                    | "us"
                    | "uy"
                    | "ve"
            )
        }) =>
        {
            "latam"
        }
        "es" => "spanish",
        language => SIMPLE
            .iter()
            .find(|(code, _)| *code == language)
            .map_or("english", |(_, steam)| *steam),
    }
}

pub fn is_supported(language: &str) -> bool {
    SIMPLE.iter().any(|(_, value)| *value == language)
        || matches!(
            language,
            "schinese" | "tchinese" | "portuguese" | "brazilian" | "spanish" | "latam"
        )
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn system_locales_keep_steam_regional_and_script_variants() {
        for (locale, expected) in [
            ("fr-FR", "french"),
            ("fr_CA", "french"),
            ("en-GB", "english"),
            ("de-DE", "german"),
            ("pt-BR", "brazilian"),
            ("pt-PT", "portuguese"),
            ("es-MX", "latam"),
            ("es-419", "latam"),
            ("es-ES", "spanish"),
            ("zh-Hant-HK", "tchinese"),
            ("zh-TW", "tchinese"),
            ("zh-Hans-TW", "schinese"),
            ("zh-CN", "schinese"),
            ("ko-KR", "koreana"),
            ("nb-NO", "norwegian"),
            ("uk-UA", "ukrainian"),
            ("ms-MY", "malay"),
            ("id-ID", "indonesian"),
            ("", "english"),
            ("xx-XX", "english"),
        ] {
            assert_eq!(from_locale(locale), expected, "{locale}");
            assert!(is_supported(from_locale(locale)));
        }
        assert!(!is_supported("fr-FR"));
    }
}
