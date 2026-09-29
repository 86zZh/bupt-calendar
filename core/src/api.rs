//! 给 Kotlin 用的高层门面。
//!
//! 这一层**不依赖 JNI**，只处理 JSON 字符串，因此可以在桌面端直接跑单元测试
//! 甚至集成测试；JNI 薄壳（`jni_bridge.rs`）只负责把 Java 的 `String` 递进来。
//!
//! 典型调用顺序（对应 App 里的一次「导入课表」）：
//!
//! ```text
//! 1. parse_html(页面 HTML)                -> ParseReport（课程条目 + 诊断信息）
//! 2. plan_events(条目, 学期起始, 作息表)   -> PlanResult（要去重的日程 + 已存在条数）
//! 3. Kotlin 逐条写入系统日历，收集 _ID
//! 4. record_import(批次信息, 日程, _ID)    -> ImportOutcome（本次真正写入的条数）
//! ```

use chrono::{Local, NaiveDate};
use serde::{Deserialize, Serialize};

use crate::models::{BellSchedule, CalendarEvent, CourseEntry};
use crate::parser::ParseReport;
use crate::store::{ImportBatch, Store};
use crate::term::{bell_schedule, build_events, monday_of, parse_date, SCHEDULE_MAINLAND};

/// `parse_html` 的返回：解析报告 + 建议的作息表。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ParseHtmlResult {
    pub report: ParseReport,
    /// 解析出的课程条目
    pub entries: Vec<CourseEntry>,
    /// 课程摘要（按课程名聚合）
    pub summary: Vec<CourseSummaryLite>,
}

/// 单门课的聚合摘要，用于导入前给用户看「将导入 N 门课」。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CourseSummaryLite {
    pub name: String,
    pub teacher: String,
    pub room: String,
    /// 课表条目数
    pub entries: usize,
    /// 展开后的日程总数（需要学期起始日期才能算，缺失时为 0）
    pub events: usize,
}

/// `plan_events` 的返回。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlanResult {
    /// 需要写入系统日历的日程（已剔除记录库里已存在的）
    pub events: Vec<CalendarEvent>,
    /// 因为指纹重复而无需重复写入的条数
    pub already_imported: usize,
    /// 展开出的日程总数（含已存在的）
    pub total_events: usize,
    /// 摘要
    pub summary: Vec<CourseSummaryLite>,
}

/// `record_import` 的入参。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RecordImportRequest {
    pub term_start: String,
    pub source: String,
    /// 与 `events` 一一对应的系统日历 `_ID`；缺省或 -1 表示该条没有回填到
    pub calendar_event_ids: Vec<i64>,
    pub events: Vec<CalendarEvent>,
    /// 是否在写入前清空历史记录（重新导入课表时用）
    #[serde(default)]
    pub replace_existing: bool,
}

/// 解析课表 HTML。
pub fn parse_html(html: &str) -> String {
    parse_html_documents_json(&to_json(&vec![html.to_string()]))
}

/// 解析「主页面 + 各个内嵌框架」的**多个** HTML 文档。
///
/// `docs_json` 是一个 JSON 字符串数组，例如 `["<html>...主页面...", "<html>...iframe..."]`。
///
/// 之所以要支持多个文档：北邮教务的课表经常显示在 iframe / frame 里，
/// 主文档里没有课表。只解析主文档就会误报「找不到课表」。
///
/// 传入非法 JSON 时返回空数组处理（当作没解析到），不会让 App 崩掉。
pub fn parse_html_documents_json(docs_json: &str) -> String {
    let docs: Vec<String> = match serde_json::from_str(docs_json) {
        Ok(v) => v,
        Err(_) => Vec::new(),
    };
    let report = crate::parser::parse_documents(&docs);
    let entries = report.entries.clone();
    let summary = summarize(&entries, None, None);
    let out = ParseHtmlResult {
        report,
        entries,
        summary,
    };
    to_json(&out)
}

/// 返回内置作息表（`id` 为空时返回北邮本部作息表）。
pub fn get_schedule(id: &str) -> String {
    let id = if id.trim().is_empty() {
        SCHEDULE_MAINLAND
    } else {
        id.trim()
    };
    to_json(&bell_schedule(id))
}

