//! 导入记录存储。
//!
//! # 为什么需要它
//!
//! 需求是「课表变化时能一键清空 App 添加的日程，**只清 App 添加的**」。
//! 系统日历是公共数据库，无法只靠查询区分「哪些是别的 App 写的」。
//! 因此本模块持久化每一次导入的结果：
//!
//! * `import_batch` —— 一次导入（一次点「导入课表」）的元信息；
//! * `imported_event` —— 该批次写入系统日历的每一条日程，含**系统日历返回的
//!   事件 `_ID`** 与业务**指纹**。
//!
//! 清除时优先用记录的 `_ID` 精确删除；若某条 `_ID` 已失效（用户手动删了、
//! 或系统回收了），再按指纹兜底匹配。这样既不会误删其他 App 的日程，
//! 也不会因为用户手动改动而残留垃圾。
//!
//! 另外，指纹还让**重复导入同一份课表变得幂等**：同一指纹只会写入一次。

use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};

use crate::models::CalendarEvent;

/// 一次导入的批次摘要。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ImportBatch {
    pub id: i64,
    /// Unix 时间戳（秒）
    pub created_at: i64,
    /// 学期第一周周一的日期
    pub term_start: String,
    /// 课表来源描述，例如「jwgl 直连」/「WebVPN」
    pub source: String,
    /// 该批次写入的日程条数
    pub event_count: i64,
}

/// 写入系统日历后回填的日历事件信息。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EventRef {
    pub fingerprint: String,
    /// 系统日历 CalendarContract.Events._ID
    pub calendar_event_id: i64,
    pub title: String,
    pub start: String,
    pub end: String,
    pub location: String,
}

/// 导入结果统计。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ImportOutcome {
    pub batch_id: i64,
    /// 新写入的日程数
    pub inserted: usize,
    /// 因为指纹重复而跳过的日程数（幂等导入）
    pub skipped: usize,
}

/// 记录库。
pub struct Store {
    conn: Connection,
}

impl Store {
    /// 打开（或创建）指定路径的记录库。
    pub fn open(path: &str) -> Result<Self, String> {
        let conn = Connection::open(path).map_err(|e| format!("打不开记录库 {path}: {e}"))?;
        let store = Store { conn };
        store.migrate()?;
        Ok(store)
    }

    /// 内存库，仅用于测试。
    pub fn open_in_memory() -> Result<Self, String> {
        let conn = Connection::open_in_memory().map_err(|e| format!("建不了内存库: {e}"))?;
        let store = Store { conn };
        store.migrate()?;
        Ok(store)
    }

    fn migrate(&self) -> Result<(), String> {
        self.conn
            .execute_batch(
                r#"
                PRAGMA journal_mode = WAL;
                CREATE TABLE IF NOT EXISTS import_batch (
                    id          INTEGER PRIMARY KEY AUTOINCREMENT,
                    created_at  INTEGER NOT NULL,
                    term_start  TEXT    NOT NULL,
                    source      TEXT    NOT NULL DEFAULT '',
                    event_count INTEGER NOT NULL DEFAULT 0
                );
                CREATE TABLE IF NOT EXISTS imported_event (
                    fingerprint       TEXT PRIMARY KEY,
                    batch_id          INTEGER NOT NULL,
                    calendar_event_id INTEGER,
                    title             TEXT NOT NULL,
                    start             TEXT NOT NULL,
                    end               TEXT NOT NULL,
                    location          TEXT NOT NULL DEFAULT '',
                    FOREIGN KEY (batch_id) REFERENCES import_batch(id) ON DELETE CASCADE
                );
                CREATE INDEX IF NOT EXISTS idx_imported_event_batch
                    ON imported_event(batch_id);
                "#,
            )
            .map_err(|e| format!("初始化记录库失败: {e}"))
    }

    /// 开始一个新批次。
    pub fn begin_batch(&self, term_start: &str, source: &str, created_at: i64) -> Result<i64, String> {
        self.conn
            .execute(
                "INSERT INTO import_batch (created_at, term_start, source) VALUES (?1, ?2, ?3)",
                params![created_at, term_start, source],
            )
            .map_err(|e| format!("创建导入批次失败: {e}"))?;
        Ok(self.conn.last_insert_rowid())
    }

    /// 该指纹是否已经导入过。
    pub fn fingerprint_exists(&self, fingerprint: &str) -> Result<bool, String> {
        let found: Option<i64> = self
            .conn
            .query_row(
                "SELECT 1 FROM imported_event WHERE fingerprint = ?1",
                params![fingerprint],
                |row| row.get(0),
            )
            .optional()
            .map_err(|e| format!("查询指纹失败: {e}"))?;
        Ok(found.is_some())
    }

