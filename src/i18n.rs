//! Språkval: svenska eller engelska.
//!
//! Språket är en global inställning som både gränssnittet och bakgrundstråden
//! läser. Inget språkberoende sparas i den delade mappen: händelsefilerna
//! innehåller bara data, så lagmedlemmar kan använda olika språk.

use std::sync::atomic::{AtomicU8, Ordering};

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Lang {
    /// Engelska.
    En,
    /// Svenska (även för okända värden i konfigurationsfilen).
    #[default]
    #[serde(other)]
    Sv,
}

impl Lang {
    pub const ALL: [Lang; 2] = [Lang::Sv, Lang::En];

    /// Språkets eget namn, visas likadant oavsett valt språk.
    pub fn name(self) -> &'static str {
        match self {
            Lang::Sv => "Svenska",
            Lang::En => "English",
        }
    }
}

static CURRENT: AtomicU8 = AtomicU8::new(0);

#[cfg(test)]
thread_local! {
    static OVERRIDE: std::cell::Cell<Option<Lang>> = const { std::cell::Cell::new(None) };
}

/// Byter språk för hela appen.
pub fn set(lang: Lang) {
    CURRENT.store(u8::from(lang == Lang::En), Ordering::Relaxed);
}

pub fn lang() -> Lang {
    #[cfg(test)]
    if let Some(lang) = OVERRIDE.with(|o| o.get()) {
        return lang;
    }
    if CURRENT.load(Ordering::Relaxed) == 1 {
        Lang::En
    } else {
        Lang::Sv
    }
}

pub fn is_en() -> bool {
    lang() == Lang::En
}

/// Väljer text efter aktuellt språk.
pub fn t(sv: &'static str, en: &'static str) -> &'static str {
    match lang() {
        Lang::Sv => sv,
        Lang::En => en,
    }
}

/// Kör `f` med ett visst språk i den här testtråden, utan att störa andra tester.
#[cfg(test)]
pub fn with_lang<R>(lang: Lang, f: impl FnOnce() -> R) -> R {
    OVERRIDE.with(|o| o.set(Some(lang)));
    let result = f();
    OVERRIDE.with(|o| o.set(None));
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn swedish_is_default_and_english_can_be_selected() {
        assert_eq!(t("Köad", "Queued"), "Köad");
        with_lang(Lang::En, || assert_eq!(t("Köad", "Queued"), "Queued"));
        assert_eq!(t("Köad", "Queued"), "Köad");
    }

    #[test]
    fn config_values() {
        assert_eq!(serde_json::to_string(&Lang::En).unwrap(), "\"en\"");
        assert_eq!(serde_json::from_str::<Lang>("\"sv\"").unwrap(), Lang::Sv);
        assert_eq!(serde_json::from_str::<Lang>("\"en\"").unwrap(), Lang::En);
        // Okända språk ger svenska i stället för att hela konfigurationen kastas.
        assert_eq!(serde_json::from_str::<Lang>("\"de\"").unwrap(), Lang::Sv);
    }
}
