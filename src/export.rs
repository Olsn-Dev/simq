//! CSV export and statistics. Pure functions, so that they can be tested
//! without the user interface.

use std::collections::HashMap;

use chrono::{Local, TimeZone};
use uuid::Uuid;

use crate::blobs::FileRef;
use crate::i18n::t;
use crate::model::{
    one_line, priority_label, Department, LocKind, Location, Part, Request, Status,
};

/// Excel in Swedish expects semicolons between the columns.
const SEPARATOR: char = ';';

fn local_time(ms: u64) -> String {
    i64::try_from(ms)
        .ok()
        .and_then(|ms| Local.timestamp_millis_opt(ms).single())
        .map(|t| t.format("%Y-%m-%d %H:%M").to_string())
        .unwrap_or_default()
}

/// One CSV field. Line breaks become spaces, quotes are doubled, and a value
/// that a spreadsheet would read as a formula gets a leading apostrophe.
fn field(value: &str) -> String {
    let mut text = one_line(value).trim().to_string();
    if text.starts_with(['=', '+', '-', '@']) {
        text.insert(0, '\'');
    }
    if text.contains([SEPARATOR, '"', ',']) {
        format!("\"{}\"", text.replace('"', "\"\""))
    } else {
        text
    }
}

fn location(location: &Option<Location>) -> String {
    match location {
        None => String::new(),
        Some(l) if l.kind == LocKind::File => FileRef::parse(&l.value)
            .map(|file| file.name)
            .unwrap_or_default(),
        Some(l) => l.value.clone(),
    }
}

/// The given requests as CSV text (UTF-8 with a byte order mark, so that
/// Excel shows å, ä and ö correctly).
pub fn requests_csv(requests: &[&Request], departments: &[Department], parts: &[Part]) -> String {
    let department = |id: Option<Uuid>| {
        id.and_then(|id| departments.iter().find(|d| d.id == id))
            .map(|d| d.name.clone())
            .unwrap_or_default()
    };
    let part = |id: Option<Uuid>| {
        id.and_then(|id| parts.iter().find(|p| p.id == id))
            .map(|p| p.name.clone())
            .unwrap_or_default()
    };
    let header = [
        "#",
        t("Titel", "Title"),
        t("Avdelning", "Department"),
        t("Del", "Part"),
        t("Typ", "Type"),
        t("Prioritet", "Priority"),
        "Status",
        t("Beställare", "Requester"),
        t("Utförare", "Assignee"),
        "Deadline",
        t("Skapad", "Created"),
        t("Påbörjad", "Started"),
        t("Avslutad", "Finished"),
        t("Indata", "Input"),
        t("Rapport", "Report"),
        t("Resultat", "Results"),
        t("Sammanfattning", "Summary"),
        t("Anteckning", "Note"),
        t("Kommentarer", "Comments"),
        t("Beskrivning", "Description"),
    ];
    let mut out = String::from('\u{feff}');
    let mut line = |cells: Vec<String>| {
        let cells: Vec<String> = cells.iter().map(|c| field(c)).collect();
        out.push_str(&cells.join(&SEPARATOR.to_string()));
        out.push_str("\r\n");
    };
    line(header.iter().map(|h| h.to_string()).collect());
    for r in requests {
        line(vec![
            r.seq.to_string(),
            r.title.clone(),
            department(r.department),
            part(r.part),
            r.sim_type.clone(),
            priority_label(r.priority).to_string(),
            r.status.label().to_string(),
            r.requester.clone(),
            r.assignee.clone(),
            r.deadline.clone(),
            local_time(r.created_at),
            r.started_at.map(local_time).unwrap_or_default(),
            r.finished_at.map(local_time).unwrap_or_default(),
            location(&r.input),
            location(&r.report),
            location(&r.results),
            r.summary.clone(),
            r.note.clone(),
            r.comments.len().to_string(),
            r.description.clone(),
        ]);
    }
    out
}

