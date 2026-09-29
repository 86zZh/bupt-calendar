//! 北邮新教务课表 → 系统日历 的核心逻辑（Rust）。
//!
//! 模块划分：
//! * [`models`] —— 数据模型（课表条目、周次、作息表、日历日程）
//! * [`parser`] —— 强智 jsxsd 课表页 HTML 解析
//! * [`term`]   —— 作息时间表与「周次 → 日期」换算、日程生成
//! * [`store`]  —— 导入记录（去重 + 一键清除的依据）
//! * [`api`]    —— 给 Kotlin 用的高层门面（不依赖 JNI，可单元测试）
pub mod api;
pub mod models;
pub mod parser;
pub mod store;
pub mod term;

/// JNI 导出层只在 Android 目标下编译：
/// 桌面端跑 `cargo test` 时不需要（也无法）链接 JNI。
#[cfg(target_os = "android")]
pub mod jni_bridge;

pub use models::*;
pub use parser::{parse_documents, parse_timetable, ParseReport};
pub use store::{ImportBatch, ImportOutcome, Store};
pub use term::{bell_schedule, build_events, parse_date, SCHEDULE_MAINLAND};
