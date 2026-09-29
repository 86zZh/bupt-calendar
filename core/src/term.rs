//! 作息时间表与「周次 → 具体日期」换算。
//!
//! 作息表来源：北京邮电大学本科教务处官方网站教学日历
//! <https://jwbs.bupt.edu.cn/info/1007/1264.htm>（正文为图片，已人工核对）
//!
//! 一天共 14 小节：
//! ```text
//! 上午  1 08:00-08:45   2 08:50-09:35   3 09:50-10:35   4 10:40-11:25   5 11:30-12:15
//! 下午  6 13:00-13:45   7 13:50-14:35   8 14:45-15:30   9 15:40-16:25  10 16:35-17:20  11 17:25-18:10
//! 晚上 12 18:30-19:15  13 19:20-20:05  14 20:10-20:55
//! ```

use crate::models::{BellSchedule, CalendarEvent, CourseEntry, Section};
// TimeZone 提供 `and_local_timezone`；在部分目标上编译器会认为它"未使用"，
// 因此显式允许该警告。
#[allow(unused_imports)]
use chrono::TimeZone;
use chrono::{Datelike, Duration, Local, NaiveDate, NaiveDateTime, NaiveTime, Weekday};

/// 北邮本部（西土城 / 沙河）作息表。
pub const SCHEDULE_MAINLAND: &str = "bupt_main";

/// 北邮海南校区作息表。
///
/// 官方教学日历脚注写明「海南校区教学节次按照海南试验区教学节次执行」，
/// 但目前未取得该套作息的确切时间，因此先与本部保持一致，
/// 待拿到海南校区课表时间表后再单独校准（见 `bell_schedule` 的 TODO）。
pub const SCHEDULE_HAINAN: &str = "bupt_hainan";

/// 小节时间原始表：(序号, 开始, 结束)
const BUPT_SECTIONS: [(i32, &str, &str); 14] = [
    (1, "08:00", "08:45"),
    (2, "08:50", "09:35"),
    (3, "09:50", "10:35"),
    (4, "10:40", "11:25"),
    (5, "11:30", "12:15"),
    (6, "13:00", "13:45"),
    (7, "13:50", "14:35"),
    (8, "14:45", "15:30"),
    (9, "15:40", "16:25"),
    (10, "16:35", "17:20"),
    (11, "17:25", "18:10"),
    (12, "18:30", "19:15"),
    (13, "19:20", "20:05"),
    (14, "20:10", "20:55"),
];

/// 取得指定作息表。
pub fn bell_schedule(id: &str) -> BellSchedule {
    let sections = BUPT_SECTIONS
        .iter()
        .map(|(index, start, end)| Section {
            index: *index,
            start: (*start).to_string(),
            end: (*end).to_string(),
        })
        .collect();

    match id {
        // TODO(海南): 拿到海南试验区教学节次后，在此返回独立的 sections。
        SCHEDULE_HAINAN => BellSchedule {
            id: SCHEDULE_HAINAN.to_string(),
            name: "北邮海南校区".to_string(),
            sections,
        },
        _ => BellSchedule {
            id: SCHEDULE_MAINLAND.to_string(),
            name: "北邮本部".to_string(),
            sections,
        },
    }
}

/// 把「第几周 + 星期几」换算成具体日期。
///
/// `week1_monday` 是**第 1 周的周一**。北邮官方教学日历存在「第 0 周」，
/// 该周就是 `week1_monday - 7 天`，因此这里对 `week = 0` 也必须正确工作。
///
/// 公式：`date = week1_monday + (week - 1) * 7 + (day - 1)`
pub fn date_of(week1_monday: NaiveDate, week: i32, day: i32) -> Option<NaiveDate> {
    if !(1..=7).contains(&day) {
        return None;
    }
    let offset_days = (week as i64 - 1) * 7 + (day as i64 - 1);
    week1_monday.checked_add_signed(Duration::days(offset_days))
}

/// 解析 `YYYY-MM-DD`。
pub fn parse_date(s: &str) -> Result<NaiveDate, String> {
    NaiveDate::parse_from_str(s.trim(), "%Y-%m-%d")
        .map_err(|e| format!("学期起始日期格式应为 YYYY-MM-DD，收到 {s:?}: {e}"))
}

