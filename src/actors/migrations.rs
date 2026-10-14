//! Persistent format transitions must not turn retired guest inputs into Owner authority.
use rusqlite::{Transaction, params};

pub(super) fn retire_guest_execution(tx: &Transaction<'_>, now: i64) -> rusqlite::Result<()> {
    if tx.query_row(
        "SELECT EXISTS(SELECT 1 FROM stage_public_meta WHERE key='retired_guest_execution')",
        [],
        |row| row.get::<_, bool>(0),
    )? {
        return Ok(());
    }
    // Keep archived facts. Retire only unfinished inputs admitted through the removed guest path.
    tx.execute(
        "UPDATE messages SET status='\"interrupted\"', record=json_set(record,
            '$.status','interrupted', '$.cancel_requested',json('true'),
            '$.error','Guest access was retired', '$.finished_at_ms',?1, '$.updated_at_ms',?1)
         WHERE json_extract(record,'$.origin_share_id') IS NOT NULL
           AND status IN ('\"queued\"','\"running\"')",
        params![now],
    )?;
    tx.execute(
        "UPDATE approvals SET record=json_set(record,'$.decision','cancelled','$.resolved_at_ms',?1)
         WHERE json_extract(record,'$.decision')='pending'
           AND message_id IN (SELECT id FROM messages
             WHERE json_extract(record,'$.origin_share_id') IS NOT NULL)",
        params![now],
    )?;
    tx.execute(
        "INSERT INTO stage_public_meta VALUES('retired_guest_execution','1')",
        [],
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{Value, json};

    #[test]
    fn retired_guest_requests_and_reviews_cannot_execute_as_owner_after_upgrade() {
        let mut db = rusqlite::Connection::open_in_memory().unwrap();
        db.execute_batch(
            "CREATE TABLE stage_public_meta(key TEXT PRIMARY KEY,value TEXT);
            CREATE TABLE messages(id TEXT,status TEXT,record TEXT);
            CREATE TABLE approvals(message_id TEXT,record TEXT);",
        )
        .unwrap();
        for (id, origin, status) in [
            ("owner", Value::Null, "queued"),
            ("guest", json!("former-access"), "queued"),
            ("running", json!("former-access"), "running"),
            ("archived", json!("former-access"), "completed"),
        ] {
            let record =
                json!({"origin_share_id":origin,"status":status,"content":"retain original input"});
            db.execute(
                "INSERT INTO messages VALUES(?1,?2,?3)",
                params![
                    id,
                    serde_json::to_string(status).unwrap(),
                    record.to_string()
                ],
            )
            .unwrap();
            db.execute(
                "INSERT INTO approvals VALUES(?1,?2)",
                params![id, json!({"decision":"pending"}).to_string()],
            )
            .unwrap();
        }
        let tx = db.transaction().unwrap();
        retire_guest_execution(&tx, 123).unwrap();
        tx.commit().unwrap();
        let record = |id: &str| -> Value {
            serde_json::from_str(
                &db.query_row("SELECT record FROM messages WHERE id=?1", [id], |row| {
                    row.get::<_, String>(0)
                })
                .unwrap(),
            )
            .unwrap()
        };
        assert_eq!(record("owner")["status"], "queued");
        assert_eq!(record("archived")["status"], "completed");
        for id in ["guest", "running"] {
            let value = record(id);
            assert_eq!(value["status"], "interrupted");
            assert_eq!(value["cancel_requested"], true);
            assert_eq!(value["content"], "retain original input");
            assert_eq!(
                db.query_row(
                    "SELECT json_extract(record,'$.decision') FROM approvals WHERE message_id=?1",
                    [id],
                    |row| row.get::<_, String>(0)
                )
                .unwrap(),
                "cancelled"
            );
        }
        assert_eq!(
            db.query_row(
                "SELECT json_extract(record,'$.decision') FROM approvals WHERE message_id='owner'",
                [],
                |row| row.get::<_, String>(0)
            )
            .unwrap(),
            "pending"
        );
    }
}
