//! Lokal SQLite-cache.
//!
//! Cachen är helt härledd från filerna i den delade mappen och kan raderas
//! när som helst. Tabeller:
//! - `ops`        – alla inlästa händelser (filens text)
//! - `seen_files` – vilka filer som redan lästs, och hur det gick
//! - `requests`   – härledda ärenden
//! - `op_result`  – om varje händelse tillämpades eller avvisades, och varför
//! - `meta`       – vilken mapp cachen hör till

use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::sync::Arc;

use rusqlite::{params, Connection};
use uuid::Uuid;

use crate::i18n::t;

use crate::model::{Location, Request};
use crate::ops::Op;
use crate::replay::{sort_ops, Outcome, State};

const SCHEMA_VERSION: i64 = 1;

const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS meta (
    key   TEXT PRIMARY KEY,
    value TEXT NOT NULL
);
CREATE TABLE IF NOT EXISTS ops (
    op_id      TEXT PRIMARY KEY,
    wall_ms    INTEGER NOT NULL,
    counter    INTEGER NOT NULL,
    client_id  TEXT NOT NULL,
    request_id TEXT NOT NULL,
    kind       TEXT NOT NULL,
    json       TEXT NOT NULL
);
CREATE TABLE IF NOT EXISTS seen_files (
    path     TEXT PRIMARY KEY,
    state    TEXT NOT NULL,
    attempts INTEGER NOT NULL,
    error    TEXT NOT NULL
);
CREATE TABLE IF NOT EXISTS requests (
    request_id   TEXT PRIMARY KEY,
    seq          INTEGER NOT NULL,
    title        TEXT NOT NULL,
    description  TEXT NOT NULL,
    sim_type     TEXT NOT NULL,
    priority     INTEGER NOT NULL,
    requester    TEXT NOT NULL,
    deadline     TEXT NOT NULL,
    input_kind   TEXT NOT NULL,
    input_ref    TEXT NOT NULL,
    status       TEXT NOT NULL,
    assignee     TEXT NOT NULL,
    created_at   INTEGER NOT NULL,
    started_at   INTEGER,
    finished_at  INTEGER,
    report_kind  TEXT NOT NULL,
    report_ref   TEXT NOT NULL,
    results_kind TEXT NOT NULL,
    results_ref  TEXT NOT NULL,
    summary      TEXT NOT NULL,
    note         TEXT NOT NULL
);
CREATE TABLE IF NOT EXISTS op_result (
    op_id   TEXT PRIMARY KEY,
    outcome TEXT NOT NULL,
    reason  TEXT NOT NULL
);
";

