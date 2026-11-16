//! Domain model: status, location kinds and the derived request.

use std::cmp::Ordering;

use uuid::Uuid;

use crate::i18n::t;

pub const MAX_TITLE: usize = 200;
pub const MAX_NAME: usize = 200;
pub const MAX_TEXT: usize = 4000;
pub const MAX_DEPT_NAME: usize = 60;
pub const MAX_PART_NAME: usize = 120;
/// Most zones a department can have on the car drawing.
pub const MAX_ZONES: usize = 16;
/// The car drawing is this many units wide and high. Zones use the same units.
pub const CANVAS_W: u16 = 1000;
pub const CANVAS_H: u16 = 600;
/// Smallest side of a zone.
pub const MIN_ZONE: u16 = 12;
/// Most car regions a department can have, and the longest region id.
pub const MAX_REGIONS: usize = 32;
pub const MAX_REGION_ID: usize = 32;
/// Number of colours a department can choose between.
pub const COLOR_COUNT: u8 = 10;

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
    Delete,
    Comment,
    DeptSet,
    DeptDelete,
    PartSet,
    PartDelete,
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
            OpKind::Delete => "delete",
            OpKind::Comment => "comment",
            OpKind::DeptSet => "dept_set",
            OpKind::DeptDelete => "dept_delete",
            OpKind::PartSet => "part_set",
            OpKind::PartDelete => "part_delete",
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
            "delete" => OpKind::Delete,
            "comment" => OpKind::Comment,
            "dept_set" => OpKind::DeptSet,
            "dept_delete" => OpKind::DeptDelete,
            "part_set" => OpKind::PartSet,
            "part_delete" => OpKind::PartDelete,
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
            OpKind::Delete => t("Tog bort ärendet", "Deleted the request"),
            OpKind::Comment => t("Kommenterade", "Commented"),
            OpKind::DeptSet => t("Ändrade avdelningen", "Changed the department"),
            OpKind::DeptDelete => t("Tog bort avdelningen", "Deleted the department"),
            OpKind::PartSet => t("Ändrade delen", "Changed the part"),
            OpKind::PartDelete => t("Tog bort delen", "Deleted the part"),
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
            OpKind::Delete => t("tog bort ärendet", "deleted the request"),
            OpKind::Comment => t("kommenterade", "commented"),
            OpKind::DeptSet => t("ändrade avdelningen", "changed the department"),
            OpKind::DeptDelete => t("tog bort avdelningen", "deleted the department"),
            OpKind::PartSet => t("ändrade delen", "changed the part"),
            OpKind::PartDelete => t("tog bort delen", "deleted the part"),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum LocKind {
    #[default]
    Drive,
    Vault,
    Other,
    /// A file uploaded through the app.
    File,
}

impl LocKind {
    pub const ALL: [LocKind; 4] = [
        LocKind::Drive,
        LocKind::Vault,
        LocKind::Other,
        LocKind::File,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            LocKind::Drive => "drive",
            LocKind::Vault => "vault",
            LocKind::Other => "other",
            LocKind::File => "file",
        }
    }

    pub fn parse(s: &str) -> Option<LocKind> {
        Some(match s {
            "drive" => LocKind::Drive,
            "vault" => LocKind::Vault,
            "other" => LocKind::Other,
            "file" => LocKind::File,
            _ => return None,
        })
    }

    pub fn label(self) -> &'static str {
        match self {
            LocKind::Drive => "Google Drive",
            LocKind::Vault => "Vault",
            LocKind::Other => t("Annat", "Other"),
            LocKind::File => t("Ladda upp fil", "Upload file"),
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
            LocKind::File => "",
        }
    }

    pub fn help(self) -> &'static str {
        match self {
            LocKind::Drive => {
                t("Klistra in delningslänken från Google Drive. Den måste börja med http:// eller https://.", "Paste the sharing link from Google Drive. It must start with http:// or https://.")
            }
            LocKind::Vault => t("Beskriv kort var i Vault filen ligger.", "Briefly describe where in Vault the file is."),
            LocKind::Other => t("Fri text: beskriv var filen finns.", "Free text: describe where the file is."),
            LocKind::File => t("Filen laddas upp och kan hämtas av alla i laget. Högst 500 MB.", "The file is uploaded and can be downloaded by everyone on the team. At most 500 MB."),
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
    if kind == LocKind::File && crate::blobs::FileRef::parse(value).is_none() {
        return Err(t(
            "ogiltig referens till en uppladdad fil",
            "invalid reference to an uploaded file",
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
    /// The department that asks for the simulation, if one was chosen.
    pub department: Option<Uuid>,
    /// The part of the car the simulation is about, if one was chosen.
    pub part: Option<Uuid>,
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
    /// Set when the request has been deleted. It is then hidden from the list
    /// and no further events apply to it.
    pub deleted_at: Option<u64>,
    /// Who last changed the request, and how.
    pub last_by: String,
    pub last_kind: OpKind,
    pub comments: Vec<Comment>,
    /// The app that created the request.
    pub created_by: Uuid,
    /// When the request last changed (comments included), and by which app.
    pub changed_at: u64,
    pub changed_by: Uuid,
}

/// A comment on a request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Comment {
    pub op_id: Uuid,
    pub at_ms: u64,
    pub by: String,
    pub text: String,
}

/// A rectangle on the car drawing, in the drawing's own units.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Zone {
    pub x: u16,
    pub y: u16,
    pub w: u16,
    pub h: u16,
}

impl Zone {
    pub fn valid(&self) -> bool {
        self.w >= MIN_ZONE
            && self.h >= MIN_ZONE
            && u32::from(self.x) + u32::from(self.w) <= u32::from(CANVAS_W)
            && u32::from(self.y) + u32::from(self.h) <= u32::from(CANVAS_H)
    }
}

/// True for a well-formed region id: lower-case letters, digits and `_`.
/// Ids this app does not know are kept, so that newer apps can add regions.
pub fn valid_region_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= MAX_REGION_ID
        && id
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_')
}