/// 根据年份和季节推断学期第一周周一，供 UI 作为默认值。
///
/// 依据北邮官方教学日历的规律（春季学期第 1 周周一通常落在 3 月第 1 个周一
/// 前后，秋季学期落在 9 月第 2 个周一前后）。这里给出的是**建议值**，
/// 界面上必须允许用户手动改正 —— 课表周次与日期的对应关系错一周，
/// 整个导入结果就全错，所以宁可让用户确认。
pub fn suggest_term_start(today: &str) -> String {
    let today = parse_date(today).unwrap_or_else(|_| Local::now().date_naive());
    let year = today.format("%Y").to_string().parse::<i32>().unwrap_or(2026);
    let month = today.format("%m").to_string().parse::<u32>().unwrap_or(3);

    // 2 月到 7 月之间视为春季学期，其余视为秋季学期
    let (y, m, d) = if (2..=7).contains(&month) {
        (year, 3u32, 2u32)
    } else if month >= 8 {
        (year, 9u32, 8u32)
    } else {
        (year - 1, 9, 8)
    };
    let naive = NaiveDate::from_ymd_opt(y, m, d).unwrap_or(today);
    monday_of(naive).format("%Y-%m-%d").to_string()
}

/// 生成导入计划：展开日程并剔除已导入过的。
///
/// * `entries_json` —— `parse_html` 返回的 `entries` 数组
/// * `term_start` —— 第 1 周周一的日期 `YYYY-MM-DD`
/// * `schedule_id` —— 作息表 id，空则用北邮本部
/// * `week_filter_json` —— 可选，`[1,3,5]` 形式只导入这些周次；`null`/空表示全部
/// * `store_path` —— 记录库路径；为空字符串表示不做去重（只用于预览）
pub fn plan_events(
    entries_json: &str,
    term_start: &str,
    schedule_id: &str,
    week_filter_json: &str,
    store_path: &str,
) -> String {
    let result = (|| -> Result<PlanResult, String> {
        let entries: Vec<CourseEntry> =
            serde_json::from_str(entries_json).map_err(|e| format!("课程数据格式不对: {e}"))?;
        let start = parse_date(term_start)?;
        let schedule: BellSchedule = bell_schedule(if schedule_id.trim().is_empty() {
            SCHEDULE_MAINLAND
        } else {
            schedule_id.trim()
        });

        let filter: Option<Vec<i32>> = {
            let t = week_filter_json.trim();
            if t.is_empty() || t == "null" {
                None
            } else {
                Some(
                    serde_json::from_str::<Vec<i32>>(t)
                        .map_err(|e| format!("周次筛选格式不对: {e}"))?,
                )
            }
        };

        let all = build_events(&entries, start, &schedule, filter.as_deref());
        let total_events = all.len();

        // 去重：已记录过的指纹不再重复写入系统日历
        let mut events = Vec::new();
        let mut already = 0usize;
        if store_path.trim().is_empty() {
            events = all;
        } else {
            let store = Store::open(store_path)?;
            for ev in all {
                if store.fingerprint_exists(&ev.fingerprint)? {
                    already += 1;
                } else {
                    events.push(ev);
                }
            }
        }

        let filtered_weeks = filter.as_deref();
        let summary = summarize(&entries, Some(&schedule), Some((start, filtered_weeks)));
        Ok(PlanResult {
            events,
            already_imported: already,
            total_events,
            summary,
        })
    })();

    match result {
        Ok(r) => to_json(&r),
        Err(e) => to_json(&ErrorPayload { error: e }),
    }
}