#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    #[error("{}: {0}", t("Fel i den lokala cachen", "Local cache error"))]
    Sql(#[from] rusqlite::Error),
    #[error("{}: {0}", t("Kan inte skapa cachen", "Cannot create the cache"))]
    Io(#[from] std::io::Error),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SeenState {
    /// Inläst (eller en dubblett av en redan inläst händelse).
    Ok,
    /// Gick inte att tolka ännu, försök igen vid nästa runda.
    Retry,
    /// Gick inte att tolka efter max antal försök.
    Corrupt,
    /// Komplett men ogiltig fil.
    Invalid,
}

impl SeenState {
    fn as_str(self) -> &'static str {
        match self {
            SeenState::Ok => "ok",
            SeenState::Retry => "retry",
            SeenState::Corrupt => "corrupt",
            SeenState::Invalid => "invalid",
        }
    }

    fn parse(s: &str) -> SeenState {
        match s {
            "ok" => SeenState::Ok,
            "corrupt" => SeenState::Corrupt,
            "invalid" => SeenState::Invalid,
            _ => SeenState::Retry,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Seen {
    pub state: SeenState,
    pub attempts: u32,
    pub error: String,
}

impl Seen {
    pub fn ok() -> Self {
        Seen {
            state: SeenState::Ok,
            attempts: 0,
            error: String::new(),
        }
    }
}

#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct IngestReport {
    /// Antal nya händelser.
    pub added: usize,
    /// Antal händelser som redan fanns (samma op_id).
    pub duplicates: usize,
    /// Sant om en ny händelse sorterades före redan tillämpade, så att hela
    /// loggen spelades upp från början.
    pub full_replay: bool,
    /// Händelser som tidigare var tillämpade men nu är avvisade.
    pub flipped: Vec<Uuid>,
}

pub struct Store {
    conn: Connection,
    ops: Vec<Arc<Op>>,
    ids: HashSet<Uuid>,
    state: State,
    seen: HashMap<String, Seen>,
}

impl Store {
    pub fn open(path: &Path) -> Result<Store, StoreError> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        Self::init(Connection::open(path)?)
    }

    /// Öppnar cachen. Går den inte att öppna (t.ex. skadad fil) kastas den
    /// och byggs om från den delade mappen.
    pub fn open_or_recreate(path: &Path) -> Result<Store, StoreError> {
        match Self::open(path) {
            Ok(store) => Ok(store),
            Err(_) => {
                for suffix in ["", "-wal", "-shm"] {
                    let mut file = path.as_os_str().to_owned();
                    file.push(suffix);
                    let _ = std::fs::remove_file(file);
                }
                Self::open(path)
            }
        }
    }

    pub fn open_in_memory() -> Result<Store, StoreError> {
        Self::init(Connection::open_in_memory()?)
    }

    fn init(conn: Connection) -> Result<Store, StoreError> {
        conn.execute_batch("PRAGMA journal_mode = WAL; PRAGMA synchronous = NORMAL;")?;
        let version: i64 = conn.query_row("PRAGMA user_version", [], |row| row.get(0))?;
        if version != 0 && version != SCHEMA_VERSION {
            conn.execute_batch(
                "DROP TABLE IF EXISTS meta; DROP TABLE IF EXISTS ops;
                 DROP TABLE IF EXISTS seen_files; DROP TABLE IF EXISTS requests;
                 DROP TABLE IF EXISTS op_result;",
            )?;
        }
        conn.execute_batch(SCHEMA)?;
        conn.pragma_update(None, "user_version", SCHEMA_VERSION)?;
        let mut store = Store {
            conn,
            ops: Vec::new(),
            ids: HashSet::new(),
            state: State::default(),
            seen: HashMap::new(),
        };
        store.reload()?;
        Ok(store)
    }

    /// Läser in händelserna från cachen och spelar upp dem från början.
    pub fn reload(&mut self) -> Result<(), StoreError> {
        self.ops.clear();
        self.ids.clear();
        self.seen.clear();
        {
            let mut stmt = self.conn.prepare("SELECT json FROM ops")?;
            let rows = stmt.query_map([], |row| row.get::<_, String>(0))?;
            for text in rows {
                if let Ok(op) = Op::parse(&text?) {
                    if self.ids.insert(op.op_id) {
                        self.ops.push(Arc::new(op));
                    }
                }
            }
        }
        {
            let mut stmt = self
                .conn
                .prepare("SELECT path, state, attempts, error FROM seen_files")?;
            let rows = stmt.query_map([], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, u32>(2)?,
                    row.get::<_, String>(3)?,
                ))
            })?;
            for row in rows {
                let (path, state, attempts, error) = row?;
                self.seen.insert(
                    path,
                    Seen {
                        state: SeenState::parse(&state),
                        attempts,
                        error,
                    },
                );
            }
        }
        sort_ops(&mut self.ops);
        self.state = State::replay_all(&self.ops);
        let tx = self.conn.unchecked_transaction()?;
        write_derived_full(&tx, &self.state)?;
        tx.commit()?;
        Ok(())
    }

    pub fn state(&self) -> &State {
        &self.state
    }

    /// Alla händelser i den totala ordningen.
    pub fn ops(&self) -> &[Arc<Op>] {
        &self.ops
    }

    pub fn seen(&self) -> &HashMap<String, Seen> {
        &self.seen
    }

    /// Tar emot nya händelser (i godtycklig ordning). Dubbletter ignoreras.
    /// Sorteras alla nya sist tillämpas de inkrementellt, annars spelas hela
    /// loggen upp från början i en transaktion.
    pub fn ingest(&mut self, incoming: Vec<Op>) -> Result<IngestReport, StoreError> {
        let mut report = IngestReport::default();
        let mut fresh: Vec<Arc<Op>> = Vec::new();
        let mut batch_ids = HashSet::new();
        for op in incoming {
            if self.ids.contains(&op.op_id) || !batch_ids.insert(op.op_id) {
                report.duplicates += 1;
            } else {
                fresh.push(Arc::new(op));
            }
        }
        if fresh.is_empty() {
            return Ok(report);
        }
        sort_ops(&mut fresh);
        match self.ingest_sorted(&fresh, &mut report) {
            Ok(()) => Ok(report),
            Err(e) => {
                // Minnet och databasen kan ha hamnat i otakt: läs om från databasen.
                let _ = self.reload();
                Err(e)
            }
        }
    }

    fn ingest_sorted(
        &mut self,
        fresh: &[Arc<Op>],
        report: &mut IngestReport,
    ) -> Result<(), StoreError> {
        let needs_full = match (self.ops.last(), fresh.first()) {
            (Some(last), Some(first)) => first.key() < last.key(),
            _ => false,
        };
        let tx = self.conn.unchecked_transaction()?;
        {
            let mut stmt = tx.prepare_cached(
                "INSERT OR IGNORE INTO ops (op_id, wall_ms, counter, client_id, request_id, kind, json)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            )?;
            for op in fresh {
                stmt.execute(params![
                    op.op_id.to_string(),
                    i64::try_from(op.hlc.wall_ms).unwrap_or(i64::MAX),
                    op.hlc.counter,
                    op.client_id.to_string(),
                    op.request_id.to_string(),
                    op.kind,
                    op.json,
                ])?;
            }
        }
        for op in fresh {
            self.ids.insert(op.op_id);
        }

        if needs_full {
            self.ops.extend(fresh.iter().cloned());
            sort_ops(&mut self.ops);
            let old = std::mem::take(&mut self.state);
            self.state = State::replay_all(&self.ops);
            for op in &self.ops {
                let was_applied = matches!(old.outcomes.get(&op.op_id), Some(Outcome::Applied));
                let now_rejected = matches!(
                    self.state.outcomes.get(&op.op_id),
                    Some(Outcome::Rejected { .. })
                );
                if was_applied && now_rejected {
                    report.flipped.push(op.op_id);
                }
            }
            write_derived_full(&tx, &self.state)?;
            report.full_replay = true;
        } else {
            let mut touched_ops = Vec::new();
            let mut touched_requests = HashSet::new();
            for op in fresh {
                touched_ops.extend(self.state.apply(op));
                touched_requests.insert(op.request_id);
                self.ops.push(Arc::clone(op));
            }
            for op_id in &touched_ops {
                if let Some(outcome) = self.state.outcomes.get(op_id) {
                    write_outcome(&tx, op_id, outcome)?;
                }
            }
            for request_id in &touched_requests {
                if let Some(request) = self.state.requests.get(request_id) {
                    write_request(&tx, request)?;
                }
            }
        }
        tx.commit()?;
        report.added = fresh.len();
        Ok(())
    }

    /// Sparar hur det gick att läsa ett antal filer.
    pub fn mark_seen(&mut self, updates: Vec<(String, Seen)>) -> Result<(), StoreError> {
        if updates.is_empty() {
            return Ok(());
        }
        let tx = self.conn.unchecked_transaction()?;
        {
            let mut stmt = tx.prepare_cached(
                "INSERT INTO seen_files (path, state, attempts, error) VALUES (?1, ?2, ?3, ?4)
                 ON CONFLICT(path) DO UPDATE SET
                     state = excluded.state, attempts = excluded.attempts, error = excluded.error",
            )?;
            for (path, seen) in &updates {
                stmt.execute(params![
                    path,
                    seen.state.as_str(),
                    seen.attempts,
                    seen.error
                ])?;
            }
        }
        tx.commit()?;
        self.seen.extend(updates);
        Ok(())
    }

    /// Tömmer cachen helt. Nästa synk läser om allt från mappen.
    pub fn clear(&mut self) -> Result<(), StoreError> {
        self.conn.execute_batch(
            "BEGIN;
             DELETE FROM ops; DELETE FROM seen_files;
             DELETE FROM requests; DELETE FROM op_result;
             COMMIT;",
        )?;
        self.ops.clear();
        self.ids.clear();
        self.seen.clear();
        self.state = State::default();
        Ok(())
    }

    pub fn meta_get(&self, key: &str) -> Result<Option<String>, StoreError> {
        let mut stmt = self
            .conn
            .prepare_cached("SELECT value FROM meta WHERE key = ?1")?;
        let mut rows = stmt.query(params![key])?;
        match rows.next()? {
            Some(row) => Ok(Some(row.get(0)?)),
            None => Ok(None),
        }
    }

    pub fn meta_set(&self, key: &str, value: &str) -> Result<(), StoreError> {
        self.conn.execute(
            "INSERT INTO meta (key, value) VALUES (?1, ?2)
             ON CONFLICT(key) DO UPDATE SET value = excluded.value",
            params![key, value],
        )?;
        Ok(())
    }

    /// De härledda tabellerna som text, i fast ordning. Används för att
    /// jämföra två cachar med varandra.
    pub fn derived_dump(&self) -> Result<Vec<String>, StoreError> {
        let mut out = Vec::new();
        for (table, key) in [("requests", "request_id"), ("op_result", "op_id")] {
            let mut stmt = self
                .conn
                .prepare(&format!("SELECT * FROM {table} ORDER BY {key}"))?;
            let columns = stmt.column_count();
            let mut rows = stmt.query([])?;
            while let Some(row) = rows.next()? {
                let mut line = table.to_string();
                for i in 0..columns {
                    let value: rusqlite::types::Value = row.get(i)?;
                    line.push_str(&format!("|{value:?}"));
                }
                out.push(line);
            }
        }
        Ok(out)
    }
}