/// A name and how many requests it has.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Count {
    pub label: String,
    pub total: usize,
    /// Of these, how many are queued or in progress.
    pub active: usize,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Stats {
    pub total: usize,
    pub by_status: Vec<(Status, usize)>,
    pub by_department: Vec<Count>,
    pub by_type: Vec<Count>,
    pub by_assignee: Vec<Count>,
    /// Average time from created to started, for requests that were taken.
    pub avg_wait_ms: Option<u64>,
    /// Average time from created to finished, for done requests.
    pub avg_lead_ms: Option<u64>,
    pub overdue: usize,
}

fn average(values: &[u64]) -> Option<u64> {
    (!values.is_empty()).then(|| values.iter().sum::<u64>() / values.len() as u64)
}

fn counts(requests: &[Request], key: impl Fn(&Request) -> String) -> Vec<Count> {
    let mut map: HashMap<String, (usize, usize)> = HashMap::new();
    for r in requests {
        let entry = map.entry(key(r)).or_default();
        entry.0 += 1;
        entry.1 += usize::from(r.status.is_active());
    }
    let mut list: Vec<Count> = map
        .into_iter()
        .map(|(label, (total, active))| Count {
            label,
            total,
            active,
        })
        .collect();
    list.sort_by(|a, b| b.total.cmp(&a.total).then(a.label.cmp(&b.label)));
    list
}

/// Statistics over the given requests. `today` is `YYYY-MM-DD`.
pub fn stats(requests: &[Request], departments: &[Department], today: &str) -> Stats {
    let none = || t("(ingen)", "(none)").to_string();
    let or_none = |text: &str| {
        if text.trim().is_empty() {
            none()
        } else {
            one_line(text.trim())
        }
    };
    let by_status = [
        Status::Queued,
        Status::Running,
        Status::Done,
        Status::Failed,
        Status::Cancelled,
    ]
    .into_iter()
    .map(|status| {
        (
            status,
            requests.iter().filter(|r| r.status == status).count(),
        )
    })
    .collect();
    let waits: Vec<u64> = requests
        .iter()
        .filter_map(|r| Some(r.started_at?.saturating_sub(r.created_at)))
        .collect();
    let leads: Vec<u64> = requests
        .iter()
        .filter(|r| r.status == Status::Done)
        .filter_map(|r| Some(r.finished_at?.saturating_sub(r.created_at)))
        .collect();
    let taken: Vec<Request> = requests
        .iter()
        .filter(|r| !r.assignee.trim().is_empty())
        .cloned()
        .collect();
    Stats {
        total: requests.len(),
        by_status,
        by_department: counts(requests, |r| {
            r.department
                .and_then(|id| departments.iter().find(|d| d.id == id))
                .map(|d| one_line(&d.name))
                .unwrap_or_else(none)
        }),
        by_type: counts(requests, |r| or_none(&r.sim_type)),
        by_assignee: counts(&taken, |r| or_none(&r.assignee)),
        avg_wait_ms: average(&waits),
        avg_lead_ms: average(&leads),
        overdue: requests
            .iter()
            .filter(|r| {
                r.status.is_active() && !r.deadline.is_empty() && r.deadline.as_str() < today
            })
            .count(),
    }
}