    /// 记录一条已经写入系统日历的日程。
    ///
    /// 指纹冲突（重复导入）时返回 `false`，不会覆盖原有记录。
    pub fn record_event(&self, batch_id: i64, ev: &CalendarEvent, calendar_event_id: Option<i64>) -> Result<bool, String> {
        let n = self
            .conn
            .execute(
                "INSERT OR IGNORE INTO imported_event
                   (fingerprint, batch_id, calendar_event_id, title, start, end, location)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                params![
                    ev.fingerprint,
                    batch_id,
                    calendar_event_id,
                    ev.title,
                    ev.start,
                    ev.end,
                    ev.location
                ],
            )
            .map_err(|e| format!("写入导入记录失败: {e}"))?;
        Ok(n > 0)
    }

    /// 更新批次的日程计数。
    pub fn finish_batch(&self, batch_id: i64, event_count: i64) -> Result<(), String> {
        self.conn
            .execute(
                "UPDATE import_batch SET event_count = ?1 WHERE id = ?2",
                params![event_count, batch_id],
            )
            .map_err(|e| format!("更新批次失败: {e}"))?;
        Ok(())
    }

    /// 列出所有批次，最新的在前。
    pub fn list_batches(&self) -> Result<Vec<ImportBatch>, String> {
        let mut stmt = self
            .conn
            .prepare(
                "SELECT id, created_at, term_start, source, event_count
                 FROM import_batch ORDER BY id DESC",
            )
            .map_err(|e| format!("查询批次失败: {e}"))?;
        let rows = stmt
            .query_map([], |row| {
                Ok(ImportBatch {
                    id: row.get(0)?,
                    created_at: row.get(1)?,
                    term_start: row.get(2)?,
                    source: row.get(3)?,
                    event_count: row.get(4)?,
                })
            })
            .map_err(|e| format!("读取批次失败: {e}"))?;
        rows.collect::<Result<Vec<_>, _>>()
            .map_err(|e| format!("读取批次失败: {e}"))
    }

    /// 取出本 App 记录过的**全部**日程引用，供「一键清空」使用。
    pub fn all_event_refs(&self) -> Result<Vec<EventRef>, String> {
        let mut stmt = self
            .conn
            .prepare(
                "SELECT fingerprint, COALESCE(calendar_event_id, -1), title, start, end, location
                 FROM imported_event",
            )
            .map_err(|e| format!("查询导入记录失败: {e}"))?;
        let rows = stmt
            .query_map([], |row| {
                Ok(EventRef {
                    fingerprint: row.get(0)?,
                    calendar_event_id: row.get(1)?,
                    title: row.get(2)?,
                    start: row.get(3)?,
                    end: row.get(4)?,
                    location: row.get(5)?,
                })
            })
            .map_err(|e| format!("读取导入记录失败: {e}"))?;
        rows.collect::<Result<Vec<_>, _>>()
            .map_err(|e| format!("读取导入记录失败: {e}"))
    }

    /// 统计本 App 记录了多少条日程。
    pub fn event_count(&self) -> Result<i64, String> {
        self.conn
            .query_row("SELECT COUNT(*) FROM imported_event", [], |row| row.get(0))
            .map_err(|e| format!("统计失败: {e}"))
    }

    /// 清空**记录库**。
    ///
    /// 注意：这**不**删除系统日历里的事件。正确顺序是
    /// 1. 用 [`Store::all_event_refs`] 拿到要删的事件；
    /// 2. Kotlin 侧逐条删除系统日历事件；
    /// 3. 再调用本方法把记录清掉。
    ///
    /// 这样即使第 2 步中途失败，记录还在，用户再点一次「清空」仍能补齐。
    pub fn clear_records(&self) -> Result<(), String> {
        self.conn
            .execute_batch("DELETE FROM imported_event; DELETE FROM import_batch;")
            .map_err(|e| format!("清空记录失败: {e}"))
    }

    /// 回填某条记录对应的系统日历事件 `_ID`。
    ///
    /// 用于「先写系统日历、拿到 `_ID` 后再补记录」的场景。
    pub fn attach_calendar_id(&self, fingerprint: &str, calendar_id: i64) -> Result<bool, String> {
        let n = self
            .conn
            .execute(
                "UPDATE imported_event SET calendar_event_id = ?1 WHERE fingerprint = ?2",
                params![calendar_id, fingerprint],
            )
            .map_err(|e| format!("回填日历事件 id 失败: {e}"))?;
        Ok(n > 0)
    }

