//! Helpers for the tests.

use std::path::{Path, PathBuf};

use uuid::Uuid;

use crate::hlc::Hlc;
use crate::model::{LocKind, Location};
use crate::ops::{build_op, NewAction, Op};

/// Temporary folder that is removed when dropped.
pub struct TempDir(PathBuf);

impl TempDir {
    pub fn new() -> Self {
        let path = std::env::temp_dir().join(format!("simq-test-{}", Uuid::new_v4()));
        std::fs::create_dir_all(&path).expect("can create temp folder");
        TempDir(path)
    }

    pub fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// Small deterministic random generator (xorshift) so that the tests do not need
/// an extra dependency.
pub struct Rng(u64);

impl Rng {
    pub fn new(seed: u64) -> Self {
        Rng(seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1)
    }

    pub fn next(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.0 = x;
        x
    }

    pub fn below(&mut self, n: usize) -> usize {
        (self.next() % n.max(1) as u64) as usize
    }

    pub fn shuffle<T>(&mut self, items: &mut [T]) {
        for i in (1..items.len()).rev() {
            let j = self.below(i + 1);
            items.swap(i, j);
        }
    }
}

pub fn create_action(title: &str) -> NewAction {
    NewAction::Create {
        title: title.to_string(),
        description: "Beskrivning med å, ä och ö".to_string(),
        sim_type: "FEA".to_string(),
        priority: 2,
        requester: "Anna".to_string(),
        deadline: "2026-12-01".to_string(),
        input: Some(Location {
            kind: LocKind::Vault,
            value: "Uprights/v3/Assembly.iam".to_string(),
        }),
    }
}

pub fn take_action(assignee: &str) -> NewAction {
    NewAction::Take {
        assignee: assignee.to_string(),
    }
}

pub fn finish_action(assignee: &str) -> NewAction {
    NewAction::Finish {
        assignee: assignee.to_string(),
        report: Some(Location {
            kind: LocKind::Drive,
            value: "https://drive.google.com/file/d/rapport".to_string(),
        }),
        results: Some(Location {
            kind: LocKind::Vault,
            value: "Uprights/v3/Results".to_string(),
        }),
        summary: "Max spänning 212 MPa, säkerhetsfaktor 1,8.".to_string(),
    }
}

/// Builds an event with an explicit timestamp.
pub fn op_at(
    client_id: Uuid,
    wall_ms: u64,
    counter: u32,
    by: &str,
    request_id: Uuid,
    action: &NewAction,
) -> Op {
    build_op(client_id, Hlc { wall_ms, counter }, by, request_id, action).expect("valid test event")
}
