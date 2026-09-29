//! 端到端流程测试：完整模拟 App 里点一次「导入课表」到「清空」的全过程。
//!
//! 前面那些测试都是分开验证零件的（解析器 / 换算 / 记录库各测各的）。
//! 这个文件不一样：它**按 App 里真实的调用顺序**把整条链路串起来跑一遍，
//! 并且用的是**真实的北邮课表页面**。
//!
//! 一次导入在 App 里的实际调用顺序是：
//!
//! ```text
//! 1. BuptCore.parseHtml(页面 HTML)          → 解析出课程条目
//! 2. BuptCore.planEvents(条目, 第1周周一)    → 展开成日历日程 + 剔除已导入过的
//! 3. 系统日历逐条写入，收集返回的 _ID         （这里用假 id 代替）
//! 4. BuptCore.recordImport(日程, _ID)       → 登记到本地记录库
//! ```
//!
//! 清空时：
//!
//! ```text
//! 5. BuptCore.listImportedEvents()          → 取出要删的日程
//! 6. 系统日历逐条删除                        （App 侧做）
//! 7. BuptCore.clearRecords()                → 删干净后才清记录
//! ```
//!
//! 这个测试的价值在于：能抓住「零件都对、但串起来顺序错了」这类 bug。

use std::path::PathBuf;

use bupt_core::api::{
    clear_records, imported_event_count, list_imported_events, parse_html, plan_events,
    record_import, RecordImportRequest,
};
use bupt_core::models::CalendarEvent;
use bupt_core::store::ImportOutcome;

/// 真实课表页的第 1 周周一（2026-2027 学年第一学期）
const TERM_START: &str = "2026-09-14";

fn real_page_html() -> Option<String> {
    let path: PathBuf = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/real_xskb_list.html");
    std::fs::read_to_string(path).ok()
}

/// 临时记录库，测试结束自动删除。
struct TempStore {
    dir: PathBuf,
    path: String,
}