/// 展开日程但不做去重，返回条目统计（用于导入前预览）。
pub fn preview_events(entries_json: &str, term_start: &str, schedule_id: &str) -> String {
    let result = (|| -> Result<PlanResult, String> {
        let entries: Vec<CourseEntry> =
            serde_json::from_str(entries_json).map_err(|e| format!("课程数据格式不对: {e}"))?;
        let start = parse_date(term_start)?;
        let schedule = bell_schedule(if schedule_id.trim().is_empty() {
            SCHEDULE_MAINLAND
        } else {
            schedule_id.trim()
        });
        let events = build_events(&entries, start, &schedule, None);
        let summary = summarize(&entries, Some(&schedule), Some((start, None)));
        Ok(PlanResult {
            total_events: events.len(),
            events,
            already_imported: 0,
            summary,
        })
    })();
    match result {
        Ok(r) => to_json(&r),
        Err(e) => to_json(&ErrorPayload { error: e }),
    }
}

/// 把「已经成功写入系统日历」的结果记录到本地库。
pub fn record_import(store_path: &str, request_json: &str) -> String {
    let result = (|| -> Result<crate::store::ImportOutcome, String> {
        let req: RecordImportRequest =
            serde_json::from_str(request_json).map_err(|e| format!("导入请求格式不对: {e}"))?;
        let store = Store::open(store_path)?;
        if req.replace_existing {
            // 重新导入课表：先清掉旧记录（系统日历的删除由 Kotlin 侧完成）
            store.clear_records()?;
        }
        let now = Local::now().timestamp();
        let batch_id = store.begin_batch(&req.term_start, &req.source, now)?;

        let mut inserted = 0usize;
        let mut skipped = 0usize;
        for (idx, ev) in req.events.iter().enumerate() {
            let cal_id = req
                .calendar_event_ids
                .get(idx)
                .copied()
                .filter(|v| *v > 0);
            if store.record_event(batch_id, ev, cal_id)? {
                inserted += 1;
            } else {
                skipped += 1;
            }
        }
        store.finish_batch(batch_id, inserted as i64)?;
        Ok(crate::store::ImportOutcome {
            batch_id,
            inserted,
            skipped,
        })
    })();
    match result {
        Ok(r) => to_json(&r),
        Err(e) => to_json(&ErrorPayload { error: e }),
    }
}

/// 取出本 App 记录过的全部日程，供「一键清空」逐条删除系统日历事件。
pub fn list_imported_events(store_path: &str) -> String {
    match Store::open(store_path).and_then(|s| s.all_event_refs()) {
        Ok(refs) => to_json(&refs),
        Err(e) => to_json(&ErrorPayload { error: e }),
    }
}

/// 列出历史导入批次。
pub fn list_import_batches(store_path: &str) -> String {
    let batches: Result<Vec<ImportBatch>, String> =
        Store::open(store_path).and_then(|s| s.list_batches());
    match batches {
        Ok(b) => to_json(&b),
        Err(e) => to_json(&ErrorPayload { error: e }),
    }
}

/// 记录库里有多少条日程。
pub fn imported_event_count(store_path: &str) -> i64 {
    Store::open(store_path)
        .and_then(|s| s.event_count())
        .unwrap_or(0)
}

/// 清空本地记录（**必须在系统日历事件删除成功之后调用**）。
pub fn clear_records(store_path: &str) -> bool {
    Store::open(store_path)
        .and_then(|s| s.clear_records())
        .is_ok()
}

/// 只删除指定指纹的记录（用于按课程精细清理）。
pub fn delete_records(store_path: &str, fingerprints_json: &str) -> i64 {
    let fps: Vec<String> = match serde_json::from_str(fingerprints_json) {
        Ok(v) => v,
        Err(_) => return -1,
    };
    Store::open(store_path)
        .and_then(|s| s.delete_fingerprints(&fps))
        .map(|n| n as i64)
        .unwrap_or(-1)
}

/// 记录某个已写入系统日历事件的 id（用于先写日历、后回填的场景）。
pub fn attach_calendar_id(store_path: &str, fingerprint: &str, calendar_id: i64) -> bool {
    let Ok(store) = Store::open(store_path) else {
        return false;
    };
    store
        .attach_calendar_id(fingerprint, calendar_id)
        .unwrap_or(false)
}

#[derive(Debug, Serialize, Deserialize)]
struct ErrorPayload {
    error: String,
}

