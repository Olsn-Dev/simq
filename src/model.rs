//! Domain model: status, location kinds and the derived request.

use std::cmp::Ordering;

use uuid::Uuid;

use crate::i18n::t;

pub const MAX_TITLE: usize = 200;
pub const MAX_NAME: usize = 200;
pub const MAX_TEXT: usize = 4000;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Status {
    Queued,
    Running,
    Done,
    Failed,
    Cancelled,
}

impl Status {
    pub fn as_str(self) -> &'static str {
        match self {
            Status::Queued => "queued",
            Status::Running => "running",
            Status::Done => "done",
            Status::Failed => "failed",
            Status::Cancelled => "cancelled",
        }
    }

    /// Name shown in the user interface.
    pub fn label(self) -> &'static str {
        match self {
            Status::Queued => t("Köad", "Queued"),
            Status::Running => t("Pågår", "In progress"),
            Status::Done => t("Klar", "Done"),
            Status::Failed => t("Misslyckad", "Failed"),
            Status::Cancelled => t("Avbruten", "Cancelled"),
        }
    }

    pub fn is_active(self) -> bool {
        matches!(self, Status::Queued | Status::Running)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum OpKind {
    Create,
    Take,
    Finish,
    Fail,
    Cancel,
    Requeue,
}

impl OpKind {
    pub fn as_str(self) -> &'static str {
        match self {
            OpKind::Create => "create",
            OpKind::Take => "take",
            OpKind::Finish => "finish",
            OpKind::Fail => "fail",
            OpKind::Cancel => "cancel",
            OpKind::Requeue => "requeue",
        }
    }

    pub fn parse(s: &str) -> Option<OpKind> {
        Some(match s {
            "create" => OpKind::Create,
            "take" => OpKind::Take,
            "finish" => OpKind::Finish,
            "fail" => OpKind::Fail,
            "cancel" => OpKind::Cancel,
            "requeue" => OpKind::Requeue,
            _ => return None,
        })
    }

    /// "Anna {…}" – used in the history.
    pub fn label(self) -> &'static str {
        match self {
            OpKind::Create => t("Skapade ärendet", "Created the request"),
            OpKind::Take => t("Tog ärendet", "Took the request"),
            OpKind::Finish => t("Markerade ärendet som klart", "Marked the request as done"),
            OpKind::Fail => t(
                "Markerade ärendet som misslyckat",
                "Marked the request as failed",
            ),
            OpKind::Cancel => t("Avbröt ärendet", "Cancelled the request"),
            OpKind::Requeue => t("Köade om ärendet", "Requeued the request"),
        }
    }

    /// "Anna {…} before you" – used in the dialog when someone got there first.
    pub fn verb(self) -> &'static str {
        match self {
            OpKind::Create => t("skapade ärendet", "created the request"),
            OpKind::Take => t("tog ärendet", "took the request"),
            OpKind::Finish => t("markerade ärendet som klart", "marked the request as done"),
            OpKind::Fail => t(
                "markerade ärendet som misslyckat",
                "marked the request as failed",
            ),
            OpKind::Cancel => t("avbröt ärendet", "cancelled the request"),
            OpKind::Requeue => t("köade om ärendet", "requeued the request"),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum LocKind {
    #[default]
    Drive,
    Vault,
    Other,
}

impl LocKind {
    pub const ALL: [LocKind; 3] = [LocKind::Drive, LocKind::Vault, LocKind::Other];

    pub fn as_str(self) -> &'static str {
        match self {
            LocKind::Drive => "drive",
            LocKind::Vault => "vault",
            LocKind::Other => "other",
        }
    }

    pub fn parse(s: &str) -> Option<LocKind> {
        Some(match s {
            "drive" => LocKind::Drive,
            "vault" => LocKind::Vault,
            "other" => LocKind::Other,
            _ => return None,
        })
    }

    pub fn label(self) -> &'static str {
        match self {
            LocKind::Drive => "Google Drive",
            LocKind::Vault => "Vault",
            LocKind::Other => t("Annat", "Other"),
        }
    }

    pub fn placeholder(self) -> &'static str {
        match self {
            LocKind::Drive => "https://drive.google.com/…",
            LocKind::Vault => t(
                "t.ex. Uprights/v3/Assembly.iam",
                "e.g. Uprights/v3/Assembly.iam",
            ),
            LocKind::Other => t(
                "t.ex. USB-stickan i verkstadslådan",
                "e.g. the USB stick in the workshop drawer",
            ),
        }
    }

    pub fn help(self) -> &'static str {
        match self {
            LocKind::Drive => {
                t("Klistra in delningslänken från Google Drive. Den måste börja med http:// eller https://.", "Paste the sharing link from Google Drive. It must start with http:// or https://.")
            }
            LocKind::Vault => t("Beskriv kort var i Vault filen ligger.", "Briefly describe where in Vault the file is."),
            LocKind::Other => t("Fri text: beskriv var filen finns.", "Free text: describe where the file is."),
        }
    }
}