impl TempStore {
    fn new(tag: &str) -> Self {
        let dir = std::env::temp_dir().join(format!(
            "bupt_app_flow_{tag}_{}_{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ));
        std::fs::create_dir_all(&dir).expect("建测试目录");
        let path = dir.join("imports.db").to_string_lossy().to_string();
        TempStore { dir, path }
    }
}

impl Drop for TempStore {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

/// 从 `parse_html` 的返回里取出课表条目（模拟 Kotlin 用 gson 取 entries 字段）。
fn entries_of(parsed: &str) -> String {
    let v: serde_json::Value = serde_json::from_str(parsed).expect("parse_html 必须返回合法 JSON");
    assert!(v.get("error").is_none() || v["error"].is_null(), "解析出错了: {}", v["error"]);
    v["entries"].to_string()
}

/// 模拟 App 的完整导入流程，返回（写入的日程数，声称跳过的条数）。
fn import_once(store_path: &str, html: &str) -> (usize, usize) {
    let entries = entries_of(&parse_html(html));
    let plan_json = plan_events(&entries, TERM_START, "", "", store_path);
    let v: serde_json::Value = serde_json::from_str(&plan_json).expect("plan_events 返回合法 JSON");
    assert!(
        v.get("error").is_none() || v["error"].is_null(),
        "生成计划失败: {}",
        v["error"]
    );

    let events: Vec<CalendarEvent> =
        serde_json::from_value(v["events"].clone()).expect("events 字段应能反序列化");
    // App 侧会真的往系统日历里写，这里用递增的假 id 代替
    let ids: Vec<i64> = (1..=events.len() as i64).map(|i| 1000 + i).collect();

    let req = RecordImportRequest {
        term_start: TERM_START.to_string(),
        source: "集成测试".to_string(),
        calendar_event_ids: ids,
        events: events.clone(),
        replace_existing: false,
    };
    let outcome: ImportOutcome =
        serde_json::from_str(&record_import(store_path, &serde_json::to_string(&req).unwrap()))
            .expect("record_import 返回合法 JSON");
    assert!(outcome.batch_id > 0, "应返回有效的批次 id");

    (events.len(), outcome.skipped)
}

/// 完整走一遍：导入 → 再导入（应全部跳过）→ 清空 → 再导入（应恢复）。
#[test]
fn full_import_dedup_clear_cycle() {
    let Some(html) = real_page_html() else {
        eprintln!("跳过：真实课表页面不存在");
        return;
    };
    let store = TempStore::new("cycle");

    // ---- 第一次导入：应该全部写入 ----
    let entries = entries_of(&parse_html(&html));
    let parse_report: serde_json::Value = serde_json::from_str(&parse_html(&html)).unwrap();
    assert_eq!(
        parse_report["entries"].as_array().unwrap().len(),
        16,
        "真实页面应解析出 16 门课"
    );

    let (planned, skipped) = import_once(&store.path, &html);
    assert!(planned > 0, "第一次导入应产生日程");
    assert_eq!(skipped, 0, "第一次导入不应跳过任何条目");
    assert_eq!(
        imported_event_count(&store.path) as usize,
        planned,
        "记录库里应有全部日程"
    );

    // ---- 第二次导入同一份课表：应该全部跳过（幂等） ----
    let plan_json = plan_events(&entries, TERM_START, "", "", &store.path);
    let v: serde_json::Value = serde_json::from_str(&plan_json).unwrap();
    assert_eq!(
        v["already_imported"].as_u64().unwrap() as usize,
        planned,
        "第二次导入应识别出全部已导入"
    );
    assert_eq!(
        v["events"].as_array().unwrap().len(),
        0,
        "第二次导入不应产生新日程"
    );
    assert_eq!(
        imported_event_count(&store.path) as usize,
        planned,
        "重复导入不应增加记录"
    );

    // ---- 清空：App 先删系统日历事件，成功了才清记录 ----
    let refs = list_imported_events(&store.path);
    let refs: Vec<serde_json::Value> = serde_json::from_str(&refs).unwrap();
    assert_eq!(refs.len(), planned, "应能取回全部待删日程");
    // 每条都必须带着系统日历的事件 id（否则清空时只能靠标题+时间兜底）
    for r in &refs {
        assert!(
            r["calendar_event_id"].as_i64().unwrap_or(-1) > 0,
            "有记录没带上日历事件 id: {r}"
        );
        assert!(!r["title"].as_str().unwrap_or("").is_empty());
        assert!(!r["start"].as_str().unwrap_or("").is_empty());
    }

    assert!(clear_records(&store.path), "清空记录应成功");
    assert_eq!(imported_event_count(&store.path), 0, "清空后记录应为 0");

    // ---- 清空后重新导入：应该又能全部写入（这就是「课表变了重新导入」） ----
    let (planned2, skipped2) = import_once(&store.path, &html);
    assert_eq!(planned2, planned, "重新导入的日程数应与首次一致");
    assert_eq!(skipped2, 0, "清空后不应有跳过");
}

/// `replace_existing = true` 时必须先清掉旧记录，否则重新导入会全被跳过。
#[test]
fn replace_existing_resets_records() {
    let Some(html) = real_page_html() else {
        return;
    };
    let store = TempStore::new("replace");

    let (planned, _) = import_once(&store.path, &html);
    assert_eq!(imported_event_count(&store.path) as usize, planned);

    // 用户改了课表，想重来：App 会用 replace_existing 再登记一次
    let entries = entries_of(&parse_html(&html));
    let plan_json = plan_events(&entries, TERM_START, "", "", "");
    let v: serde_json::Value = serde_json::from_str(&plan_json).unwrap();
    let events: Vec<CalendarEvent> = serde_json::from_value(v["events"].clone()).unwrap();
    let ids: Vec<i64> = (1..=events.len() as i64).map(|i| 7000 + i).collect();

    let req = RecordImportRequest {
        term_start: TERM_START.to_string(),
        source: "重新导入".to_string(),
        calendar_event_ids: ids,
        events: events.clone(),
        replace_existing: true,
    };
    let outcome: ImportOutcome =
        serde_json::from_str(&record_import(&store.path, &serde_json::to_string(&req).unwrap()))
            .unwrap();

    assert_eq!(outcome.skipped, 0, "replace_existing 时不应有跳过");
    assert_eq!(outcome.inserted, events.len());
    assert_eq!(
        imported_event_count(&store.path) as usize,
        events.len(),
        "记录数应正好是本次导入的条数（旧的已清掉）"
    );
}

/// 星期与时间必须落在合理的范围内——防止「星期整体错位」这类静默错误。
#[test]
fn planned_events_land_on_correct_weekdays_and_hours() {
    let Some(html) = real_page_html() else {
        return;
    };
    let entries = entries_of(&parse_html(&html));
    let plan_json = plan_events(&entries, TERM_START, "", "", "");
    let v: serde_json::Value = serde_json::from_str(&plan_json).unwrap();
    let events: Vec<CalendarEvent> = serde_json::from_value(v["events"].clone()).unwrap();
    assert!(!events.is_empty());

    // 这份课表周一~周四有课，周五六日没有
    for ev in &events {
        let date = &ev.start[..10];
        let d = bupt_core::term::parse_date(date).expect("日期应可解析");
        let weekday = chrono::Datelike::weekday(&d).num_days_from_monday();
        assert!(
            weekday <= 4,
            "出现了周{}的日程（该课表只有周一到周五有课）: {date} {}",
            weekday + 1,
            ev.title
        );

        // 时间必须落在北邮官方作息表的 14 个小节里
        let hhmm = &ev.start[11..16];
        let valid = [
            "08:00", "08:50", "09:50", "10:40", "11:30", "13:00", "13:50",
            "14:45", "15:40", "16:35", "17:25", "18:30", "19:20", "20:10",
        ];
        assert!(valid.contains(&hhmm), "开始时间不在作息表里: {hhmm} ({})", ev.title);

        // 结束时间必须晚于开始时间，且长度合理（不超过 4 小时）
        let dur_min = (ev.end_millis - ev.start_millis) / 60_000;
        assert!(dur_min > 0 && dur_min <= 240, "时长异常: {dur_min} 分钟 ({})", ev.title);
    }

    // 每个日程的指纹必须唯一（否则去重会误杀）
    let mut fps: Vec<&str> = events.iter().map(|e| e.fingerprint.as_str()).collect();
    let total = fps.len();
    fps.sort_unstable();
    fps.dedup();
    assert_eq!(fps.len(), total, "同一批日程里出现了重复指纹");
}

/// 课表页没解析出课程时，App 应该能拿到可读的告警，而不是崩溃或静默失败。
#[test]
fn empty_or_wrong_page_gives_readable_warning() {
    let store = TempStore::new("empty");

    // 登录页（没有课表）
    let login_page = "<html><body><h1>统一身份认证</h1><form>...</form></body></html>";
    let parsed: serde_json::Value = serde_json::from_str(&parse_html(login_page)).unwrap();
    assert_eq!(parsed["report"]["found_table"].as_bool().unwrap(), false);
    assert!(parsed["entries"].as_array().unwrap().is_empty());
    let warnings = parsed["report"]["warnings"].as_array().unwrap();
    assert!(!warnings.is_empty(), "应给出「没找到课表」的告警");
    assert!(
        warnings[0].as_str().unwrap().contains("课表"),
        "告警应是人话: {}",
        warnings[0]
    );

    // 解析结果为空时，生成计划也不应报错，只是没有日程
    let plan: serde_json::Value =
        serde_json::from_str(&plan_events("[]", TERM_START, "", "", &store.path)).unwrap();
    assert!(plan["events"].as_array().unwrap().is_empty());
    assert_eq!(plan["total_events"].as_u64().unwrap(), 0);

    // 学期日期写错时要给出明确错误，而不是算出乱七八糟的日期
    let bad: serde_json::Value =
        serde_json::from_str(&plan_events("[]", "2026/09/14", "", "", &store.path)).unwrap();
    assert!(
        bad["error"].as_str().unwrap_or("").contains("YYYY-MM-DD"),
        "日期格式错误应给出提示: {}",
        bad["error"]
    );
}

/// 日期映射的强校验：**算出来的日期，星期几必须和课程条目里的星期几一致**。
///
/// 这正是用户反馈的「多处错位」最可能的表现形式：整份课表偏移一天或一周。
/// 光看「时间在不在作息表里」抓不到这类错误，必须逐条比对星期。
#[test]
fn every_event_lands_on_the_course_weekday_and_week() {
    let Some(html) = real_page_html() else {
        return;
    };
    let parsed: serde_json::Value =
        serde_json::from_str(&bupt_core::api::parse_html(&html)).unwrap();
    let entries: Vec<bupt_core::models::CourseEntry> =
        serde_json::from_value(parsed["entries"].clone()).unwrap();
    assert!(!entries.is_empty());

    let term_start = bupt_core::term::parse_date(TERM_START).unwrap();
    assert_eq!(
        chrono::Datelike::weekday(&term_start),
        chrono::Weekday::Mon,
        "第 1 周周一必须是周一"
    );

    let schedule = bupt_core::term::bell_schedule(bupt_core::term::SCHEDULE_MAINLAND);
    let events = bupt_core::term::build_events(&entries, term_start, &schedule, None);
    assert!(!events.is_empty());

    // 建索引用于反查。
    //
    // ⚠️ 不能只按课程名反查：同一门课可能在一周里上多次，
    // 例如「高等数学（一）」周一 1-3 节、周四 3-4 节。
    // 只按名字查会拿到错误的那一条，从而误报「错位」。
    // 最可靠的是直接用日程自带的 `source_index` 定位。
    use std::collections::HashMap;
    let mut index: HashMap<(String, i32, i32), ()> = HashMap::new();
    for e in &entries {
        index.insert((e.name.clone(), e.start_node, e.end_node), ());
    }

    for ev in &events {
        // ① 用 source_index 精确定位产生这条日程的课程条目
        let entry = entries
            .get(ev.source_index)
            .unwrap_or_else(|| panic!("source_index {} 越界", ev.source_index));
        assert_eq!(entry.name, ev.title, "source_index 指向的课程名与日程不一致");

        let date = bupt_core::term::parse_date(&ev.start[..10]).unwrap();
        let actual_weekday = chrono::Datelike::weekday(&date).num_days_from_monday() as i32 + 1;

        // ② 星期必须对上
        assert_eq!(
            actual_weekday, entry.day,
            "错位！课程「{}」({}-{}节) 在课表里是周{}，但算出来的日期 {} 是周{}",
            ev.title,
            entry.start_node,
            entry.end_node,
            entry.day,
            &ev.start[..10],
            actual_weekday
        );

        // ③ 该日期必须落在课程声明的某个周次内
        let weeks = entry.all_weeks();
        let delta_days = (date - term_start).num_days();
        assert!(delta_days >= 0, "算出了第 1 周之前的日期: {}", ev.start);
        let week_no = (delta_days / 7) as i32 + 1;
        assert!(
            weeks.contains(&week_no),
            "错位！课程「{}」({}-{}节) 声明周次 {:?}，但算出了第 {} 周（{}）",
            ev.title,
            entry.start_node,
            entry.end_node,
            weeks,
            week_no,
            &ev.start[..10]
        );

        // ④ 与日期公式互相印证
        let expected_date =
            term_start + chrono::Duration::days(((week_no - 1) * 7 + (entry.day - 1)) as i64);
        assert_eq!(date, expected_date, "日期公式不一致: {}", ev.start);

        // ⑤ 反向检查：这条日程对应的 (课程名, 节次) 组合必须真的存在于课表里
        assert!(
            index.contains_key(&(ev.title.clone(), entry.start_node, entry.end_node)),
            "产生了课表里不存在的课程组合: {} {}-{}节",
            ev.title,
            entry.start_node,
            entry.end_node
        );
    }
}

/// 周日（day=7）必须落在星期天，不能被算成周一。
///
/// 这一条专门针对 `Calendar.DAY_OF_WEEK`（周日=1）那类陷阱：
/// 代码里任何一处把周日和周一弄混，都会让周日课程错位。
#[test]
fn sunday_courses_land_on_sunday() {
    use bupt_core::models::{CourseEntry, WeekRange, WeekType};

    let entry = CourseEntry {
        name: "周日的课".into(),
        teacher: "测试".into(),
        room: "教1-101".into(),
        group: String::new(),
        day: 7, // 周日
        start_node: 1,
        end_node: 2,
        nodes_explicit: true,
        weeks: vec![WeekRange::new(1, 3, WeekType::Every)],
        raw: String::new(),
    };

    // 2026-09-14 是周一
    let week1_monday = bupt_core::term::parse_date("2026-09-14").unwrap();
    let schedule = bupt_core::term::bell_schedule(bupt_core::term::SCHEDULE_MAINLAND);
    let events = bupt_core::term::build_events(&[entry], week1_monday, &schedule, None);
    assert_eq!(events.len(), 3);

    for ev in &events {
        let date = bupt_core::term::parse_date(&ev.start[..10]).unwrap();
        assert_eq!(
            chrono::Datelike::weekday(&date),
            chrono::Weekday::Sun,
            "周日课程被算到了 {}（周{}）",
            &ev.start[..10],
            chrono::Datelike::weekday(&date).number_from_monday()
        );
    }

    // 第 1 周的周日 = 周一 + 6 天 = 2026-09-20
    assert_eq!(&events[0].start[..10], "2026-09-20");
    // 第 2 周的周日 = 2026-09-27
    assert_eq!(&events[1].start[..10], "2026-09-27");
}

/// 第 0 周（北邮存在）不能被算成第 -1 周或被丢弃。
#[test]
fn week_zero_courses_land_before_week_one() {
    use bupt_core::models::{CourseEntry, WeekRange, WeekType};

    let entry = CourseEntry {
        name: "第0周的课".into(),
        teacher: String::new(),
        room: String::new(),
        group: String::new(),
        day: 1,
        start_node: 1,
        end_node: 2,
        nodes_explicit: true,
        weeks: vec![WeekRange::new(0, 0, WeekType::Every)],
        raw: String::new(),
    };

    let week1_monday = bupt_core::term::parse_date("2026-09-14").unwrap();
    let schedule = bupt_core::term::bell_schedule(bupt_core::term::SCHEDULE_MAINLAND);
    let events = bupt_core::term::build_events(&[entry], week1_monday, &schedule, None);

    assert_eq!(events.len(), 1, "第 0 周的课应该被保留");
    assert_eq!(&events[0].start[..10], "2026-09-07", "第 0 周应是第 1 周的前一周");
    let date = bupt_core::term::parse_date(&events[0].start[..10]).unwrap();
    assert_eq!(chrono::Datelike::weekday(&date), chrono::Weekday::Mon);
}