fn outcome_columns(outcome: &Outcome) -> (&'static str, String) {
    match outcome {
        Outcome::Applied => ("applied", String::new()),
        Outcome::Rejected { reason, .. } => ("rejected", reason.text()),
        Outcome::Waiting => (
            "waiting",
            t(
                "Väntar på att ärendet ska synkas",
                "Waiting for the request to sync",
            )
            .to_string(),
        ),
        Outcome::Ignored => (
            "ignored",
            t(
                "Kräver en nyare version av appen",
                "Requires a newer version of the app",
            )
            .to_string(),
        ),
    }
}

fn write_outcome(conn: &Connection, op_id: &Uuid, outcome: &Outcome) -> Result<(), StoreError> {
    let (name, reason) = outcome_columns(outcome);
    conn.prepare_cached(
        "INSERT OR REPLACE INTO op_result (op_id, outcome, reason) VALUES (?1, ?2, ?3)",
    )?
    .execute(params![op_id.to_string(), name, reason])?;
    Ok(())
}

fn location_columns(location: &Option<Location>) -> (&str, &str) {
    match location {
        Some(l) => (l.kind.as_str(), l.value.as_str()),
        None => ("", ""),
    }
}

fn millis(value: u64) -> i64 {
    i64::try_from(value).unwrap_or(i64::MAX)
}

