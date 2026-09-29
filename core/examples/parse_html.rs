//! 离线校验解析器：把一份课表 HTML 文件喂进来，打印解析结果与生成的日程。
//!
//! 用途：拿到真实教务页面（浏览器「另存为」或复制源码）后，
//! 不用装 App、不用手机，直接在桌面上确认解析是否正确。
//!
//! 用法：
//! ```text
//! cargo run --example parse_html -- 课表.html
//! cargo run --example parse_html -- 课表.html 2026-03-02        # 顺便展开日程
//! cargo run --example parse_html -- 课表.html 2026-03-02 ics    # 导出 .ics 供手机导入
//! ```

use std::env;
use std::fs;
use std::process::ExitCode;

use bupt_core::models::WeekType;
use bupt_core::parser::parse_timetable;
use bupt_core::term::{bell_schedule, build_events, parse_date, SCHEDULE_MAINLAND};

fn weekday_cn(day: i32) -> &'static str {
    match day {
        1 => "周一",
        2 => "周二",
        3 => "周三",
        4 => "周四",
        5 => "周五",
        6 => "周六",
        7 => "周日",
        _ => "??",
    }
}

fn week_type_cn(t: WeekType) -> &'static str {
    match t {
        WeekType::Every => "每周",
        WeekType::Odd => "单周",
        WeekType::Even => "双周",
    }
}

fn main() -> ExitCode {
    let args: Vec<String> = env::args().collect();
    if args.len() < 2 {
        eprintln!("用法: {} <课表.html> [第1周周一 YYYY-MM-DD] [ics]", args[0]);
        return ExitCode::from(2);
    }
    let path = &args[1];
    let html = match fs::read_to_string(path) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("读不了 {path}: {e}");
            return ExitCode::FAILURE;
        }
    };

    let report = parse_timetable(&html);

    println!("文件        : {path} ({} 字节)", html.len());
    println!("找到课表表格: {}", if report.found_table { "是" } else { "否" });
    println!("取值策略    : {}（title=按属性 / position=按节点顺序）", report.strategy);
    println!("课程条目数  : {}", report.entries.len());
    if !report.warnings.is_empty() {
        println!("告警        :");
        for w in &report.warnings {
            println!("  - {w}");
        }
    }
    println!();

    if report.entries.is_empty() {
        println!("没有解析出任何课程。请确认这份 HTML 是「课表查询」页面，并且课表已展开。");
        return ExitCode::FAILURE;
    }

    println!("{:<24} {:<4} {:<9} {:<12} {:<16} {}", "课程", "星期", "节次", "周次", "教师", "地点");
    println!("{}", "-".repeat(100));
    for e in &report.entries {
        let weeks: Vec<String> = e
            .weeks
            .iter()
            .map(|w| {
                if w.start == w.end {
                    format!("{}{}", w.start, week_type_cn(w.week_type))
                } else {
                    format!("{}-{}{}", w.start, w.end, week_type_cn(w.week_type))
                }
            })
            .collect();
        println!(
            "{:<24} {:<4} {:<9} {:<12} {:<16} {}",
            truncate(&e.name, 22),
            weekday_cn(e.day),
            format!("{}-{}节", e.start_node, e.end_node),
            weeks.join(","),
            truncate(&e.teacher, 14),
            e.room,
        );
    }

    // --debug：打印每条记录的原始 HTML，用于排查「字段取错位」这类问题
    if args.iter().any(|a| a == "--debug") {
        println!();
        println!("=== 原始 HTML（清理后）===");
        for (i, e) in report.entries.iter().enumerate() {
            println!(
                "[{i:>3}] name={:?} teacher={:?} room={:?} day={} nodes={}-{}\n      raw={:?}",
                e.name, e.teacher, e.room, e.day, e.start_node, e.end_node, e.raw
            );
        }
    }

    // 汇总每个星期几有多少节课，方便一眼看出是否解析错位
    println!();
    let mut per_day = [0usize; 8];
    for e in &report.entries {
        if (1..=7).contains(&e.day) {
            per_day[e.day as usize] += 1;
        }
    }
    print!("每天条目数  :");
    for d in 1..=7usize {
        print!(" {}={}", weekday_cn(d as i32), per_day[d]);
    }
    println!();

    // 可选：展开成具体日程
    if args.len() >= 3 {
        let term_start = match parse_date(&args[2]) {
            Ok(d) => d,
            Err(e) => {
                eprintln!("{e}");
                return ExitCode::FAILURE;
            }
        };
        let schedule = bell_schedule(SCHEDULE_MAINLAND);
        let events = build_events(&report.entries, term_start, &schedule, None);
        println!();
        println!(
            "按第 1 周周一 = {} 展开，共 {} 条日程（前 12 条）：",
            term_start,
            events.len()
        );
        for ev in events.iter().take(12) {
            println!(
                "  {} ~ {}  {}  @ {}",
                ev.start, ev.end, ev.title, ev.location
            );
        }
        if events.len() > 12 {
            println!("  … 其余 {} 条略", events.len() - 12);
        }

        // 导出 .ics（可以直接发到手机上导入系统日历）
        if args.get(3).map(|s| s == "ics").unwrap_or(false) {
            let ics = to_ics(&events);
            let out = "课表.ics";
            match fs::write(out, ics) {
                Ok(_) => println!("\n已导出 {out}（{} 条日程）", events.len()),
                Err(e) => eprintln!("导出失败: {e}"),
            }
        }
    }

    ExitCode::SUCCESS
}

fn truncate(s: &str, n: usize) -> String {
    let chars: Vec<char> = s.chars().collect();
    if chars.len() <= n {
        s.to_string()
    } else {
        chars[..n].iter().collect::<String>() + "…"
    }
}

/// 生成 iCalendar 文本。只用于桌面端调试，App 本身是直接写系统日历的。
fn to_ics(events: &[bupt_core::models::CalendarEvent]) -> String {
    let mut out = String::from(
        "BEGIN:VCALENDAR\r\nVERSION:2.0\r\nPRODID:-//bupt-calendar//Rust//CN\r\nCALSCALE:GREGORIAN\r\n",
    );
    for ev in events {
        // 2026-03-02T08:00:00 -> 20260302T080000
        let compact = |s: &str| s.replace(['-', ':'], "").replace('T', "T");
        out.push_str("BEGIN:VEVENT\r\n");
        out.push_str(&format!("UID:{}\r\n", ev.fingerprint));
        out.push_str(&format!("DTSTART:{}\r\n", compact(&ev.start)));
        out.push_str(&format!("DTEND:{}\r\n", compact(&ev.end)));
        out.push_str(&format!("SUMMARY:{}\r\n", escape_ics(&ev.title)));
        out.push_str(&format!("LOCATION:{}\r\n", escape_ics(&ev.location)));
        out.push_str(&format!(
            "DESCRIPTION:{}\r\n",
            escape_ics(&ev.description.replace('\n', "\\n"))
        ));
        out.push_str("END:VEVENT\r\n");
    }
    out.push_str("END:VCALENDAR\r\n");
    out
}

fn escape_ics(s: &str) -> String {
    s.replace('\\', "\\\\")
        .replace(';', "\\;")
        .replace(',', "\\,")
}
