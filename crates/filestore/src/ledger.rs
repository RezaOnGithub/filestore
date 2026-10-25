//! Command acceptance: idempotent submission of user actions.
//!
//! A client may attach a unique request ID to a mutating command. The first
//! acceptance records the operation, a hash of its payload, and its result
//! in the same transaction as the operation itself. A retry with the same ID
//! and payload returns the recorded result without repeating the operation;
//! reusing the ID for anything else is rejected. This simplifies DESIGN.md
//! §10.1 by omitting the server-issued witness.

use rusqlite::{OptionalExtension, params};
use serde::Serialize;
use serde_json::Value;

use crate::catalog::atomic;
use crate::ids::now_ms;
use crate::{Catalog, Error, Result};

/// Run `f` under `request_id`. Returns the result and whether it was newly
/// executed (`false` for a replayed retry).
pub fn run<T: Serialize>(
    cat: &Catalog,
    request_id: Option<&str>,
    operation: &str,
    payload: &Value,
    f: impl FnOnce(&Catalog) -> Result<T>,
) -> Result<(Value, bool)> {
    let Some(rid) = request_id.filter(|r| !r.is_empty()) else {
        return Ok((serde_json::to_value(f(cat)?)?, true));
    };
    let hash = blake3::hash(payload.to_string().as_bytes()).to_hex().to_string();
    let tx = atomic(&cat.conn)?;
    let existing: Option<(String, String, String)> = tx
        .query_row(
            "SELECT operation, payload_hash, result FROM command_ledger WHERE request_id = ?1",
            [rid],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .optional()?;
    if let Some((op, h, result)) = existing {
        if op == operation && h == hash {
            return Ok((serde_json::from_str(&result)?, false));
        }
        return Err(Error::Conflict(
            "this request ID was already used for a different request".into(),
        ));
    }
    let value = serde_json::to_value(f(cat)?)?;
    tx.execute(
        "INSERT INTO command_ledger (request_id, operation, payload_hash, result, created_at)
         VALUES (?1, ?2, ?3, ?4, ?5)",
        params![rid, operation, hash, value.to_string(), now_ms()],
    )?;
    tx.commit()?;
    Ok((value, true))
}