fn to_json<T: Serialize>(v: &T) -> String {
    serde_json::to_string(v).unwrap_or_else(|e| format!("{{\"error\":\"JSON 序列化失败: {e}\"}}"))
}

/// 按课程名聚合出摘要。
fn summarize(
    entries: &[CourseEntry],
    schedule: Option<&BellSchedule>,
    term: Option<(NaiveDate, Option<&[i32]>)>,
) -> Vec<CourseSummaryLite> {
    use std::collections::BTreeMap;
    let mut map: BTreeMap<String, CourseSummaryLite> = BTreeMap::new();

    for e in entries {
        let key = e.name.trim().to_string();
        let slot = map.entry(key.clone()).or_insert_with(|| CourseSummaryLite {
            name: key.clone(),
            teacher: e.teacher.trim().to_string(),
            room: e.room.trim().to_string(),
            entries: 0,
            events: 0,
        });
        slot.entries += 1;
        if slot.teacher.is_empty() {
            slot.teacher = e.teacher.trim().to_string();
        }
        if slot.room.is_empty() {
            slot.room = e.room.trim().to_string();
        }
    }

    if let (Some(sched), Some((start, filter))) = (schedule, term) {
        for e in entries {
            let evs = build_events(std::slice::from_ref(e), start, sched, filter);
            if let Some(slot) = map.get_mut(e.name.trim()) {
                slot.events += evs.len();
            }
        }
    }

    map.into_values().collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::EventRef;

    const SAMPLE: &str = r#"
<table id="kbtable">
  <tr><th></th><th>星期一</th><th>星期二</th><th>星期三</th><th>星期四</th><th>星期五</th><th>星期六</th><th>星期日</th></tr>
  <tr><td>1</td>
    <td><div class="kbcontent" id="kbcontent_1-1">
      <font title="课程名称">高等数学</font><br>
      <font title="老师">张教授</font><br>
      <font title="周次(节次)">1-4(周)[01-02节]</font><br>
      <font title="教室">教1-201</font></div></td>
    <td>&nbsp;</td><td>&nbsp;</td>
    <td><div class="kbcontent" id="kbcontent_4-1">
      <font title="课程名称">大学物理</font><br>
      <font title="老师">李教授</font><br>
      <font title="周次(节次)">1-2(周)[01-02节]</font><br>
      <font title="教室">教2-305</font></div></td>
    <td>&nbsp;</td><td>&nbsp;</td><td>&nbsp;</td>
  </tr>
</table>"#;

    #[test]
    fn parse_html_reports_entries_and_summary() {
        let json = parse_html(SAMPLE);
        let v: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(v["entries"].as_array().unwrap().len(), 2);
        let summary = v["summary"].as_array().unwrap();
        assert_eq!(summary.len(), 2);
        assert!(v["report"]["found_table"].as_bool().unwrap());
    }

    #[test]
    fn plan_events_expands_and_reports() {
        let entries = {
            let v: serde_json::Value = serde_json::from_str(&parse_html(SAMPLE)).unwrap();
            v["entries"].to_string()
        };
        let json = plan_events(&entries, "2026-03-02", "", "", "");
        let v: serde_json::Value = serde_json::from_str(&json).unwrap();
        // 高等数学 4 周 + 大学物理 2 周
        assert_eq!(v["total_events"].as_u64().unwrap(), 6);
        assert_eq!(v["events"].as_array().unwrap().len(), 6);
        assert_eq!(v["already_imported"].as_u64().unwrap(), 0);
        assert!(v["error"].is_null());

        let first = &v["events"][0];
        assert_eq!(first["start"], "2026-03-02T08:00:00");
        assert_eq!(first["end"], "2026-03-02T09:35:00");
    }

    #[test]
    fn plan_events_respects_week_filter() {
        let entries = {
            let v: serde_json::Value = serde_json::from_str(&parse_html(SAMPLE)).unwrap();
            v["entries"].to_string()
        };
        let json = plan_events(&entries, "2026-03-02", "", "[1,2]", "");
        let v: serde_json::Value = serde_json::from_str(&json).unwrap();
        // 高数 2 周 + 物理 2 周
        assert_eq!(v["total_events"].as_u64().unwrap(), 4);
    }

    #[test]
    fn plan_events_reports_bad_date() {
        let json = plan_events("[]", "不是日期", "", "", "");
        let v: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert!(v["error"].as_str().unwrap().contains("YYYY-MM-DD"));
    }

    #[test]
    fn schedule_json_has_14_sections() {
        let v: serde_json::Value = serde_json::from_str(&get_schedule("")).unwrap();
        assert_eq!(v["sections"].as_array().unwrap().len(), 14);
        assert_eq!(v["sections"][0]["start"], "08:00");
    }

    #[test]
    fn suggest_term_start_gives_monday() {
        let s = suggest_term_start("2026-03-15");
        assert_eq!(s, "2026-03-02"); // 春季建议值
        let s = suggest_term_start("2025-10-01");
        assert_eq!(s, "2025-09-08"); // 秋季建议值
    }

    #[test]
    fn dedup_works_against_store() {
        let dir = std::env::temp_dir().join(format!("bupt_api_test_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let db = dir.join("r.db").to_string_lossy().to_string();
        let entries = {
            let v: serde_json::Value = serde_json::from_str(&parse_html(SAMPLE)).unwrap();
            v["entries"].to_string()
        };

        // 第一次：全部需要写入
        let plan: serde_json::Value =
            serde_json::from_str(&plan_events(&entries, "2026-03-02", "", "", &db)).unwrap();
        let events: Vec<CalendarEvent> = serde_json::from_value(plan["events"].clone()).unwrap();
        let ids: Vec<i64> = (1..=events.len() as i64).collect();
        let req = RecordImportRequest {
            term_start: "2026-03-02".into(),
            source: "test".into(),
            calendar_event_ids: ids,
            events: events.clone(),
            replace_existing: false,
        };
        let outcome: crate::store::ImportOutcome =
            serde_json::from_str(&record_import(&db, &serde_json::to_string(&req).unwrap())).unwrap();
        assert_eq!(outcome.inserted, 6);

        // 第二次：全部应判定为已导入
        let plan2: serde_json::Value =
            serde_json::from_str(&plan_events(&entries, "2026-03-02", "", "", &db)).unwrap();
        assert_eq!(plan2["already_imported"].as_u64().unwrap(), 6);
        assert_eq!(plan2["events"].as_array().unwrap().len(), 0);

        // 清除记录后又能重新导入
        assert!(clear_records(&db));
        let plan3: serde_json::Value =
            serde_json::from_str(&plan_events(&entries, "2026-03-02", "", "", &db)).unwrap();
        assert_eq!(plan3["events"].as_array().unwrap().len(), 6);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn list_imported_events_returns_ids_for_deletion() {
        let dir = std::env::temp_dir().join(format!("bupt_api_list_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let db = dir.join("r.db").to_string_lossy().to_string();
        let entries = {
            let v: serde_json::Value = serde_json::from_str(&parse_html(SAMPLE)).unwrap();
            v["entries"].to_string()
        };
        let plan: serde_json::Value =
            serde_json::from_str(&plan_events(&entries, "2026-03-02", "", "", &db)).unwrap();
        let events: Vec<CalendarEvent> = serde_json::from_value(plan["events"].clone()).unwrap();
        let ids: Vec<i64> = (100..100 + events.len() as i64).collect();
        let req = RecordImportRequest {
            term_start: "2026-03-02".into(),
            source: "test".into(),
            calendar_event_ids: ids.clone(),
            events,
            replace_existing: false,
        };
        record_import(&db, &serde_json::to_string(&req).unwrap());

        let refs: Vec<EventRef> = serde_json::from_str(&list_imported_events(&db)).unwrap();
        assert_eq!(refs.len(), 6);
        let mut got: Vec<i64> = refs.iter().map(|r| r.calendar_event_id).collect();
        got.sort_unstable();
        assert_eq!(got, ids);
        assert_eq!(imported_event_count(&db), 6);

        let _ = std::fs::remove_dir_all(&dir);
    }
}