fn write_request(conn: &Connection, r: &Request) -> Result<(), StoreError> {
    let (input_kind, input_ref) = location_columns(&r.input);
    let (report_kind, report_ref) = location_columns(&r.report);
    let (results_kind, results_ref) = location_columns(&r.results);
    conn.prepare_cached(
        "INSERT OR REPLACE INTO requests (
             request_id, seq, title, description, sim_type, priority, requester, deadline,
             input_kind, input_ref, status, assignee, created_at, started_at, finished_at,
             report_kind, report_ref, results_kind, results_ref, summary, note)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15,
                 ?16, ?17, ?18, ?19, ?20, ?21)",
    )?
    .execute(params![
        r.request_id.to_string(),
        r.seq,
        r.title,
        r.description,
        r.sim_type,
        r.priority,
        r.requester,
        r.deadline,
        input_kind,
        input_ref,
        r.status.as_str(),
        r.assignee,
        millis(r.created_at),
        r.started_at.map(millis),
        r.finished_at.map(millis),
        report_kind,
        report_ref,
        results_kind,
        results_ref,
        r.summary,
        r.note,
    ])?;
    Ok(())
}

/// Ersätter de härledda tabellerna med resultatet av en full uppspelning.
fn write_derived_full(conn: &Connection, state: &State) -> Result<(), StoreError> {
    conn.execute_batch("DELETE FROM requests; DELETE FROM op_result;")?;
    for request in state.requests.values() {
        write_request(conn, request)?;
    }
    for (op_id, outcome) in &state.outcomes {
        write_outcome(conn, op_id, outcome)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::Status;
    use crate::ops::NewAction;
    use crate::replay::Reject;
    use crate::testutil::{create_action, finish_action, op_at, take_action, Rng, TempDir};

    /// En blandad uppsättning händelser: flera ärenden, konkurrerande take,
    /// ogiltiga övergångar, händelser utan create och okända sorter.
    fn scenario() -> Vec<Op> {
        let clients: Vec<Uuid> = (0..4).map(|_| Uuid::new_v4()).collect();
        let names = ["Anna", "Bo", "Cia", "Dan"];
        let mut ops = Vec::new();
        let mut t = 1_794_000_000_000u64;
        for i in 0..12usize {
            let id = Uuid::new_v4();
            let c = |k: usize| clients[(i + k) % 4];
            let n = |k: usize| names[(i + k) % 4];
            t += 1000;
            ops.push(op_at(
                c(0),
                t,
                0,
                n(0),
                id,
                &create_action(&format!("Ärende {i}")),
            ));
            // Två som tar samtidigt, ibland på exakt samma millisekund.
            let same_ms = i % 3 == 0;
            ops.push(op_at(c(1), t + 500, 0, n(1), id, &take_action(n(1))));
            ops.push(op_at(
                c(2),
                if same_ms { t + 500 } else { t + 501 },
                0,
                n(2),
                id,
                &take_action(n(2)),
            ));
            match i % 4 {
                0 => ops.push(op_at(c(1), t + 600, 0, n(1), id, &finish_action(n(1)))),
                1 => {
                    ops.push(op_at(
                        c(3),
                        t + 600,
                        0,
                        n(3),
                        id,
                        &NewAction::Fail {
                            note: "Divergerade".to_string(),
                        },
                    ));
                    ops.push(op_at(c(0), t + 700, 0, n(0), id, &NewAction::Requeue));
                    ops.push(op_at(c(0), t + 700, 1, n(0), id, &NewAction::Requeue));
                }
                2 => {
                    ops.push(op_at(
                        c(0),
                        t + 600,
                        0,
                        n(0),
                        id,
                        &NewAction::Cancel {
                            note: String::new(),
                        },
                    ));
                    ops.push(op_at(c(1), t + 601, 0, n(1), id, &finish_action(n(1))));
                }
                _ => {
                    // Take med tidigare stämpel än create (klocka som går fel).
                    ops.push(op_at(c(3), t - 400, 0, n(3), id, &take_action(n(3))));
                }
            }
        }
        // Händelse för ett ärende som aldrig skapas.
        ops.push(op_at(
            clients[0],
            t + 5000,
            0,
            "Anna",
            Uuid::new_v4(),
            &take_action("Anna"),
        ));
        // Okänd sort från en nyare app.
        let template = op_at(
            clients[1],
            t + 6000,
            0,
            "Bo",
            Uuid::new_v4(),
            &NewAction::Requeue,
        );
        let mut value: serde_json::Value = serde_json::from_str(&template.json).unwrap();
        value["kind"] = serde_json::json!("archive");
        ops.push(Op::parse(&value.to_string()).unwrap());
        ops
    }

    fn fingerprint(store: &Store) -> (State, Vec<String>) {
        (store.state().clone(), store.derived_dump().unwrap())
    }

    #[test]
    fn any_arrival_order_gives_the_same_final_state() {
        let ops = scenario();
        let mut canonical = Store::open_in_memory().unwrap();
        canonical.ingest(ops.clone()).unwrap();
        let expected = fingerprint(&canonical);
        assert_eq!(expected.0.requests.len(), 12);
        assert!(expected.0.unsupported_seen);

        let mut rng = Rng::new(20_261_111);
        let mut full_replays = 0;
        for round in 0..120 {
            let mut shuffled = ops.clone();
            rng.shuffle(&mut shuffled);
            // Lägg till några dubbletter på slumpade platser.
            for _ in 0..5 {
                let dup = shuffled[rng.below(shuffled.len())].clone();
                let at = rng.below(shuffled.len());
                shuffled.insert(at, dup);
            }
            let mut store = Store::open_in_memory().unwrap();
            // Varierande klumpstorlek: en i taget, små klumpar eller allt på en gång.
            let chunk = match round % 3 {
                0 => 1,
                1 => 1 + rng.below(7),
                _ => shuffled.len(),
            };
            for part in shuffled.chunks(chunk) {
                if store.ingest(part.to_vec()).unwrap().full_replay {
                    full_replays += 1;
                }
            }
            assert_eq!(store.ops().len(), ops.len());
            assert_eq!(fingerprint(&store), expected, "omgång {round}");
        }
        assert!(full_replays > 100);
    }

    #[test]
    fn late_take_with_earlier_hlc_flips_the_result() {
        let (a, b, c) = (Uuid::new_v4(), Uuid::new_v4(), Uuid::new_v4());
        let id = Uuid::new_v4();
        let create = op_at(a, 1000, 0, "Anna", id, &create_action("Upright"));
        let take_bo = op_at(b, 2500, 0, "Bo", id, &take_action("Bo"));
        let take_cia = op_at(c, 2000, 0, "Cia", id, &take_action("Cia"));

        let mut store = Store::open_in_memory().unwrap();
        let report = store.ingest(vec![create]).unwrap();
        assert!(!report.full_replay);
        let report = store.ingest(vec![take_bo.clone()]).unwrap();
        assert!(!report.full_replay, "sist i ordningen: inkrementellt");
        assert!(report.flipped.is_empty());
        assert_eq!(store.state().requests[&id].assignee, "Bo");
        assert_eq!(store.state().outcomes[&take_bo.op_id], Outcome::Applied);

        // Cias take skapades tidigare men kommer fram senare via Drive.
        let report = store.ingest(vec![take_cia.clone()]).unwrap();
        assert!(report.full_replay);
        assert_eq!(report.flipped, vec![take_bo.op_id]);
        let request = &store.state().requests[&id];
        assert_eq!(request.status, Status::Running);
        assert_eq!(request.assignee, "Cia");
        assert_eq!(request.started_at, Some(2000));
        assert_eq!(store.state().outcomes[&take_cia.op_id], Outcome::Applied);
        assert!(matches!(
            store.state().outcomes[&take_bo.op_id],
            Outcome::Rejected {
                reason: Reject::WrongStatus(Status::Running),
                ..
            }
        ));

        // Avvisningen och anledningen sparas i op_result.
        let dump = store.derived_dump().unwrap();
        assert!(dump.iter().any(|line| line.starts_with("op_result")
            && line.contains(&take_bo.op_id.to_string())
            && line.contains("rejected")
            && line.contains("Pågår")));
    }

    #[test]
    fn duplicate_op_ids_are_ignored() {
        let a = Uuid::new_v4();
        let id = Uuid::new_v4();
        let create = op_at(a, 1000, 0, "Anna", id, &create_action("Upright"));
        let take = op_at(a, 2000, 0, "Bo", id, &take_action("Bo"));
        let mut store = Store::open_in_memory().unwrap();
        let report = store
            .ingest(vec![create.clone(), take.clone(), take.clone()])
            .unwrap();
        assert_eq!((report.added, report.duplicates), (2, 1));
        let before = fingerprint(&store);
        let report = store.ingest(vec![take, create]).unwrap();
        assert_eq!((report.added, report.duplicates), (0, 2));
        assert_eq!(fingerprint(&store), before);
        assert_eq!(store.ops().len(), 2);
    }

    #[test]
    fn op_for_unknown_request_is_applied_when_create_arrives() {
        let (a, b) = (Uuid::new_v4(), Uuid::new_v4());
        let id = Uuid::new_v4();
        let create = op_at(a, 1000, 0, "Anna", id, &create_action("Upright"));
        let take = op_at(b, 2000, 0, "Bo", id, &take_action("Bo"));

        let mut store = Store::open_in_memory().unwrap();
        store.ingest(vec![take.clone()]).unwrap();
        assert!(store.state().requests.is_empty());
        assert_eq!(store.state().outcomes[&take.op_id], Outcome::Waiting);

        let report = store.ingest(vec![create]).unwrap();
        assert!(report.full_replay);
        assert_eq!(store.state().requests[&id].status, Status::Running);
        assert_eq!(store.state().requests[&id].assignee, "Bo");
        assert_eq!(store.state().outcomes[&take.op_id], Outcome::Applied);
    }

    #[test]
    fn cache_survives_reopen_and_can_be_cleared() {
        let tmp = TempDir::new();
        let path = tmp.path().join("cache.sqlite");
        let ops = scenario();
        let expected = {
            let mut store = Store::open(&path).unwrap();
            store.ingest(ops.clone()).unwrap();
            store
                .mark_seen(vec![("2026-11/x.json".to_string(), Seen::ok())])
                .unwrap();
            store.meta_set("folder", "/tmp/SimQ").unwrap();
            fingerprint(&store)
        };
        let mut store = Store::open(&path).unwrap();
        assert_eq!(fingerprint(&store), expected);
        assert_eq!(store.seen().len(), 1);
        assert_eq!(
            store.meta_get("folder").unwrap().as_deref(),
            Some("/tmp/SimQ")
        );

        store.clear().unwrap();
        assert!(store.ops().is_empty());
        assert!(store.seen().is_empty());
        assert!(store.derived_dump().unwrap().is_empty());
        drop(store);

        // En skadad cachefil kastas och ersätts.
        std::fs::write(&path, b"detta ar inte en databas").unwrap();
        let store = Store::open_or_recreate(&path).unwrap();
        assert!(store.ops().is_empty());
    }

    #[test]
    fn full_replay_of_10_000_ops_with_cache_write_is_fast() {
        let clients: Vec<Uuid> = (0..8).map(|_| Uuid::new_v4()).collect();
        let mut ops = Vec::with_capacity(10_000);
        let mut t = 1_794_000_000_000u64;
        for i in 0..2_500usize {
            let id = Uuid::new_v4();
            for action in [
                create_action("Lastfall"),
                take_action("Bo"),
                take_action("Cia"),
                finish_action("Bo"),
            ] {
                t += 7;
                ops.push(op_at(clients[i % 8], t, 0, "Bo", id, &action));
            }
        }
        // En händelse som sorteras allra först tvingar fram full uppspelning.
        let early = ops.remove(0);
        let mut store = Store::open_in_memory().unwrap();
        store.ingest(ops).unwrap();
        assert_eq!(store.ops().len(), 9_999);

        let started = std::time::Instant::now();
        let report = store.ingest(vec![early]).unwrap();
        let elapsed = started.elapsed();
        assert!(report.full_replay);
        assert_eq!(store.ops().len(), 10_000);
        assert_eq!(store.state().requests.len(), 2_500);
        assert!(
            elapsed < std::time::Duration::from_secs(1),
            "full uppspelning tog {elapsed:?}"
        );
    }
}