/// 把日期调整到它所在周的周一。
pub fn monday_of(d: NaiveDate) -> NaiveDate {
    let delta = d.weekday().num_days_from_monday() as i64;
    d - Duration::days(delta)
}

fn parse_hhmm(s: &str) -> Option<NaiveTime> {
    NaiveTime::parse_from_str(s.trim(), "%H:%M").ok()
}

/// 某一小节对应的 (开始时间, 结束时间)。
pub fn node_time(schedule: &BellSchedule, node: i32) -> Option<(NaiveTime, NaiveTime)> {
    let sec = schedule.section(node)?;
    Some((parse_hhmm(&sec.start)?, parse_hhmm(&sec.end)?))
}

/// 星期几的中文名，用于日程描述。
pub fn weekday_cn(day: i32) -> &'static str {
    match day {
        1 => "周一",
        2 => "周二",
        3 => "周三",
        4 => "周四",
        5 => "周五",
        6 => "周六",
        7 => "周日",
        _ => "未知",
    }
}

/// 由 `chrono::Weekday` 转 1..7。
pub fn weekday_num(w: Weekday) -> i32 {
    w.num_days_from_monday() as i32 + 1
}

/// 把课表条目展开成一条条具体的日历日程。
///
/// * `entries` —— 解析出来的课表条目
/// * `week1_monday` —— 第 1 周的周一
/// * `schedule` —— 作息表
/// * `week_filter` —— 若为 `Some`，只生成这些周次的日程；`None` 表示全部
///
/// 结束时间取「该条目最后一小节的结束时间」。同一门课若跨越相邻小节
/// （强智里形如 `[01-02节]`），会自动合并为一段连续时间。
pub fn build_events(
    entries: &[CourseEntry],
    week1_monday: NaiveDate,
    schedule: &BellSchedule,
    week_filter: Option<&[i32]>,
) -> Vec<CalendarEvent> {
    let mut events = Vec::new();

    for (index, entry) in entries.iter().enumerate() {
        if !entry.is_valid() {
            continue;
        }
        let Some((start_time, _)) = node_time(schedule, entry.start_node) else {
            continue;
        };
        let Some((_, end_time)) = node_time(schedule, entry.end_node) else {
            continue;
        };

        let mut weeks = entry.all_weeks();
        if let Some(filter) = week_filter {
            weeks.retain(|w| filter.contains(w));
        }

        for week in weeks {
            let Some(date) = date_of(week1_monday, week, entry.day) else {
                continue;
            };
            let start = NaiveDateTime::new(date, start_time);
            let end = NaiveDateTime::new(date, end_time);
            // 同一小节开始与结束相同时（异常数据）跳过，避免生成零长度日程
            if end <= start {
                continue;
            }

            let location = compose_location(entry);
            let description = compose_description(entry, week);

            let fingerprint = fingerprint_of(
                &entry.name,
                date,
                start_time,
                &location,
                &entry.teacher,
            );

            // 按本机时区换算成 epoch 毫秒，Android 侧可直接使用
            let start_millis = start.and_local_timezone(Local).single()
                .map(|dt| dt.timestamp_millis())
                .unwrap_or(0);
            let end_millis = end.and_local_timezone(Local).single()
                .map(|dt| dt.timestamp_millis())
                .unwrap_or(0);

            events.push(CalendarEvent {
                fingerprint,
                title: entry.name.trim().to_string(),
                description,
                location,
                start: start.format("%Y-%m-%dT%H:%M:%S").to_string(),
                end: end.format("%Y-%m-%dT%H:%M:%S").to_string(),
                start_millis,
                end_millis,
                all_day: false,
                source_index: index,
            });
        }
    }

    // 稳定排序，保证同样输入产生同样顺序（便于测试与幂等导入）
    events.sort_by(|a, b| {
        a.start
            .cmp(&b.start)
            .then_with(|| a.title.cmp(&b.title))
            .then_with(|| a.fingerprint.cmp(&b.fingerprint))
    });
    events
}

fn compose_location(entry: &CourseEntry) -> String {
    let mut loc = entry.room.trim().to_string();
    let group = entry.group.trim();
    if !group.is_empty() {
        if !loc.is_empty() {
            loc.push(' ');
        }
        loc.push_str(group);
    }
    loc
}