/// "3 d 4 h", "5 h 10 min" or "12 min".
pub fn duration_label(ms: u64) -> String {
    let minutes = ms / 60_000;
    let (days, hours, mins) = (minutes / 1440, (minutes % 1440) / 60, minutes % 60);
    if days > 0 {
        format!("{days} d {hours} h")
    } else if hours > 0 {
        format!("{hours} h {mins} min")
    } else {
        format!("{mins} min")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::Comment;
    use crate::ops::NewAction;
    use crate::replay::State;
    use crate::testutil::{create_action, finish_action, op_at, take_action};
    use std::sync::Arc;

    fn sample() -> (Vec<Request>, Vec<Department>) {
        let client = Uuid::new_v4();
        let dept = Uuid::new_v4();
        let mut state = State::default();
        let mut apply = |t: u64, id: Uuid, action: &NewAction| {
            state.apply(&Arc::new(op_at(client, t, 0, "Anna", id, action)));
        };
        apply(
            1,
            dept,
            &NewAction::DeptSet {
                regions: Vec::new(),
                name: "Aero".to_string(),
                color: 0,
                zones: Vec::new(),
            },
        );
        let (a, b, c) = (Uuid::new_v4(), Uuid::new_v4(), Uuid::new_v4());
        let mut with_dept = create_action("=SUM(A1); \"vinge\"\nrad två");
        if let NewAction::Create { department, .. } = &mut with_dept {
            *department = Some(dept);
        }
        apply(1_000, a, &with_dept);
        apply(61_000, a, &take_action("Bo"));
        apply(3_601_000, a, &finish_action("Bo"));
        apply(2_000, b, &create_action("Upright"));
        apply(122_000, b, &take_action("Cia"));
        apply(3_000, c, &create_action("Monokock"));
        let mut requests: Vec<Request> = state.requests.values().cloned().collect();
        requests.sort_by_key(|r| r.seq);
        requests[0].comments.push(Comment {
            op_id: Uuid::new_v4(),
            at_ms: 5,
            by: "Bo".to_string(),
            text: "Hej".to_string(),
        });
        let departments = state.departments.values().cloned().collect();
        (requests, departments)
    }

    #[test]
    fn csv_is_safe_for_spreadsheets() {
        let (requests, departments) = sample();
        let refs: Vec<&Request> = requests.iter().collect();
        let csv = crate::i18n::with_lang(crate::i18n::Lang::Sv, || {
            requests_csv(&refs, &departments, &[])
        });
        assert!(csv.starts_with('\u{feff}'));
        let lines: Vec<&str> = csv.trim_start_matches('\u{feff}').split("\r\n").collect();
        assert_eq!(
            lines.len(),
            5,
            "header, three requests and a final line break"
        );
        assert!(lines[0].starts_with("#;Titel;Avdelning;Del;Typ;"));
        // The formula is defused, the quotes doubled and the line break removed.
        assert!(lines[1]
            .starts_with("1;\"'=SUM(A1); \"\"vinge\"\" rad två\";Aero;;FEA;Normal;Klar;Anna;Bo;"));
        assert!(lines[2].starts_with("2;Upright;;;FEA;Normal;Pågår;Anna;Cia;"));
        assert_eq!(
            lines[1].matches(';').count() - 1,
            lines[0].matches(';').count()
        );
        assert_eq!(field("-1"), "'-1");
        assert_eq!(field("@x"), "'@x");
        assert_eq!(field("a,b"), "\"a,b\"");
    }

    #[test]
    fn statistics() {
        let (requests, departments) = sample();
        let stats = crate::i18n::with_lang(crate::i18n::Lang::Sv, || {
            stats(&requests, &departments, "2027-01-01")
        });
        assert_eq!(stats.total, 3);
        assert_eq!(
            stats.by_status,
            vec![
                (Status::Queued, 1),
                (Status::Running, 1),
                (Status::Done, 1),
                (Status::Failed, 0),
                (Status::Cancelled, 0)
            ]
        );
        assert_eq!(stats.by_department[0].label, "(ingen)");
        assert_eq!(
            (stats.by_department[0].total, stats.by_department[0].active),
            (2, 2)
        );
        assert_eq!(stats.by_department[1].label, "Aero");
        assert_eq!(stats.by_type.len(), 1);
        assert_eq!(stats.by_assignee.len(), 2);
        // Waits of 60 s and 120 s, one lead time of an hour.
        assert_eq!(stats.avg_wait_ms, Some(90_000));
        assert_eq!(stats.avg_lead_ms, Some(3_600_000));
        // The sample requests have the deadline 2026-12-01; the done one does not count.
        assert_eq!(stats.overdue, 2);
        assert_eq!(
            super::stats(&requests, &departments, "2026-01-01").overdue,
            0
        );

        assert_eq!(duration_label(59_000), "0 min");
        assert_eq!(duration_label(3_600_000 + 5 * 60_000), "1 h 5 min");
        assert_eq!(duration_label(3 * 86_400_000 + 4 * 3_600_000), "3 d 4 h");
    }
}
