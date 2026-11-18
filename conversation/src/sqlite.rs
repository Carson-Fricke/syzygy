//! Local WAL-backed conversation log. A user event itself is durable runnable
//! work, recovered by folding its chat at startup.

use std::{path::Path, time::Duration};

use rusqlite::{Connection, TransactionBehavior, params};

use crate::{Error, Event, Record, Store};

pub struct SqliteStore {
    db: Connection,
}

impl SqliteStore {
    pub fn open(path: impl AsRef<Path>) -> Result<Self, Error> {
        let db = Connection::open(path).map_err(storage)?;
        db.busy_timeout(Duration::from_secs(5)).map_err(storage)?;
        db.execute_batch(
            "PRAGMA journal_mode = WAL;
             PRAGMA synchronous = FULL;
             CREATE TABLE IF NOT EXISTS syzygy_conversation_events (
                 chat TEXT NOT NULL,
                 sequence INTEGER NOT NULL CHECK(sequence > 0),
                 turn_id TEXT NOT NULL,
                 event TEXT NOT NULL,
                 PRIMARY KEY (chat, sequence)
             ) WITHOUT ROWID;",
        )
        .map_err(storage)?;
        Ok(Self { db })
    }
}

fn storage<E: std::error::Error + Send + Sync + 'static>(error: E) -> Error {
    Error::Store(Box::new(error))
}

impl Store for SqliteStore {
    fn compare_and_append(
        &mut self,
        chat: &str,
        expected: u64,
        turn_id: &str,
        event: &Event,
    ) -> Result<u64, Error> {
        let json = serde_json::to_string(event).map_err(storage)?;
        let tx = self
            .db
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(storage)?;
        let current: i64 = tx
            .query_row(
                "SELECT COALESCE(MAX(sequence), 0) FROM syzygy_conversation_events WHERE chat = ?1",
                [chat],
                |row| row.get(0),
            )
            .map_err(storage)?;
        if u64::try_from(current).map_err(storage)? != expected {
            return Err(Error::Conflict);
        }
        let next = current
            .checked_add(1)
            .ok_or_else(|| Error::Invalid("conversation sequence exhausted".into()))?;
        tx.execute(
            "INSERT INTO syzygy_conversation_events(chat, sequence, turn_id, event) VALUES (?1, ?2, ?3, ?4)",
            params![chat, next, turn_id, json],
        ).map_err(storage)?;
        tx.commit().map_err(storage)?;
        Ok(next as u64)
    }

    fn read_after(&self, chat: &str, after: u64, limit: usize) -> Result<Vec<Record>, Error> {
        let Ok(after) = i64::try_from(after) else {
            return Ok(Vec::new());
        };
        let mut statement = self
            .db
            .prepare_cached(
                "SELECT sequence, turn_id, event FROM syzygy_conversation_events
             WHERE chat = ?1 AND sequence > ?2 ORDER BY sequence LIMIT ?3",
            )
            .map_err(storage)?;
        let mut rows = statement
            .query(params![
                chat,
                after,
                i64::try_from(limit).unwrap_or(i64::MAX)
            ])
            .map_err(storage)?;
        let mut records = Vec::new();
        while let Some(row) = rows.next().map_err(storage)? {
            let json: String = row.get(2).map_err(storage)?;
            records.push(Record {
                sequence: u64::try_from(row.get::<_, i64>(0).map_err(storage)?).map_err(storage)?,
                turn_id: row.get(1).map_err(storage)?,
                event: serde_json::from_str(&json).map_err(storage)?,
            });
        }
        Ok(records)
    }

    fn chats(&self) -> Result<Vec<String>, Error> {
        let mut statement = self
            .db
            .prepare_cached("SELECT DISTINCT chat FROM syzygy_conversation_events ORDER BY chat")
            .map_err(storage)?;
        statement
            .query_map([], |row| row.get(0))
            .map_err(storage)?
            .collect::<Result<Vec<_>, _>>()
            .map_err(storage)
    }
}
