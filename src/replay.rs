//! Den deterministiska statusmaskinen.
//!
//! Händelserna spelas upp i den totala ordningen (wall_ms, counter, client_id,
//! op_id). Varje händelse blir tillämpad eller avvisad beroende på ärendets
//! status just då. Resultatet beror bara på MÄNGDEN händelser, aldrig på i
//! vilken ordning filerna råkade komma fram.

use std::collections::HashMap;
use std::sync::Arc;

use uuid::Uuid;

use crate::i18n::{is_en, t};

use crate::model::{OpKind, Request, Status};
use crate::ops::{Action, Op};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Reject {
    /// create för ett request_id som redan finns.
    AlreadyExists,
    /// Övergången är inte tillåten från den status ärendet hade.
    WrongStatus(Status),
    /// Händelsens innehåll är ogiltigt.
    Invalid(String),
}

impl Reject {
    pub fn text(&self) -> String {
        match self {
            Reject::AlreadyExists => {
                t("Ärendet fanns redan", "The request already existed").to_string()
            }
            Reject::WrongStatus(status) => {
                if is_en() {
                    format!("The request was already {}", status.label().to_lowercase())
                } else {
                    format!("Ärendet var redan {}", status.label())
                }
            }
            Reject::Invalid(msg) => format!("{}: {msg}", t("Ogiltig händelse", "Invalid event")),
        }
    }
}

/// Den händelse som gjorde att en senare händelse avvisades.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Blocker {
    pub by: String,
    pub kind: OpKind,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    Applied,
    Rejected {
        reason: Reject,
        blocker: Option<Blocker>,
    },
    /// Ärendets create har inte kommit fram än.
    Waiting,
    /// Okänd sort eller nyare version: sparad men utan effekt.
    Ignored,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct State {
    pub requests: HashMap<Uuid, Request>,
    pub outcomes: HashMap<Uuid, Outcome>,
    waiting: HashMap<Uuid, Vec<Arc<Op>>>,
    /// Sant om någon händelse kräver en nyare app.
    pub unsupported_seen: bool,
    next_seq: u32,
}

impl State {
    /// Spelar upp hela loggen från början. `ops` måste vara sorterad.
    pub fn replay_all(ops: &[Arc<Op>]) -> State {
        let mut state = State::default();
        for op in ops {
            state.apply(op);
        }
        state
    }

    /// Tillämpar nästa händelse i ordningen. Returnerar id för de händelser
    /// vars utfall sattes (händelsen själv, plus sådana som väntat på create).
    pub fn apply(&mut self, op: &Arc<Op>) -> Vec<Uuid> {
        if self.outcomes.contains_key(&op.op_id) {
            return Vec::new(); // dubblett
        }
        let mut touched = vec![op.op_id];
        match &op.action {
            Action::Unsupported => {
                self.unsupported_seen = true;
                self.outcomes.insert(op.op_id, Outcome::Ignored);
            }
            Action::Invalid(msg) => {
                self.outcomes.insert(
                    op.op_id,
                    Outcome::Rejected {
                        reason: Reject::Invalid(msg.clone()),
                        blocker: None,
                    },
                );
            }
            Action::Create(payload) => {
                if let Some(existing) = self.requests.get(&op.request_id) {
                    let blocker = Blocker {
                        by: existing.last_by.clone(),
                        kind: existing.last_kind,
                    };
                    self.outcomes.insert(
                        op.op_id,
                        Outcome::Rejected {
                            reason: Reject::AlreadyExists,
                            blocker: Some(blocker),
                        },
                    );
                } else {
                    self.next_seq += 1;
                    let request = Request {
                        request_id: op.request_id,
                        seq: self.next_seq,
                        title: payload.title.clone(),
                        description: payload.description.clone(),
                        sim_type: payload.sim_type.clone(),
                        priority: payload.priority,
                        requester: payload.requester.clone(),
                        deadline: payload.deadline.clone(),
                        input: payload.input.clone(),
                        status: Status::Queued,
                        assignee: String::new(),
                        created_at: op.hlc.wall_ms,
                        started_at: None,
                        finished_at: None,
                        report: None,
                        results: None,
                        summary: String::new(),
                        note: String::new(),
                        last_by: op.by.clone(),
                        last_kind: OpKind::Create,
                    };
                    self.requests.insert(op.request_id, request);
                    self.outcomes.insert(op.op_id, Outcome::Applied);
                    // Händelser som kom före sin create tillämpas nu, i ordning.
                    if let Some(waiting) = self.waiting.remove(&op.request_id) {
                        for earlier in waiting {
                            self.apply_to_existing(&earlier);
                            touched.push(earlier.op_id);
                        }
                    }
                }
            }
            _ => {
                if self.requests.contains_key(&op.request_id) {
                    self.apply_to_existing(op);
                } else {
                    self.waiting
                        .entry(op.request_id)
                        .or_default()
                        .push(Arc::clone(op));
                    self.outcomes.insert(op.op_id, Outcome::Waiting);
                }
            }
        }
        touched
    }

