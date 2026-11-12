//! Language choice: Swedish or English.
//!
//! The language is a global setting read by both the user interface and the
//! background thread. Nothing language-dependent is stored in the shared folder:
//! the event files contain only data, so team members can use different languages.

use std::sync::atomic::{AtomicU8, Ordering};

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Lang {
    /// English.
    En,
    /// Swedish (also for unknown values in the configuration file).
    #[default]
    #[serde(other)]
    Sv,
}

impl Lang {
    pub const ALL: [Lang; 2] = [Lang::Sv, Lang::En];

    /// The language's own name, shown the same regardless of the selected language.
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

/// Changes the language for the whole app.
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

/// Picks the text for the current language.
pub fn t(sv: &'static str, en: &'static str) -> &'static str {
    match lang() {
        Lang::Sv => sv,
        Lang::En => en,
    }
}

/// Runs `f` with a given language in this test thread, without disturbing other tests.
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
        // Unknown languages give Swedish instead of the whole configuration being discarded.
        assert_eq!(serde_json::from_str::<Lang>("\"de\"").unwrap(), Lang::Sv);
    }
}