/// A location for a file: kind + value.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Location {
    pub kind: LocKind,
    pub value: String,
}

/// True only for links that may be opened in the browser: http:// or https://,
/// without whitespace or control characters. All other schemes (file:, javascript: …)
/// give false.
pub fn is_http_url(s: &str) -> bool {
    let rest = if let Some(rest) = strip_prefix_ignore_case(s, "https://") {
        rest
    } else if let Some(rest) = strip_prefix_ignore_case(s, "http://") {
        rest
    } else {
        return false;
    };
    !rest.is_empty() && !s.chars().any(|c| c.is_whitespace() || c.is_control())
}

fn strip_prefix_ignore_case<'a>(s: &'a str, prefix: &str) -> Option<&'a str> {
    let head = s.get(..prefix.len())?;
    if head.eq_ignore_ascii_case(prefix) {
        s.get(prefix.len()..)
    } else {
        None
    }
}

pub fn char_len(s: &str) -> usize {
    s.chars().count()
}

/// Validates a pair (kind, value) as it appears in an event file.
/// An empty value means "no location given".
pub fn parse_location(kind: &str, value: &str) -> Result<Option<Location>, String> {
    if char_len(value) > MAX_TEXT {
        return Err(t(
            "platsbeskrivningen är för lång",
            "the location description is too long",
        )
        .to_string());
    }
    if value.trim().is_empty() {
        return if kind.is_empty() || LocKind::parse(kind).is_some() {
            Ok(None)
        } else {
            Err(format!(
                "{} \"{}\"",
                t("okänd platstyp", "unknown location type"),
                truncate_chars(kind, 40)
            ))
        };
    }
    let kind = LocKind::parse(kind).ok_or_else(|| {
        format!(
            "{} \"{}\"",
            t("okänd platstyp", "unknown location type"),
            truncate_chars(kind, 40)
        )
    })?;
    if kind == LocKind::Drive && !is_http_url(value) {
        return Err(t(
            "en Google Drive-plats måste vara en länk som börjar med http:// eller https://",
            "a Google Drive location must be a link starting with http:// or https://",
        )
        .to_string());
    }
    Ok(Some(Location {
        kind,
        value: value.to_string(),
    }))
}

/// Empty string or a valid date of the form YYYY-MM-DD.
pub fn valid_deadline(s: &str) -> bool {
    s.is_empty() || (s.len() == 10 && chrono::NaiveDate::parse_from_str(s, "%Y-%m-%d").is_ok())
}

pub fn priority_label(priority: u8) -> &'static str {
    match priority {
        1 => t("Hög", "High"),
        2 => "Normal",
        3 => t("Låg", "Low"),
        _ => "?",
    }
}