    fn apply_to_existing(&mut self, op: &Op) {
        if let Some(request) = self.requests.get_mut(&op.request_id) {
            let outcome = transition(request, op);
            self.outcomes.insert(op.op_id, outcome);
        }
    }

    /// Vad skulle hända om `op` tillämpades nu? Ändrar ingenting.
    pub fn preview(&self, op: &Op) -> Outcome {
        match &op.action {
            Action::Unsupported => Outcome::Ignored,
            Action::Invalid(msg) => Outcome::Rejected {
                reason: Reject::Invalid(msg.clone()),
                blocker: None,
            },
            Action::Create(_) => match self.requests.get(&op.request_id) {
                Some(existing) => Outcome::Rejected {
                    reason: Reject::AlreadyExists,
                    blocker: Some(Blocker {
                        by: existing.last_by.clone(),
                        kind: existing.last_kind,
                    }),
                },
                None => Outcome::Applied,
            },
            _ => match self.requests.get(&op.request_id) {
                Some(existing) => transition(&mut existing.clone(), op),
                None => Outcome::Waiting,
            },
        }
    }
}

/// Statusmaskinen för ett befintligt ärende.
fn transition(request: &mut Request, op: &Op) -> Outcome {
    let status = request.status;
    let at = op.hlc.wall_ms;
    let kind = match &op.action {
        Action::Take { assignee } if status == Status::Queued => {
            request.status = Status::Running;
            request.assignee = assignee.clone();
            request.started_at = Some(at);
            OpKind::Take
        }
        Action::Finish(payload) if status.is_active() => {
            request.status = Status::Done;
            request.assignee = payload.assignee.clone();
            request.report = Some(payload.report.clone());
            request.results = payload.results.clone();
            request.summary = payload.summary.clone();
            request.finished_at = Some(at);
            OpKind::Finish
        }
        Action::Fail { note } if status.is_active() => {
            request.status = Status::Failed;
            request.note = note.clone();
            request.finished_at = Some(at);
            OpKind::Fail
        }
        Action::Cancel { note } if status.is_active() => {
            request.status = Status::Cancelled;
            request.note = note.clone();
            request.finished_at = Some(at);
            OpKind::Cancel
        }
        Action::Requeue if matches!(status, Status::Failed | Status::Cancelled) => {
            request.status = Status::Queued;
            request.assignee.clear();
            request.note.clear();
            request.started_at = None;
            request.finished_at = None;
            OpKind::Requeue
        }
        _ => {
            return Outcome::Rejected {
                reason: Reject::WrongStatus(status),
                blocker: Some(Blocker {
                    by: request.last_by.clone(),
                    kind: request.last_kind,
                }),
            }
        }
    };
    request.last_by = op.by.clone();
    request.last_kind = kind;
    Outcome::Applied
}

