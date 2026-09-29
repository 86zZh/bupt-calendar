//! 针对 `tests/fixtures/` 下课表页面的解析校验。
//!
//! 这个测试是**数据驱动**的：把任何一份教务课表页面的 HTML 放进
//! `core/tests/fixtures/`（浏览器里「另存为」或复制源码），再跑
//! `cargo test`，它就会自动校验解析器能读懂那份页面。
//!
//! 这样做的好处是：不用装 App、不用连校园网，就能在桌面上确认
//! 解析逻辑对真实页面是否有效；页面结构一有变化也能第一时间发现。
//!
//! 注意：**真实页面样本不进版本库**（含教学班号等可关联到个人的信息）。
//! 版本库里只有两份手写样本，其中 `sample_timetable_quirks.html`
//! 用虚构数据复刻了真实页面的全部难缠结构。

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use bupt_core::models::CourseEntry;
use bupt_core::parser::parse_timetable;
use bupt_core::term::{bell_schedule, build_events, parse_date, SCHEDULE_MAINLAND};

/// 手写样例（简易版）：内容完全掌握，做**强断言**。
const MY_SAMPLE: &str = "sample_timetable.html";

/// 手写样例（坑位版）：用虚构数据复刻真实页面的难缠结构，做**强断言**。
const QUIRKS_SAMPLE: &str = "sample_timetable_quirks.html";

fn fixtures_dir() -> &'static Path {
    static DIR: OnceLock<PathBuf> = OnceLock::new();
    DIR.get_or_init(|| Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures"))
        .as_path()
}

#[test]
fn bundled_sample_parses_fully() {
    let path = fixtures_dir().join(MY_SAMPLE);
    let html = fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("读不了 {}: {e}", path.display()));

    let report = parse_timetable(&html);
    assert!(report.found_table, "应该找到 #kbtable");
    assert_eq!(report.strategy, "title", "该样例每格都有 title 属性");
    assert!(report.warnings.is_empty(), "不该有告警: {:?}", report.warnings);

    // 7 条：含「一格两门课」（大学物理 + 物理实验）
    assert_eq!(report.entries.len(), 7, "条目: {:#?}", report.entries);

    let by_name = |n: &str| {
        report
            .entries
            .iter()
            .find(|e| e.name == n)
            .unwrap_or_else(|| panic!("缺少课程 {n}"))
    };

    let math = by_name("高等数学A");
    assert_eq!(math.day, 1, "高等数学A 在周一");
    assert_eq!((math.start_node, math.end_node), (1, 2));
    assert_eq!(math.teacher, "张明");
    assert_eq!(math.room, "教1-201");
    assert_eq!(math.weeks[0].expand().len(), 16);

    // 同一格被 `-----` 拆开的两门课，星期与节次必须一致
    let physics = by_name("大学物理");
    let lab = by_name("物理实验");
    assert_eq!(physics.day, 3);
    assert_eq!(lab.day, 3);
    assert_eq!((physics.start_node, physics.end_node), (1, 2));
    assert_eq!((lab.start_node, lab.end_node), (1, 2));
    assert_eq!(physics.weeks[0].expand(), vec![1, 2, 3, 4, 5, 6, 7, 8]);
    assert_eq!(lab.weeks[0].expand(), vec![9, 10, 11, 12, 13, 14, 15, 16]);

    // 藏在 `kbcontent1`（兄弟节点是空的 `kbcontent`）里的课也要解析出来
    let policy = by_name("形势与政策");
    assert_eq!(policy.day, 1);
    assert_eq!((policy.start_node, policy.end_node), (6, 7));
    assert_eq!(policy.room, "教4-201");

    // 星期映射不能整体错位：周一 2 条、周二 1 条、周三 2 条、周四 1 条、周五 1 条
    let mut per_day = [0usize; 8];
    for e in &report.entries {
        per_day[e.day as usize] += 1;
    }
    assert_eq!(&per_day[1..=7], &[2, 1, 2, 1, 1, 0, 0]);
}

