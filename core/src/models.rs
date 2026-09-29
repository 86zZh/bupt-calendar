//! 核心数据模型。
//!
//! 命名尽量贴近强智 jsxsd 的字段语义，方便与页面结构对照排查。

use serde::{Deserialize, Serialize};

/// 单双周 / 每周标记。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum WeekType {
    /// 每周都上
    Every,
    /// 单周
    Odd,
    /// 双周
    Even,
}

impl Default for WeekType {
    fn default() -> Self {
        WeekType::Every
    }
}

impl WeekType {
    /// 判断某个周次是否命中该单双周规则。
    pub fn matches(self, week: i32) -> bool {
        match self {
            WeekType::Every => true,
            WeekType::Odd => week % 2 == 1,
            WeekType::Even => week % 2 == 0,
        }
    }
}

/// 一段连续的周次区间，例如 `1-16周` 或 `3-5周(单)`。
///
/// 强智的周次字符串允许多段并用逗号分隔，例如 `1-4,6-8,10周`，
/// 也允许单点，例如 `5周`。解析后统一归一化成若干 `WeekRange`。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WeekRange {
    pub start: i32,
    pub end: i32,
    #[serde(default)]
    pub week_type: WeekType,
}

impl WeekRange {
    pub fn new(start: i32, end: i32, week_type: WeekType) -> Self {
        // 容错：start > end 时交换，负数/0 周次在解析阶段已被过滤
        if start > end {
            WeekRange { start: end, end: start, week_type }
        } else {
            WeekRange { start, end, week_type }
        }
    }

    /// 展开成实际周次列表（已应用单双周过滤）。
    pub fn expand(&self) -> Vec<i32> {
        (self.start..=self.end)
            .filter(|w| self.week_type.matches(*w))
            .collect()
    }
}

/// 一条课表条目，对应页面上 `div.kbcontent` 里的一个课程块。
///
/// 注意：同一格里的多门课、以及同一门课的多个周次段，
/// 在解析阶段会被拆成多条 `CourseEntry`。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CourseEntry {
    /// 课程名，例如「高等数学」
    pub name: String,
    /// 教师，可能为空
    #[serde(default)]
    pub teacher: String,
    /// 教室 / 地点，可能为空
    #[serde(default)]
    pub room: String,
    /// 分组信息（强智的 `title="分组"`），可能为空
    #[serde(default)]
    pub group: String,
    /// 星期几，1=周一 … 7=周日
    pub day: i32,
    /// 起始小节（1 基），例如 1 表示第 1 节
    pub start_node: i32,
    /// 结束小节（1 基，含）
    pub end_node: i32,
    /// 节次是否来自页面里**显式的**节次信息（`[01-02节]`），
    /// 还是仅仅沿用了所在单元格的大节序号。
    ///
    /// 强智页面把同一门课拆成「隐藏摘要版（无节次）」和「可见详情版（有节次）」，
    /// 合并时需要靠这个标志判断哪一条才代表真实的节次范围。
    #[serde(default)]
    pub nodes_explicit: bool,
    /// 周次区间，可多段
    pub weeks: Vec<WeekRange>,
    /// 课表单元格里的原始文本，保留用于排查解析问题
    #[serde(default)]
    pub raw: String,
}

impl CourseEntry {
    /// 课程占用的所有周次（去重并排序）。
    pub fn all_weeks(&self) -> Vec<i32> {
        let mut v: Vec<i32> = self.weeks.iter().flat_map(|r| r.expand()).collect();
        v.sort_unstable();
        v.dedup();
        v
    }

    /// 节次是否来自页面显式信息（而非单元格位置推断）。
    pub fn has_explicit_nodes(&self) -> bool {
        self.nodes_explicit
    }

    /// 该条目是否有效（课程名非空且小节合理）。
    pub fn is_valid(&self) -> bool {
        !self.name.trim().is_empty()
            && self.start_node >= 1
            && self.end_node >= self.start_node
            && self.day >= 1
            && self.day <= 7
            && !self.weeks.is_empty()
    }
}

/// 一条最终要写进系统日历的日程。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CalendarEvent {
    /// 去重/清除用的稳定指纹（sha256 十六进制）
    pub fingerprint: String,
    pub title: String,
    pub description: String,
    pub location: String,
    /// 本地开始时间，`YYYY-MM-DDTHH:MM:SS`
    pub start: String,
    /// 本地结束时间，`YYYY-MM-DDTHH:MM:SS`
    pub end: String,
    /// 开始时间的 epoch 毫秒（已按本机时区换算），供 Android 直接写入系统日历。
    ///
    /// 直接给毫秒是为了避免在 Kotlin 侧重新解析字符串——那需要两边对时区和
    /// 格式达成一致，是很容易出错的地方。
    #[serde(default)]
    pub start_millis: i64,
    /// 结束时间的 epoch 毫秒
    #[serde(default)]
    pub end_millis: i64,
    /// 全天事件（当前不产生，预留）
    #[serde(default)]
    pub all_day: bool,
    /// 该日程对应的课表条目在源数组中的下标，便于回查
    pub source_index: usize,
}

/// 作息表里的一小节。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Section {
    /// 小节序号，1 基
    pub index: i32,
    /// 开始时间 `HH:MM`
    pub start: String,
    /// 结束时间 `HH:MM`
    pub end: String,
}

/// 作息表（一整套节次时间）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BellSchedule {
    pub id: String,
    pub name: String,
    /// 小节列表，按 index 升序
    pub sections: Vec<Section>,
}

impl BellSchedule {
    pub fn section(&self, index: i32) -> Option<&Section> {
        self.sections.iter().find(|s| s.index == index)
    }
}

/// 一门课的统计摘要，只用于给用户展示，不参与写日历。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CourseSummary {
    pub name: String,
    pub teacher: String,
    pub room: String,
    /// 节次数（课表条目数）
    pub entries: usize,
    /// 最终展开出的日程条数
    pub events: usize,
}