fn compose_description(entry: &CourseEntry, week: i32) -> String {
    let mut parts = vec![format!("第{week}周 {}", weekday_cn(entry.day))];
    if !entry.teacher.trim().is_empty() {
        parts.push(format!("教师：{}", entry.teacher.trim()));
    }
    if !entry.room.trim().is_empty() {
        parts.push(format!("地点：{}", entry.room.trim()));
    }
    if !entry.group.trim().is_empty() {
        parts.push(format!("分组：{}", entry.group.trim()));
    }
    parts.push(format!("节次：{}-{}节", entry.start_node, entry.end_node));
    parts.join("\n")
}

/// 事件指纹：课程名 + 日期 + 开始时间 + 地点 + 教师。
///
/// 选择这些字段是因为它们共同唯一确定「哪一门课在哪一天哪一节上」，
/// 课表调整后大多数字段会变，从而产生新的指纹；而重复导入同一份课表
/// 会产生相同指纹，用于幂等去重。
///
/// 采用 FNV-1a 64 位（而非引入 sha2 依赖），碰撞概率对课表规模而言可忽略。
pub fn fingerprint_of(
    name: &str,
    date: NaiveDate,
    start: NaiveTime,
    location: &str,
    teacher: &str,
) -> String {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    let payload = format!(
        "{}\u{1f}{}\u{1f}{}\u{1f}{}\u{1f}{}",
        name.trim(),
        date.format("%Y-%m-%d"),
        start.format("%H:%M"),
        location.trim(),
        teacher.trim()
    );
    for b in payload.as_bytes() {
        hash ^= *b as u64;
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    // 再混入长度，降低相近字符串的碰撞可能
    hash ^= payload.len() as u64;
    format!("{hash:016x}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::{WeekRange, WeekType};

    fn d(s: &str) -> NaiveDate {
        NaiveDate::parse_from_str(s, "%Y-%m-%d").unwrap()
    }

    #[test]
    fn schedule_has_14_sections() {
        let s = bell_schedule(SCHEDULE_MAINLAND);
        assert_eq!(s.sections.len(), 14);
        assert_eq!(s.section(1).unwrap().start, "08:00");
        assert_eq!(s.section(1).unwrap().end, "08:45");
        assert_eq!(s.section(14).unwrap().start, "20:10");
        assert_eq!(s.section(14).unwrap().end, "20:55");
    }

    #[test]
    fn week0_is_supported() {
        // 官方教学日历：2025-2026 秋季 第1周周一 = 2025-09-08，第0周周一 = 2025-09-01
        let week1 = d("2025-09-08");
        assert_eq!(date_of(week1, 0, 1).unwrap(), d("2025-09-01"));
        assert_eq!(date_of(week1, 1, 1).unwrap(), d("2025-09-08"));
    }

    #[test]
    fn week_and_day_math() {
        let week1 = d("2026-03-02"); // 2025-2026 春季第 1 周周一
        assert_eq!(date_of(week1, 1, 1).unwrap(), d("2026-03-02"));
        assert_eq!(date_of(week1, 1, 7).unwrap(), d("2026-03-08"));
        assert_eq!(date_of(week1, 2, 1).unwrap(), d("2026-03-09"));
        assert_eq!(date_of(week1, 16, 5).unwrap(), d("2026-06-19"));
    }

    #[test]
    fn monday_of_works() {
        assert_eq!(monday_of(d("2026-03-05")), d("2026-03-02")); // 周四
        assert_eq!(monday_of(d("2026-03-02")), d("2026-03-02")); // 周一
        assert_eq!(monday_of(d("2026-03-08")), d("2026-03-02")); // 周日
    }

    fn entry(weeks: Vec<WeekRange>) -> CourseEntry {
        CourseEntry {
            name: "高等数学".into(),
            teacher: "张教授".into(),
            room: "教1-201".into(),
            group: String::new(),
            day: 1,
            start_node: 1,
            end_node: 2,
            nodes_explicit: true,
            weeks,
            raw: String::new(),
        }
    }

    #[test]
    fn expands_weekly_course_to_16_events() {
        let week1 = d("2026-03-02");
        let e = entry(vec![WeekRange::new(1, 16, WeekType::Every)]);
        let events = build_events(&[e], week1, &bell_schedule(SCHEDULE_MAINLAND), None);
        assert_eq!(events.len(), 16);
        assert_eq!(events[0].start, "2026-03-02T08:00:00");
        assert_eq!(events[0].end, "2026-03-02T09:35:00");
        assert_eq!(events[0].title, "高等数学");
        assert_eq!(events[0].location, "教1-201");
        assert!(events[0].description.contains("第1周 周一"));
        assert!(events[0].description.contains("教师：张教授"));
        assert_eq!(events[15].start, "2026-06-15T08:00:00");
    }

    /// epoch 毫秒必须与本地时间字符串一致，否则 Android 写进日历会偏时区。
    #[test]
    fn millis_match_local_wall_clock() {
        let week1 = d("2026-03-02");
        let e = entry(vec![WeekRange::new(1, 1, WeekType::Every)]);
        let events = build_events(&[e], week1, &bell_schedule(SCHEDULE_MAINLAND), None);
        let ev = &events[0];

        let start_local = NaiveDateTime::parse_from_str(&ev.start, "%Y-%m-%dT%H:%M:%S").unwrap();
        let expected = start_local.and_local_timezone(Local).single().unwrap().timestamp_millis();
        assert_eq!(ev.start_millis, expected);

        let end_local = NaiveDateTime::parse_from_str(&ev.end, "%Y-%m-%dT%H:%M:%S").unwrap();
        let expected_end = end_local.and_local_timezone(Local).single().unwrap().timestamp_millis();
        assert_eq!(ev.end_millis, expected_end);

        // 一节课 95 分钟
        assert_eq!(ev.end_millis - ev.start_millis, 95 * 60 * 1000);
    }

    #[test]
    fn odd_weeks_only() {
        let week1 = d("2026-03-02");
        let e = entry(vec![WeekRange::new(1, 8, WeekType::Odd)]);
        let events = build_events(&[e], week1, &bell_schedule(SCHEDULE_MAINLAND), None);
        assert_eq!(events.len(), 4);
        let titles: Vec<&str> = events.iter().map(|e| e.start.as_str()).collect();
        assert_eq!(
            titles,
            vec![
                "2026-03-02T08:00:00",
                "2026-03-16T08:00:00",
                "2026-03-30T08:00:00",
                "2026-04-13T08:00:00"
            ]
        );
    }

    #[test]
    fn multi_range_weeks_are_merged_and_deduped() {
        let week1 = d("2026-03-02");
        // 1-2 与 2-3 有重叠，应去重为 4 个周次
        let e = entry(vec![
            WeekRange::new(1, 2, WeekType::Every),
            WeekRange::new(2, 3, WeekType::Every),
        ]);
        assert_eq!(e.all_weeks(), vec![1, 2, 3]);
        let events = build_events(&[e], week1, &bell_schedule(SCHEDULE_MAINLAND), None);
        assert_eq!(events.len(), 3);
    }

    #[test]
    fn week_filter_limits_output() {
        let week1 = d("2026-03-02");
        let e = entry(vec![WeekRange::new(1, 16, WeekType::Every)]);
        let events = build_events(
            &[e],
            week1,
            &bell_schedule(SCHEDULE_MAINLAND),
            Some(&[1, 3, 5]),
        );
        assert_eq!(events.len(), 3);
        assert_eq!(events[0].start, "2026-03-02T08:00:00");
        assert_eq!(events[1].start, "2026-03-16T08:00:00");
        assert_eq!(events[2].start, "2026-03-30T08:00:00");
    }

    #[test]
    fn fingerprint_is_stable_and_distinct() {
        let t = NaiveTime::parse_from_str("08:00", "%H:%M").unwrap();
        let a = fingerprint_of("高等数学", d("2026-03-02"), t, "教1-201", "张教授");
        let b = fingerprint_of("高等数学", d("2026-03-02"), t, "教1-201", "张教授");
        let c = fingerprint_of("高等数学", d("2026-03-09"), t, "教1-201", "张教授");
        assert_eq!(a, b, "同样输入必须产生同样指纹");
        assert_ne!(a, c, "不同日期必须产生不同指纹");
        assert_eq!(a.len(), 16);
    }

    #[test]
    fn invalid_entries_are_skipped() {
        let week1 = d("2026-03-02");
        let mut e = entry(vec![WeekRange::new(1, 4, WeekType::Every)]);
        e.name = "   ".into();
        assert!(build_events(&[e], week1, &bell_schedule(SCHEDULE_MAINLAND), None).is_empty());
    }
}