#[test]
fn bundled_sample_expands_to_expected_event_count() {
    let html = fs::read_to_string(fixtures_dir().join(MY_SAMPLE)).unwrap();
    let report = parse_timetable(&html);

    let term_start = parse_date("2026-03-02").unwrap();
    let schedule = bell_schedule(SCHEDULE_MAINLAND);
    let events = build_events(&report.entries, term_start, &schedule, None);

    // 高数 16 + 大物 8 + 物理实验 8 + 英语 16 + 线代 8 + 程序设计 16 + 形势与政策 8 = 80
    assert_eq!(events.len(), 80);

    // 第一节必须落在周一 08:00-09:35
    let first = &events[0];
    assert_eq!(first.start, "2026-03-02T08:00:00");
    assert_eq!(first.end, "2026-03-02T09:35:00");
    assert_eq!(first.title, "高等数学A");

    // 所有日程的结束时间都必须晚于开始时间，且时间落在作息表的合法值上
    let valid_starts = [
        "08:00:00", "08:50:00", "09:50:00", "10:40:00", "11:30:00", "13:00:00",
        "13:50:00", "14:45:00", "15:40:00", "16:35:00", "17:25:00", "18:30:00",
        "19:20:00", "20:10:00",
    ];
    for ev in &events {
        assert!(ev.start < ev.end, "日程时间倒挂: {:?}", ev);
        assert!(
            valid_starts.iter().any(|t| ev.start.ends_with(t)),
            "开始时间不在作息表里: {}",
            ev.start
        );
        assert!(ev.end_millis > ev.start_millis);
    }
}

/// 遍历 fixtures 目录里所有 HTML：每份都必须至少解析出一些课程，
/// 且每条课程的字段都要合理。放进真实页面后这个测试就是回归防线。
#[test]
fn all_fixtures_parse_something_valid() {
    let dir = fixtures_dir();
    let mut checked = 0;

    let entries = fs::read_dir(dir).unwrap_or_else(|e| panic!("读不了 {}: {e}", dir.display()));
    for item in entries {
        let path = item.unwrap().path();
        let is_html = path
            .extension()
            .map(|e| e.eq_ignore_ascii_case("html") || e.eq_ignore_ascii_case("htm"))
            .unwrap_or(false);
        if !is_html {
            continue;
        }
        let html = fs::read_to_string(&path).unwrap();
        let report = parse_timetable(&html);

        assert!(
            report.found_table,
            "{}: 没找到课表表格（如果不是课表页面，请把它移出 fixtures）",
            path.display()
        );
        assert!(
            !report.entries.is_empty(),
            "{}: 找到了表格但没解析出课程。告警: {:?}",
            path.display(),
            report.warnings
        );

        for e in &report.entries {
            assert!(!e.name.trim().is_empty(), "{}: 有空课程名", path.display());
            assert!(
                (1..=7).contains(&e.day),
                "{}: 课程 {} 的星期不合理: {}",
                path.display(),
                e.name,
                e.day
            );
            assert!(
                e.start_node >= 1 && e.end_node >= e.start_node && e.end_node <= 20,
                "{}: 课程 {} 的节次不合理: {}-{}",
                path.display(),
                e.name,
                e.start_node,
                e.end_node
            );
            assert!(
                !e.weeks.is_empty(),
                "{}: 课程 {} 没有解析出周次",
                path.display(),
                e.name
            );
            // 周次必须落在合理学期范围内
            for w in &e.weeks {
                assert!(
                    w.start >= 1 && w.end <= 30 && w.start <= w.end,
                    "{}: 课程 {} 的周次不合理: {}-{}",
                    path.display(),
                    e.name,
                    w.start,
                    w.end
                );
            }
        }
        checked += 1;
        println!(
            "{}: 解析出 {} 条课程（策略 {}）",
            path.file_name().unwrap().to_string_lossy(),
            report.entries.len(),
            report.strategy
        );
    }

    assert!(checked > 0, "fixtures 目录里没有 HTML 可校验");
}