pub fn sort_ops(ops: &mut [Arc<Op>]) {
    ops.sort_by_key(|op| op.key());
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{LocKind, Location};
    use crate::ops::NewAction;
    use crate::testutil::{create_action, finish_action, op_at, take_action};

    fn replay(mut ops: Vec<Op>) -> State {
        let mut arcs: Vec<Arc<Op>> = ops.drain(..).map(Arc::new).collect();
        sort_ops(&mut arcs);
        State::replay_all(&arcs)
    }

    fn rejected_for(state: &State, op: &Op, status: Status) -> bool {
        matches!(
            state.outcomes.get(&op.op_id),
            Some(Outcome::Rejected { reason: Reject::WrongStatus(s), .. }) if *s == status
        )
    }

    #[test]
    fn happy_path() {
        let (a, b) = (Uuid::new_v4(), Uuid::new_v4());
        let id = Uuid::new_v4();
        let create = op_at(a, 1000, 0, "Anna", id, &create_action("Upright"));
        let take = op_at(b, 2000, 0, "Bo", id, &take_action("Bo"));
        let finish = op_at(b, 3000, 0, "Bo", id, &finish_action("Bo"));
        let state = replay(vec![finish.clone(), take.clone(), create.clone()]);

        let request = &state.requests[&id];
        assert_eq!(request.status, Status::Done);
        assert_eq!(request.seq, 1);
        assert_eq!(request.assignee, "Bo");
        assert_eq!(request.created_at, 1000);
        assert_eq!(request.started_at, Some(2000));
        assert_eq!(request.finished_at, Some(3000));
        assert_eq!(request.report.as_ref().unwrap().kind, LocKind::Drive);
        assert_eq!(request.results.as_ref().unwrap().kind, LocKind::Vault);
        for op in [&create, &take, &finish] {
            assert_eq!(state.outcomes[&op.op_id], Outcome::Applied);
        }
    }

    #[test]
    fn earliest_take_wins_and_the_other_is_rejected() {
        let (a, b, c) = (Uuid::new_v4(), Uuid::new_v4(), Uuid::new_v4());
        let id = Uuid::new_v4();
        let create = op_at(a, 1000, 0, "Anna", id, &create_action("Upright"));
        let take_bo = op_at(b, 2000, 0, "Bo", id, &take_action("Bo"));
        let take_cia = op_at(c, 2001, 0, "Cia", id, &take_action("Cia"));
        let state = replay(vec![take_cia.clone(), create, take_bo.clone()]);

        assert_eq!(state.requests[&id].status, Status::Running);
        assert_eq!(state.requests[&id].assignee, "Bo");
        assert_eq!(state.outcomes[&take_bo.op_id], Outcome::Applied);
        assert_eq!(
            state.outcomes[&take_cia.op_id],
            Outcome::Rejected {
                reason: Reject::WrongStatus(Status::Running),
                blocker: Some(Blocker {
                    by: "Bo".to_string(),
                    kind: OpKind::Take
                }),
            }
        );
    }

    #[test]
    fn ties_are_broken_by_counter_then_client_then_op_id() {
        let id = Uuid::new_v4();
        let creator = Uuid::new_v4();
        let low = Uuid::parse_str("00000000-0000-4000-8000-000000000001").unwrap();
        let high = Uuid::parse_str("ffffffff-0000-4000-8000-000000000001").unwrap();
        let create = op_at(creator, 1000, 0, "Anna", id, &create_action("Upright"));

        // Samma millisekund och räknare: lägst client_id vinner.
        let t_low = op_at(low, 2000, 0, "Låg", id, &take_action("Låg"));
        let t_high = op_at(high, 2000, 0, "Hög", id, &take_action("Hög"));
        let state = replay(vec![create.clone(), t_high, t_low]);
        assert_eq!(state.requests[&id].assignee, "Låg");

        // Lägre räknare går före oavsett client_id.
        let t_low = op_at(low, 2000, 1, "Låg", id, &take_action("Låg"));
        let t_high = op_at(high, 2000, 0, "Hög", id, &take_action("Hög"));
        let state = replay(vec![create, t_low, t_high]);
        assert_eq!(state.requests[&id].assignee, "Hög");
    }

    #[test]
    fn invalid_transitions_are_rejected() {
        let a = Uuid::new_v4();
        let id = Uuid::new_v4();
        let note = |n: &str| n.to_string();
        let create = op_at(a, 1, 0, "A", id, &create_action("Upright"));
        let requeue_queued = op_at(a, 2, 0, "A", id, &NewAction::Requeue);
        let take = op_at(a, 3, 0, "A", id, &take_action("A"));
        let take_again = op_at(a, 4, 0, "B", id, &take_action("B"));
        let requeue_running = op_at(a, 5, 0, "A", id, &NewAction::Requeue);
        let fail = op_at(
            a,
            6,
            0,
            "A",
            id,
            &NewAction::Fail {
                note: note("Divergerade"),
            },
        );
        let take_failed = op_at(a, 7, 0, "B", id, &take_action("B"));
        let finish_failed = op_at(a, 8, 0, "B", id, &finish_action("B"));
        let cancel_failed = op_at(a, 9, 0, "B", id, &NewAction::Cancel { note: note("") });
        let requeue = op_at(a, 10, 0, "A", id, &NewAction::Requeue);
        let finish = op_at(a, 11, 0, "C", id, &finish_action("C"));
        let after_done: Vec<Op> = vec![
            op_at(a, 12, 0, "A", id, &take_action("A")),
            op_at(a, 13, 0, "A", id, &finish_action("A")),
            op_at(a, 14, 0, "A", id, &NewAction::Fail { note: note("") }),
            op_at(a, 15, 0, "A", id, &NewAction::Cancel { note: note("") }),
            op_at(a, 16, 0, "A", id, &NewAction::Requeue),
            op_at(a, 17, 0, "A", id, &create_action("Dubblett")),
        ];

        let mut all = vec![
            create.clone(),
            requeue_queued.clone(),
            take.clone(),
            take_again.clone(),
            requeue_running.clone(),
            fail.clone(),
            take_failed.clone(),
            finish_failed.clone(),
            cancel_failed.clone(),
            requeue.clone(),
            finish.clone(),
        ];
        all.extend(after_done.iter().cloned());
        let state = replay(all);

        for op in [&create, &take, &fail, &requeue, &finish] {
            assert_eq!(state.outcomes[&op.op_id], Outcome::Applied);
        }
        assert!(rejected_for(&state, &requeue_queued, Status::Queued));
        assert!(rejected_for(&state, &take_again, Status::Running));
        assert!(rejected_for(&state, &requeue_running, Status::Running));
        assert!(rejected_for(&state, &take_failed, Status::Failed));
        assert!(rejected_for(&state, &finish_failed, Status::Failed));
        assert!(rejected_for(&state, &cancel_failed, Status::Failed));
        for op in &after_done[..5] {
            assert!(rejected_for(&state, op, Status::Done));
        }
        assert!(matches!(
            state.outcomes[&after_done[5].op_id],
            Outcome::Rejected {
                reason: Reject::AlreadyExists,
                ..
            }
        ));

        // Köa om nollställde utförare, tider och anteckning innan finish från köad.
        let request = &state.requests[&id];
        assert_eq!(request.status, Status::Done);
        assert_eq!(request.assignee, "C");
        assert_eq!(request.started_at, None);
        assert_eq!(request.finished_at, Some(11));
        assert_eq!(request.note, "");
        assert_eq!(request.title, "Upright");
    }

    #[test]
    fn requeue_resets_assignee_times_and_note() {
        let a = Uuid::new_v4();
        let id = Uuid::new_v4();
        let ops = vec![
            op_at(a, 1, 0, "A", id, &create_action("Upright")),
            op_at(a, 2, 0, "B", id, &take_action("B")),
            op_at(
                a,
                3,
                0,
                "B",
                id,
                &NewAction::Cancel {
                    note: "Fel lastfall".to_string(),
                },
            ),
        ];
        let state = replay(ops.clone());
        assert_eq!(state.requests[&id].status, Status::Cancelled);
        assert_eq!(state.requests[&id].note, "Fel lastfall");
        assert_eq!(state.requests[&id].assignee, "B");

        let mut ops = ops;
        ops.push(op_at(a, 4, 0, "A", id, &NewAction::Requeue));
        let state = replay(ops);
        let request = &state.requests[&id];
        assert_eq!(request.status, Status::Queued);
        assert_eq!(request.assignee, "");
        assert_eq!(request.note, "");
        assert_eq!(request.started_at, None);
        assert_eq!(request.finished_at, None);
        assert_eq!(request.created_at, 1);
    }

    #[test]
    fn duplicates_are_ignored() {
        let a = Uuid::new_v4();
        let id = Uuid::new_v4();
        let create = op_at(a, 1, 0, "A", id, &create_action("Upright"));
        let take = op_at(a, 2, 0, "B", id, &take_action("B"));
        let once = replay(vec![create.clone(), take.clone()]);
        let twice = replay(vec![
            create.clone(),
            take.clone(),
            take.clone(),
            create.clone(),
        ]);
        assert_eq!(once, twice);
        assert_eq!(twice.outcomes.len(), 2);
        assert_eq!(twice.outcomes[&take.op_id], Outcome::Applied);
    }

    #[test]
    fn op_for_unknown_request_waits_for_create() {
        let (a, b) = (Uuid::new_v4(), Uuid::new_v4());
        let id = Uuid::new_v4();
        let create = Arc::new(op_at(a, 1000, 0, "Anna", id, &create_action("Upright")));
        let take = Arc::new(op_at(b, 2000, 0, "Bo", id, &take_action("Bo")));

        // Bara take har kommit fram.
        let mut state = State::default();
        state.apply(&take);
        assert!(state.requests.is_empty());
        assert_eq!(state.outcomes[&take.op_id], Outcome::Waiting);

        // När create dyker upp spelas loggen upp i rätt ordning.
        let mut ops = vec![Arc::clone(&take), Arc::clone(&create)];
        sort_ops(&mut ops);
        let state = State::replay_all(&ops);
        assert_eq!(state.requests[&id].status, Status::Running);
        assert_eq!(state.outcomes[&take.op_id], Outcome::Applied);

        // Även om take har en TIDIGARE stämpel än create (klockor som går fel)
        // tillämpas den så fort create hittats.
        let early_take = Arc::new(op_at(b, 500, 0, "Bo", id, &take_action("Bo")));
        let mut state = State::default();
        assert_eq!(state.apply(&early_take), vec![early_take.op_id]);
        assert_eq!(state.outcomes[&early_take.op_id], Outcome::Waiting);
        let touched = state.apply(&create);
        assert_eq!(touched, vec![create.op_id, early_take.op_id]);
        assert_eq!(state.outcomes[&early_take.op_id], Outcome::Applied);
        assert_eq!(state.requests[&id].assignee, "Bo");
    }

    #[test]
    fn finish_without_report_location_is_rejected() {
        let a = Uuid::new_v4();
        let id = Uuid::new_v4();
        let create = op_at(a, 1, 0, "A", id, &create_action("Upright"));
        let take = op_at(a, 2, 0, "B", id, &take_action("B"));
        let no_report = op_at(
            a,
            3,
            0,
            "B",
            id,
            &NewAction::Finish {
                assignee: "B".to_string(),
                report: None,
                results: None,
                summary: "Klart".to_string(),
            },
        );
        let no_summary = op_at(
            a,
            4,
            0,
            "B",
            id,
            &NewAction::Finish {
                assignee: "B".to_string(),
                report: Some(Location {
                    kind: LocKind::Other,
                    value: "USB-stickan".to_string(),
                }),
                results: None,
                summary: "  ".to_string(),
            },
        );
        let state = replay(vec![create, take, no_report.clone(), no_summary.clone()]);
        assert_eq!(state.requests[&id].status, Status::Running);
        for op in [&no_report, &no_summary] {
            assert!(matches!(
                state.outcomes[&op.op_id],
                Outcome::Rejected {
                    reason: Reject::Invalid(_),
                    ..
                }
            ));
        }

        // Resultatplats är valfri.
        let ok = op_at(
            a,
            5,
            0,
            "B",
            id,
            &NewAction::Finish {
                assignee: "B".to_string(),
                report: Some(Location {
                    kind: LocKind::Vault,
                    value: "Uprights/v3/Rapport.pdf".to_string(),
                }),
                results: None,
                summary: "Klart".to_string(),
            },
        );
        let mut state = state;
        state.apply(&Arc::new(ok.clone()));
        assert_eq!(state.outcomes[&ok.op_id], Outcome::Applied);
        assert_eq!(state.requests[&id].status, Status::Done);
        assert_eq!(state.requests[&id].results, None);
    }

    #[test]
    fn preview_does_not_change_state() {
        let a = Uuid::new_v4();
        let id = Uuid::new_v4();
        let state = replay(vec![
            op_at(a, 1, 0, "A", id, &create_action("Upright")),
            op_at(a, 2, 0, "B", id, &take_action("B")),
        ]);
        let before = state.clone();
        let take = op_at(a, 3, 0, "C", id, &take_action("C"));
        assert!(matches!(state.preview(&take), Outcome::Rejected { .. }));
        let finish = op_at(a, 3, 0, "C", id, &finish_action("C"));
        assert_eq!(state.preview(&finish), Outcome::Applied);
        assert_eq!(state, before);
    }

    #[test]
    fn full_replay_of_10_000_ops_is_fast() {
        let clients: Vec<Uuid> = (0..8).map(|_| Uuid::new_v4()).collect();
        let mut ops = Vec::with_capacity(10_000);
        let mut t = 1_000_000u64;
        for i in 0..2_500usize {
            let id = Uuid::new_v4();
            let c = clients[i % clients.len()];
            for action in [
                create_action("Lastfall"),
                take_action("Bo"),
                take_action("Cia"),
                finish_action("Bo"),
            ] {
                t += 7;
                ops.push(Arc::new(op_at(c, t, 0, "Bo", id, &action)));
            }
        }
        assert_eq!(ops.len(), 10_000);
        ops.reverse();

        let started = std::time::Instant::now();
        sort_ops(&mut ops);
        let state = State::replay_all(&ops);
        let elapsed = started.elapsed();

        assert_eq!(state.requests.len(), 2_500);
        assert!(state.requests.values().all(|r| r.status == Status::Done));
        assert_eq!(state.outcomes.len(), 10_000);
        assert!(
            elapsed < std::time::Duration::from_secs(1),
            "uppspelningen tog {elapsed:?}"
        );
    }
}