    /// 删除指定指纹的记录（用于「只重导某一门课」等更细的场景）。
    pub fn delete_fingerprints(&self, fingerprints: &[String]) -> Result<usize, String> {
        let mut n = 0;
        for fp in fingerprints {
            n += self
                .conn
                .execute("DELETE FROM imported_event WHERE fingerprint = ?1", params![fp])
                .map_err(|e| format!("删除记录失败: {e}"))?;
        }
        Ok(n)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ev(fp: &str, title: &str) -> CalendarEvent {
        CalendarEvent {
            fingerprint: fp.to_string(),
            title: title.to_string(),
            description: String::new(),
            location: "教1-201".into(),
            start: "2026-03-02T08:00:00".into(),
            end: "2026-03-02T09:35:00".into(),
            start_millis: 0,
            end_millis: 0,
            all_day: false,
            source_index: 0,
        }
    }

    #[test]
    fn records_and_lists_events() {
        let s = Store::open_in_memory().unwrap();
        let batch = s.begin_batch("2026-03-02", "jwgl 直连", 1_700_000_000).unwrap();
        assert!(s.record_event(batch, &ev("aaa", "高等数学"), Some(101)).unwrap());
        assert!(s.record_event(batch, &ev("bbb", "大学物理"), Some(102)).unwrap());
        s.finish_batch(batch, 2).unwrap();

        assert_eq!(s.event_count().unwrap(), 2);
        let refs = s.all_event_refs().unwrap();
        assert_eq!(refs.len(), 2);
        let a = refs.iter().find(|r| r.fingerprint == "aaa").unwrap();
        assert_eq!(a.calendar_event_id, 101);
        assert_eq!(a.title, "高等数学");

        let batches = s.list_batches().unwrap();
        assert_eq!(batches.len(), 1);
        assert_eq!(batches[0].event_count, 2);
        assert_eq!(batches[0].source, "jwgl 直连");
    }

    #[test]
    fn duplicate_fingerprint_is_ignored() {
        let s = Store::open_in_memory().unwrap();
        let b1 = s.begin_batch("2026-03-02", "", 1).unwrap();
        assert!(s.record_event(b1, &ev("same", "高等数学"), Some(1)).unwrap());
        // 第二次导入同一份课表：指纹重复，不应重复记录
        let b2 = s.begin_batch("2026-03-02", "", 2).unwrap();
        assert!(!s.record_event(b2, &ev("same", "高等数学"), Some(2)).unwrap());
        assert_eq!(s.event_count().unwrap(), 1);
        assert!(s.fingerprint_exists("same").unwrap());
        assert!(!s.fingerprint_exists("nope").unwrap());
    }

    #[test]
    fn clear_records_empties_everything() {
        let s = Store::open_in_memory().unwrap();
        let b = s.begin_batch("2026-03-02", "", 1).unwrap();
        s.record_event(b, &ev("x", "A"), Some(1)).unwrap();
        s.record_event(b, &ev("y", "B"), Some(2)).unwrap();
        assert_eq!(s.event_count().unwrap(), 2);
        s.clear_records().unwrap();
        assert_eq!(s.event_count().unwrap(), 0);
        assert!(s.list_batches().unwrap().is_empty());
        assert!(s.all_event_refs().unwrap().is_empty());
    }

    #[test]
    fn delete_selected_fingerprints() {
        let s = Store::open_in_memory().unwrap();
        let b = s.begin_batch("2026-03-02", "", 1).unwrap();
        s.record_event(b, &ev("x", "A"), Some(1)).unwrap();
        s.record_event(b, &ev("y", "B"), Some(2)).unwrap();
        let n = s.delete_fingerprints(&["x".into()]).unwrap();
        assert_eq!(n, 1);
        assert_eq!(s.event_count().unwrap(), 1);
    }

    #[test]
    fn records_survive_reopen() {
        let dir = std::env::temp_dir().join(format!("bupt_store_test_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("records.db");
        let p = path.to_string_lossy().to_string();
        {
            let s = Store::open(&p).unwrap();
            let b = s.begin_batch("2026-03-02", "", 1).unwrap();
            s.record_event(b, &ev("persist", "A"), Some(7)).unwrap();
        }
        {
            let s = Store::open(&p).unwrap();
            assert_eq!(s.event_count().unwrap(), 1);
            assert_eq!(s.all_event_refs().unwrap()[0].calendar_event_id, 7);
        }
        let _ = std::fs::remove_dir_all(&dir);
    }
}