/// 「坑位版」样本的强断言：真实页面里那些容易出错的结构，逐项锁死。
///
/// 这份样本用虚构数据复刻了真实页面的结构，所以**公开仓库里没有个人数据，
/// 也能完整回归这些坑**。
#[test]
fn quirks_sample_covers_all_tricky_structures() {
    let path = fixtures_dir().join(QUIRKS_SAMPLE);
    let html = fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("读不了 {}: {e}", path.display()));

    let report = parse_timetable(&html);
    assert!(report.found_table, "应找到 #kbtable");

    // ① 骨架模板 div 被过滤 + 隐藏/可见版被合并 + 备注行没被当成课程
    //    期望正好 5 条：数据结构×2（不同节次）、操作系统、计算机网络、编译原理
    assert_eq!(
        report.entries.len(),
        5,
        "条目数不对（可能骨架 div 没过滤、或配对 div 没合并）: {:#?}",
        report.entries
    );

    let find = |name: &str, day: i32, sn: i32| -> &CourseEntry {
        report
            .entries
            .iter()
            .find(|e| e.name == name && e.day == day && e.start_node == sn)
            .unwrap_or_else(|| panic!("缺少 {name} day={day} node={sn}"))
    };

    // ② 课程名取自 div 的直接文本（没有 title="课程名称"）
    let ds = find("数据结构", 1, 1);
    assert_eq!(ds.teacher, "王老师", "教师应取到");
    assert_eq!(ds.room, "教1-101", "教室应取到");

    // ③ 星期几来自课程 id 的 `<32hex>-<星期>-<类型>`（而不是靠数列）
    //    这一行和它的节次标签是 <th>，旧代码在这里会把整行错位一格
    assert_eq!(find("数据结构", 1, 1).day, 1, "数据结构应在周一");
    assert_eq!(find("操作系统", 2, 1).day, 2, "操作系统应在周二（不能被推到周一）");
    assert_eq!(find("编译原理", 3, 7).day, 3, "编译原理应在周三（不能被推到周二）");
    assert_eq!(find("计算机网络", 5, 1).day, 5, "计算机网络应在周五");

    // ④ 同一门课的不同节次必须保留成独立条目
    let ds2 = find("数据结构", 1, 3);
    assert_eq!((ds2.start_node, ds2.end_node), (3, 4));
    assert_eq!(ds.weeks, ds2.weeks, "两节的周次应相同");

    // ⑤ 三小节连排 `[01-02-03节]` 取首尾数字 → 1-3 节
    let os = find("操作系统", 2, 1);
    assert_eq!((os.start_node, os.end_node), (1, 3), "[01-02-03节] 应是 1~3 节");
    assert_eq!(os.weeks[0].start, 3);
    assert_eq!(os.weeks[0].end, 18);

    // ⑥ 多段周次 `3,5-18(周)` 拆成两个区间
    let net = find("计算机网络", 5, 1);
    let weeks = net.all_weeks();
    assert!(weeks.contains(&3), "应含第 3 周: {weeks:?}");
    assert!(weeks.contains(&5) && weeks.contains(&18), "应含 5~18 周");
    assert_eq!(weeks.len(), 1 + 14, "第 3 周 + 5..18 共 15 个周次");

    // ⑦ 备注行里的教学班号不能被当成课程
    for e in &report.entries {
        assert!(
            !e.name.contains("备注") && !e.name.contains("90000000"),
            "备注行被误当成课程了: {e:?}"
        );
    }

    // ⑧ 每条都必须有教师和地点（这份样本两个字段都全）
    for e in &report.entries {
        assert!(!e.teacher.trim().is_empty(), "{} 缺教师", e.name);
        assert!(!e.room.trim().is_empty(), "{} 缺地点", e.name);
    }

    // ⑨ 每天条目数：周一 2、周二 1、周三 1、周五 1
    let mut per_day = [0usize; 8];
    for e in &report.entries {
        per_day[e.day as usize] += 1;
    }
    assert_eq!(&per_day[1..=7], &[2, 1, 1, 0, 1, 0, 0], "每天条目数不对");
}

/// 坑位版样本展开成日程后，星期几必须和课表一致（防整体错位）。
#[test]
fn quirks_sample_events_match_weekdays() {
    let path = fixtures_dir().join(QUIRKS_SAMPLE);
    let Ok(html) = fs::read_to_string(&path) else {
        return;
    };
    let report = parse_timetable(&html);

    let term_start = parse_date("2026-09-14").unwrap(); // 周一
    let schedule = bell_schedule(SCHEDULE_MAINLAND);
    let events = build_events(&report.entries, term_start, &schedule, None);
    assert!(!events.is_empty());

    for ev in &events {
        let entry = report
            .entries
            .get(ev.source_index)
            .unwrap_or_else(|| panic!("source_index 越界: {}", ev.source_index));
        let date = parse_date(&ev.start[..10]).unwrap();
        use chrono::Datelike;
        let actual = date.weekday().num_days_from_monday() as i32 + 1;
        assert_eq!(
            actual, entry.day,
            "错位！{} 在课表里是周{}，算出来是 {}（周{}）",
            ev.title, entry.day, &ev.start[..10], actual
        );
    }

    // 三五小节连排的时间边界：操作系统是 1~3 节 → 08:00–10:35
    let os = events
        .iter()
        .find(|e| e.title == "操作系统")
        .expect("应生成操作系统的日程");
    assert!(os.start.ends_with("08:00:00"), "开始时间: {}", os.start);
    assert!(os.end.ends_with("10:35:00"), "结束时间: {}", os.end);
}