/// How far a part of the car has come.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, PartialOrd, Ord)]
pub enum PartStatus {
    #[default]
    NotStarted,
    Design,
    Simulation,
    Manufacturing,
    Done,
}

impl PartStatus {
    pub const ALL: [PartStatus; 5] = [
        PartStatus::NotStarted,
        PartStatus::Design,
        PartStatus::Simulation,
        PartStatus::Manufacturing,
        PartStatus::Done,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            PartStatus::NotStarted => "not_started",
            PartStatus::Design => "design",
            PartStatus::Simulation => "simulation",
            PartStatus::Manufacturing => "manufacturing",
            PartStatus::Done => "done",
        }
    }

    pub fn parse(s: &str) -> Option<PartStatus> {
        PartStatus::ALL.into_iter().find(|p| p.as_str() == s)
    }

    pub fn label(self) -> &'static str {
        match self {
            PartStatus::NotStarted => t("Ej påbörjad", "Not started"),
            PartStatus::Design => "Design",
            PartStatus::Simulation => t("Simulering", "Simulation"),
            PartStatus::Manufacturing => t("Tillverkning", "Manufacturing"),
            PartStatus::Done => t("Klar", "Done"),
        }
    }

    /// 0 for not started up to 4 for done.
    pub fn step(self) -> u32 {
        self as u32
    }
}

/// A department: a folder of parts with a name, a colour and zones on the car.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Department {
    pub id: Uuid,
    pub seq: u32,
    pub name: String,
    pub color: u8,
    /// Rectangles from version 0.5.0. Kept so that old data stays valid, but
    /// no longer shown: the regions below replaced them.
    pub zones: Vec<Zone>,
    /// Ids of the parts of the car drawing that belong to the department.
    pub regions: Vec<String>,
    pub created_at: u64,
    pub changed_at: u64,
    pub deleted_at: Option<u64>,
    pub last_by: String,
}

/// A part of the car that a department keeps track of.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Part {
    pub id: Uuid,
    pub seq: u32,
    pub department: Uuid,
    pub name: String,
    pub status: PartStatus,
    pub note: String,
    pub created_at: u64,
    pub changed_at: u64,
    pub deleted_at: Option<u64>,
    pub last_by: String,
}

/// Share of the work that is done, 0.0 to 1.0, for a set of part statuses.
/// `None` when there are no parts.
pub fn progress(statuses: impl Iterator<Item = PartStatus>) -> Option<f32> {
    let (mut sum, mut count) = (0u32, 0u32);
    for status in statuses {
        sum += status.step();
        count += 1;
    }
    (count > 0).then(|| sum as f32 / (count * 4) as f32)
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
    fn zones_and_progress() {
        let zone = |x, y, w, h| Zone { x, y, w, h };
        assert!(zone(0, 0, 1000, 600).valid());
        assert!(zone(988, 588, 12, 12).valid());
        assert!(!zone(989, 0, 12, 12).valid());
        assert!(!zone(0, 0, 11, 50).valid());
        assert!(!zone(0, 590, 50, 12).valid());
        assert!(!zone(65535, 0, 65535, 12).valid());

        assert_eq!(progress(std::iter::empty()), None);
        assert_eq!(progress([PartStatus::Done].into_iter()), Some(1.0));
        assert_eq!(
            progress([PartStatus::NotStarted, PartStatus::Done].into_iter()),
            Some(0.5)
        );
        for status in PartStatus::ALL {
            assert_eq!(PartStatus::parse(status.as_str()), Some(status));
        }
        assert_eq!(PartStatus::parse("klar"), None);
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