pub fn truncate_chars(s: &str, max: usize) -> String {
    if char_len(s) <= max {
        s.to_string()
    } else {
        let mut out: String = s.chars().take(max).collect();
        out.push('…');
        out
    }
}

/// Text from shared files is always shown as plain text. For single-line fields,
/// control characters (line breaks etc.) are replaced with spaces.
pub fn one_line(s: &str) -> String {
    s.chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect()
}

/// Multi-line text: keep line breaks, remove other control characters.
pub fn clean_text(s: &str) -> String {
    s.chars()
        .filter(|&c| c == '\n' || !c.is_control())
        .collect()
}

/// The derived request, as it looks after replaying the events.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Request {
    pub request_id: Uuid,
    /// Sequence number in the order the requests were created (by the total order).
    pub seq: u32,
    pub title: String,
    pub description: String,
    pub sim_type: String,
    pub priority: u8,
    pub requester: String,
    pub deadline: String,
    pub input: Option<Location>,
    pub status: Status,
    pub assignee: String,
    pub created_at: u64,
    pub started_at: Option<u64>,
    pub finished_at: Option<u64>,
    pub report: Option<Location>,
    pub results: Option<Location>,
    pub summary: String,
    pub note: String,
    /// Who last changed the request, and how.
    pub last_by: String,
    pub last_kind: OpKind,
}

/// List order: in progress first, then queued by priority and oldest
/// first, finally finished ones with the latest first.
pub fn list_order(a: &Request, b: &Request) -> Ordering {
    fn group(r: &Request) -> u8 {
        match r.status {
            Status::Running => 0,
            Status::Queued => 1,
            _ => 2,
        }
    }
    group(a)
        .cmp(&group(b))
        .then_with(|| match a.status {
            Status::Running => a.started_at.cmp(&b.started_at),
            Status::Queued => a
                .priority
                .cmp(&b.priority)
                .then(a.created_at.cmp(&b.created_at)),
            _ => b.finished_at.cmp(&a.finished_at),
        })
        .then_with(|| a.seq.cmp(&b.seq))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_http_and_https_are_links() {
        assert!(is_http_url("https://drive.google.com/file/d/abc"));
        assert!(is_http_url("http://example.com"));
        assert!(is_http_url("HTTPS://Example.com/x"));
        assert!(!is_http_url("javascript:alert(1)"));
        assert!(!is_http_url("file:///etc/passwd"));
        assert!(!is_http_url("https://"));
        assert!(!is_http_url(" https://example.com"));
        assert!(!is_http_url("https://example.com/a b"));
        assert!(!is_http_url("https://example.com/\njavascript:alert(1)"));
        assert!(!is_http_url("ftp://example.com"));
        assert!(!is_http_url("åäö"));
        assert!(!is_http_url(""));
    }

    #[test]
    fn locations_are_validated() {
        assert_eq!(parse_location("", ""), Ok(None));
        assert_eq!(parse_location("vault", "  "), Ok(None));
        assert!(parse_location("", "något").is_err());
        assert!(parse_location("dropbox", "något").is_err());
        assert!(parse_location("drive", "javascript:alert(1)").is_err());
        assert!(parse_location("drive", "Uprights/v3").is_err());
        assert_eq!(
            parse_location("vault", "Uprights/v3/Assembly.iam"),
            Ok(Some(Location {
                kind: LocKind::Vault,
                value: "Uprights/v3/Assembly.iam".to_string()
            }))
        );
        assert!(parse_location("drive", "https://drive.google.com/x")
            .unwrap()
            .is_some());
        assert!(parse_location("other", &"x".repeat(MAX_TEXT + 1)).is_err());
    }

    #[test]
    fn deadlines() {
        assert!(valid_deadline(""));
        assert!(valid_deadline("2026-11-30"));
        assert!(!valid_deadline("2026-13-01"));
        assert!(!valid_deadline("2026-1-5"));
        assert!(!valid_deadline("imorgon"));
    }
}
