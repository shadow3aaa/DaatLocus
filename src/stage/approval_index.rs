//! Local pagination keys for source-owned approvals. Resolution always goes to the source.
use super::{Result, StageId};
use crate::actors::store::ActorStore;
use rusqlite::{Transaction, params};
pub(crate) fn remember(
    tx: &Transaction<'_>,
    stage: &str,
    id: &str,
    actor: &str,
    pending: bool,
) -> rusqlite::Result<()> {
    tx.execute_batch("CREATE TABLE IF NOT EXISTS account_approval_refs(seq INTEGER PRIMARY KEY AUTOINCREMENT,stage TEXT NOT NULL,id TEXT NOT NULL,actor TEXT NOT NULL,pending INTEGER NOT NULL,UNIQUE(stage,id));CREATE INDEX IF NOT EXISTS account_approval_pending ON account_approval_refs(pending,seq);")?;
    tx.execute("INSERT INTO account_approval_refs(stage,id,actor,pending) VALUES(?1,?2,?3,?4) ON CONFLICT(stage,id) DO UPDATE SET pending=excluded.pending",params![stage,id,actor,pending])?;
    Ok(())
}
pub fn record(
    store: &ActorStore,
    stage: StageId,
    id: &str,
    actor: &str,
    pending: bool,
) -> Result<()> {
    store.stage_transaction(|tx| {
        remember(tx, &stage.to_string(), id, actor, pending)?;
        Ok(())
    })
}
pub type ApprovalReferences = (Vec<(i64, String, String)>, Option<i64>);
pub fn page(
    store: &ActorStore,
    actor: Option<&str>,
    before: Option<i64>,
    limit: usize,
) -> Result<ApprovalReferences> {
    store.stage_transaction(|tx|{
        remember(tx,"","__initialize__","",false)?;
        tx.execute("DELETE FROM account_approval_refs WHERE stage='' AND id='__initialize__'",[])?;
        tx.execute_batch("CREATE TABLE IF NOT EXISTS stage_local_approvals(id TEXT PRIMARY KEY);")?;
        tx.execute("INSERT OR IGNORE INTO account_approval_refs(stage,id,actor,pending) SELECT '',a.id,a.actor_id,0 FROM approvals a JOIN stage_local_approvals n ON n.id=a.id",[])?;
        tx.execute("UPDATE account_approval_refs SET pending=COALESCE((SELECT json_extract(a.record,'$.mode')='ask' AND json_extract(a.record,'$.decision')='pending' FROM approvals a WHERE a.id=account_approval_refs.id),0) WHERE stage=''",[])?;
        let mut rows=tx.prepare("SELECT seq,stage,id FROM account_approval_refs WHERE pending=1 AND seq<?1 AND (?2 IS NULL OR actor=?2) ORDER BY seq DESC LIMIT ?3")?.query_map(params![before.unwrap_or(i64::MAX),actor,limit as i64+1],|r|Ok((r.get::<_,i64>(0)?,r.get::<_,String>(1)?,r.get::<_,String>(2)?)))?.collect::<std::result::Result<Vec<_>,_>>()?;
        let next=if rows.len()>limit{rows.truncate(limit);rows.last().map(|r|r.0)}else{None};Ok((rows,next))
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn source_sequences_do_not_collide_and_closed_approvals_leave_the_bounded_index() {
        let dir = tempfile::tempdir().unwrap();
        let store = ActorStore::open(&dir.path().join("actors.sqlite")).unwrap();
        let a = StageId::new();
        let b = StageId::new();
        record(&store, a, "same-id", "core", true).unwrap();
        record(&store, b, "same-id", "core", true).unwrap();
        let (first, next) = page(&store, Some("core"), None, 1).unwrap();
        assert_eq!(first.len(), 1);
        assert_eq!(first[0].1, b.to_string());
        let (second, next2) = page(&store, Some("core"), next, 1).unwrap();
        assert_eq!(second[0].1, a.to_string());
        assert!(next2.is_none());
        record(&store, b, "same-id", "core", false).unwrap();
        let (pending, _) = page(&store, None, None, 2).unwrap();
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].1, a.to_string());
    }
}
