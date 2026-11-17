//! "New in this version": a short list of changes, shown once after an update.

use crate::i18n::t;
use crate::update::parse_version;

/// The changes of each version, newest first: (version, [(Swedish, English)]).
/// Only versions that have something worth telling the team are listed.
const NOTES: &[(&str, &[(&str, &str)])] = &[(
    "0.6.0",
    &[
        (
            "Fliken Arkiv: förfrågningar som blev klara för mer än 30 dagar sedan flyttas dit, så att de andra flikarna hålls korta. Inget tas bort.",
            "The Archive tab: requests that finished more than 30 days ago move there, so the other tabs stay short. Nothing is deleted.",
        ),
        (
            "Påminnelse om deadlines: en rad ovanför kön visar förfrågningar vars deadline har passerat eller är nära, och filtret Snart deadline visar bara dem.",
            "Deadline reminders: a line above the queue shows requests whose deadline has passed or is close, and the Due soon filter shows only those.",
        ),
        (
            "Historik för delar: välj Historik… i menyn för en del i översikten.",
            "History of a part: choose History… in the menu of a part in the overview.",
        ),
        (
            "Varning två veckor innan teamnyckeln går ut.",
            "A warning two weeks before the team key expires.",
        ),
        (
            "Områdena på bilen kan väljas med tangentbordet, och knapparna har namn för skärmläsare.",
            "The regions of the car can be chosen with the keyboard, and the buttons have names for screen readers.",
        ),
        (
            "Appen frågar GitHub mer sällan när fönstret ligger i bakgrunden.",
            "The app asks GitHub less often while the window is in the background.",
        ),
    ],
)];

/// The changes between `last_seen` (not included) and `current` (included),
/// newest version first. Nothing if the version has not changed.
pub fn since(last_seen: &str, current: &str) -> Vec<(&'static str, Vec<&'static str>)> {
    let (Some(seen), Some(current)) = (parse_version(last_seen), parse_version(current)) else {
        return Vec::new();
    };
    NOTES
        .iter()
        .filter(|(version, _)| parse_version(version).is_some_and(|v| v > seen && v <= current))
        .map(|(version, lines)| (*version, lines.iter().map(|(sv, en)| t(sv, en)).collect()))
        .collect()
}

/// The text of the "new in this version" window, or `None` if there is
/// nothing to tell.
pub fn text(last_seen: &str, current: &str) -> Option<String> {
    let notes = since(last_seen, current);
    if notes.is_empty() {
        return None;
    }
    let several = notes.len() > 1;
    let mut text = String::new();
    for (version, lines) in notes {
        if several {
            text.push_str(&format!("{} {version}\n", t("Version", "Version")));
        }
        for line in lines {
            text.push_str(&format!("•  {line}\n"));
        }
        text.push('\n');
    }
    Some(text.trim_end().to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::i18n::{with_lang, Lang};

    #[test]
    fn every_version_is_valid_and_newest_first() {
        let versions: Vec<_> = NOTES
            .iter()
            .map(|(v, _)| parse_version(v).expect("valid version"))
            .collect();
        assert!(versions.windows(2).all(|w| w[0] > w[1]));
        assert!(
            versions[0] <= parse_version(crate::update::current_version()).unwrap(),
            "notes for a version newer than Cargo.toml"
        );
    }

    #[test]
    fn only_versions_after_the_last_seen_are_shown() {
        assert_eq!(text("0.6.0", "0.6.0"), None);
        assert_eq!(text("0.7.0", "0.6.0"), None);
        assert_eq!(text("trasig", "0.6.0"), None);
        let shown = with_lang(Lang::En, || text("0.5.2", "0.6.0")).unwrap();
        assert!(shown.contains("Archive tab"));
        assert!(
            !shown.contains("Version 0.6.0"),
            "one version needs no heading"
        );
        // Notes for a version this app is not yet are never shown.
        assert_eq!(text("0.5.2", "0.5.9"), None);
    }
}
