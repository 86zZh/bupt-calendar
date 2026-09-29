//! 强智科技（QZDATASOFT）jsxsd 课表页解析。
//!
//! 目标页面：`https://jwgl.bupt.edu.cn/jsxsd/xskb/xskb_list.do`
//! （校外需经 `https://webvpn.bupt.edu.cn/` ）
//!
//! # 页面结构（依据多个独立实现交叉验证）
//!
//! ```html
//! <table id="kbtable">
//!   <tr>…<th>星期一</th><th>星期二</th>…</tr>   <!-- 表头 -->
//!   <tr>…                                        <!-- 每一行 = 一大节 -->
//!     <td>                                        <!-- 每一列 = 星期几 -->
//!       <div class="kbcontent" id="kbcontent_1-1">
//!         <font title="课程名称">高等数学</font><br>
//!         <font title="老师">张教授</font><br>
//!         <font title="周次(节次)">1-16(周)[01-02节]</font><br>
//!         <font title="教室">教1-201</font>
//!       </div>
//!     </td>
//!   </tr>
//! </table>
//! ```
//!
//! * 同一格里多门课以 `-----`（强智用 `--------------------`）分隔。
//! * 每个 `font` 都带 `title` 属性，这是**最稳的取值通道**（策略 A）。
//! * 若 `title` 缺失，则退回按子节点顺序取值（策略 B）：
//!   依次为 课程名 / 教师 / 周次 / 节次 / 地点 —— 这条路径与
//!   `Qiangzhi_to_ics/get_kb.py` 的实现一致。
//!
//! 解析器只依赖 HTML 文本，不依赖网络，因此可以用真实页面另存的 HTML
//! 直接做单元测试（见 `tests/`）。

use scraper::{ElementRef, Html, Selector};
use serde::{Deserialize, Serialize};

use crate::models::{CourseEntry, WeekRange, WeekType};

/// 解析结果，连同诊断信息一起返回，方便在手机上报错时定位问题。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ParseReport {
    pub entries: Vec<CourseEntry>,
    /// 解析过程中的告警（不致命）
    pub warnings: Vec<String>,
    /// 使用的取值策略："title" / "position" / "mixed"
    pub strategy: String,
    /// 是否找到了 `#kbtable`
    pub found_table: bool,
}

impl ParseReport {
    fn warn(&mut self, msg: impl Into<String>) {
        let msg = msg.into();
        if !self.warnings.contains(&msg) {
            self.warnings.push(msg);
        }
    }
}

/// 解析课表 HTML，自动选择可用的取值策略。
pub fn parse_timetable(html: &str) -> ParseReport {
    parse_documents(&[html.to_string()])
}

/// 解析**多个** HTML 文档（主页面 + 各个内嵌框架），把结果合并。
///
/// 为什么需要：北邮教务的课表经常显示在 iframe / frame 里，主文档本身没有课表。
/// 只解析主文档就会出现「人明明在课表页上，却解析不出课程」。
///
/// 合并策略：
/// * 课程条目全部汇总，然后做一次全局去重（跨框架可能有重复内容）；
/// * 只要**任意一个**文档里找到了课表表格，就算找到了；
/// * 告警只保留有意义的那条（找不到表格时不重复刷屏）。
pub fn parse_documents(docs: &[String]) -> ParseReport {
    let mut merged = ParseReport::default();

    for html in docs {
        let report = parse_single_document(html);
        merged.found_table |= report.found_table;
        if report.entries.is_empty() && !report.warnings.is_empty() && !merged.found_table {
            // 主文档没找到表格时保留它的提示，便于告诉用户「先打开课表页」
            for w in report.warnings {
                merged.warn(w);
            }
        }
        merged.entries.extend(report.entries);
        // 策略取信息量更大的那个（title 比 position 可靠）
        if strategy_rank(&report.strategy) > strategy_rank(&merged.strategy) {
            merged.strategy = report.strategy;
        }
    }

    // 跨文档去重，避免主页面与框架内容重叠时产生重复
    let before = merged.entries.len();
    merged.entries = dedupe_global(merged.entries);
    if before != merged.entries.len() {
        merged.warn(format!(
            "合并多个页面内容后去除了 {} 条重复课程",
            before - merged.entries.len()
        ));
    }

    if merged.entries.is_empty() && merged.found_table {
        merged.warn(
            "找到了课表表格但没有解析出任何课程：可能是课表处于折叠状态，或当前学期还没有课",
        );
    }
    merged
}

fn strategy_rank(s: &str) -> u8 {
    match s {
        "title" => 3,
        "mixed" => 2,
        "position" => 1,
        _ => 0,
    }
}

fn parse_single_document(html: &str) -> ParseReport {
    let mut report = ParseReport::default();
    let doc = Html::parse_document(html);

    let Some(table) = find_timetable(&doc) else {
        report.warn("没有在页面里找到课表表格（#kbtable），请确认已经到达「课表查询/我的课表」页面");
        return report;
    };
    report.found_table = true;

    let mut title_hits = 0usize;
    let mut position_hits = 0usize;

    for (day, node, divs) in iter_cells(table) {
        // 先把这一格里的所有课程块都解析出来，再整格去重
        let mut cell_entries: Vec<CourseEntry> = Vec::new();
        for div in divs {
            let raw = element_text(div);
            if raw.trim().is_empty() || is_placeholder(&raw) {
                continue;
            }

            for chunk_html in split_course_html(div) {
                let chunk_raw = element_html_text(&chunk_html);
                if chunk_raw.trim().is_empty() || is_placeholder(&chunk_raw) {
                    continue;
                }

                match parse_chunk_by_title(&chunk_html, day, node, &chunk_raw) {
                    Some(entry) => {
                        title_hits += 1;
                        cell_entries.push(entry);
                    }
                    None => match parse_chunk_by_position(&chunk_html, day, node, &chunk_raw) {
                        Some(entry) => {
                            position_hits += 1;
                            cell_entries.push(entry);
                        }
                        None => report.warn(format!(
                            "有一格无法解析，已跳过（第{day}列/第{node}行）：{}",
                            truncate(&chunk_raw, 60)
                        )),
                    },
                }
            }
        }

        report.entries.extend(dedupe_cell(cell_entries));
    }

    // 全表去重：同一门课可能在同一格被重复渲染，或跨行出现完全相同的条目。
    // 判据用「课程名 + 星期 + 节次 + 周次 + 教师 + 地点」，完全一致才算重复。
    report.entries = dedupe_global(report.entries);

    report.strategy = match (title_hits, position_hits) {
        (0, 0) => "none",
        (_, 0) => "title",
        (0, _) => "position",
        _ => "mixed",
    }
    .to_string();

    if report.entries.is_empty() && report.found_table {
        report.warn(
            "找到了课表表格但没有解析出任何课程：可能是课表处于折叠状态，或当前学期还没有课",
        );
    }

    report
}

/// 只做「页面是否包含课表」的快速判断，用于导入前给出更准确的提示。
pub fn has_timetable(html: &str) -> bool {
    let doc = Html::parse_document(html);
    find_timetable(&doc).is_some()
}

// ---------------------------------------------------------------------------
// 表格定位与遍历
// ---------------------------------------------------------------------------

fn find_timetable(doc: &Html) -> Option<ElementRef<'_>> {
    // 首选 id="kbtable"（强智个人课表标准 id）
    if let Ok(sel) = Selector::parse("#kbtable") {
        if let Some(el) = doc.select(&sel).next() {
            return Some(el);
        }
    }
    // 退路：包含 星期X 表头、且含 kbcontent 的表格
    if let Ok(sel) = Selector::parse("table") {
        for table in doc.select(&sel) {
            let has_day_header = table.text().any(|t| {
                let t = t.trim();
                t.starts_with("星期") || t.starts_with("周一")
            });
            if !has_day_header {
                continue;
            }
            if let Ok(inner) = Selector::parse("div.kbcontent, div.kbcontent1, td[title]") {
                if table.select(&inner).next().is_some() {
                    return Some(table);
                }
            }
        }
    }
    None
}

/// 解析表头行，得出每个 `<td>` 位置对应的星期几。
///
/// 强智的表头形如 `["", "星期一", "星期二", … "星期日"]`，**第一格是节次表头**，
/// 不是星期。这里按标签文本建立 `列下标 -> 星期几` 的映射，比「假设第一格
/// 一定是节次标签」更稳：即使某校把星期列顺序调换或缺少某一列也能对上。
fn header_day_map(header_row: ElementRef<'_>) -> Vec<i32> {
    let mut map = Vec::new();
    let mut fallback = 0;
    for td in header_row.children().filter_map(ElementRef::wrap) {
        if !matches!(td.value().name(), "td" | "th") {
            continue;
        }
        let text = element_text(td);
        let day = day_from_label(&text);
        match day {
            Some(d) => map.push(d),
            None => {
                // 非星期表头（通常是"节次"那一格）标记为 0，后续跳过
                fallback += 1;
                let _ = fallback;
                map.push(0);
            }
        }
    }
    map
}

fn day_from_label(text: &str) -> Option<i32> {
    let t = text.trim();
    // 形如「星期一」「周一」「星期一 Monday」
    for (i, name) in ["一", "二", "三", "四", "五", "六", "日"].iter().enumerate() {
        if t.contains(&format!("星期{name}")) || t.contains(&format!("周{name}")) {
            return Some(i as i32 + 1);
        }
    }
    if t.contains("星期天") || t.contains("周天") {
        return Some(7);
    }
    None
}

/// 从单元格里课程 div 的 `id` 推断星期几。
///
/// 强智的课程块 id 有两种格式，都带星期几：
/// * 新格式：`<32位教学班id>-<星期>-<类型>`，例如
///   `C63D887128C64AD79EFD17454B91A0DB-2-2` —— 倒数第二段 `2` 就是星期二；
/// * 旧格式：`kbcontent_<星期>-<大节>`，例如 `kbcontent_1-3` —— 下划线后
///   第一个数字 `1` 就是星期一。
///
/// 这是**最可靠**的星期来源：它不依赖表格列数、表头写法、行是否缺列。
/// 同一格里多个课程块的 id 星期段应当一致；取第一个有效的。
fn day_from_cell_divs(cell: ElementRef<'_>) -> Option<i32> {
    let Ok(sel) = Selector::parse("div.kbcontent, div.kbcontent1") else {
        return None;
    };
    for div in cell.select(&sel) {
        if let Some(day) = day_from_kbcontent_id(div.value().attr("id").unwrap_or("")) {
            return Some(day);
        }
    }
    None
}

/// 解析单个 kbcontent div id 里的星期几（1..7）。
pub(crate) fn day_from_kbcontent_id(id: &str) -> Option<i32> {
    if id.is_empty() {
        return None;
    }
    // 新格式：<32hex>-<day>-<type>（或更多段），星期在倒数第二段
    let segments: Vec<&str> = id.split('-').collect();
    if segments.len() >= 2 {
        if let Ok(d) = segments[segments.len() - 2].parse::<i32>() {
            if (1..=7).contains(&d) {
                return Some(d);
            }
        }
    }
    // 旧格式：kbcontent_<day>-<node> / kbcontent1_<day>-<node>
    let first = segments.first().copied().unwrap_or("");
    if let Some(underscore) = first.rfind('_') {
        if let Ok(d) = first[underscore + 1..].parse::<i32>() {
            if (1..=7).contains(&d) {
                return Some(d);
            }
        }
    }
    None
}

/// 遍历课表里所有「课程格」，产出 (星期几, 大节序号, 该格里的课程块列表)。
///
/// # 星期几（day）从哪来 —— 按可靠程度排序
///
/// 1. **课程 div 的 `id`**（最可靠）。强智官方约定：
///    新格式 `<教学班id>-<星期>-<类型>`，例如 `C63D887128C64AD79EFD17454B91A0DB-2-2`
///    是「星期二」列的课程块；旧格式 `kbcontent_1-3` 是「星期1」。
///    这个字段不受表格列数、表头写法影响。
/// 2. **表头映射**（表头里写着「星期一…星期日」的那一行）。
/// 3. 两者都拿不到时**跳过并告警**，绝不瞎猜。
///
/// # 曾经的血案
///
/// 早期兜底逻辑在「数据行列数与表头不一致」时按列号直接猜 day（`col as i32`），
/// 并把第 0 列当节次标签丢掉。一旦某行的节次标签是 `<th>` 或被页面改写掉
/// （WebVPN 下会发生），整行就会整体偏移一格：
/// 周一课消失、周二课跑到周一、周三课与周二重叠……和用户报告的症状完全一致。
/// 所以现在：列对齐只允许「长度一致」或「数据行恰好比表头少一个节次标签列」，
/// 其余一律跳过该行并告警，宁缺勿错。
fn iter_cells<'a>(table: ElementRef<'a>) -> Vec<(i32, i32, Vec<ElementRef<'a>>)> {
    let mut out = Vec::new();
    let Ok(tr_sel) = Selector::parse("tr") else {
        return out;
    };

    let rows: Vec<ElementRef<'a>> = table.select(&tr_sel).collect();
    // 找到表头行
    let header_index = rows
        .iter()
        .position(|tr| header_day_map(*tr).iter().any(|d| *d > 0));
    let day_map = header_index.map(|i| header_day_map(rows[i])).unwrap_or_default();

    let mut node = 0;
    for (row_index, tr) in rows.iter().enumerate() {
        if Some(row_index) == header_index {
            continue;
        }

        // td 与 th 都算作一列（节次标签有时是 <th>，只收 td 会丢列导致错位）
        let cells: Vec<ElementRef<'a>> = tr
            .children()
            .filter_map(ElementRef::wrap)
            .filter(|e| matches!(e.value().name(), "td" | "th"))
            .collect();
        if cells.is_empty() {
            continue;
        }

        // 整行都不含课程（例如纯节次标签行）则不计入大节序号
        let has_course = cells.iter().any(|td| {
            Selector::parse("div.kbcontent, div.kbcontent1")
                .map(|sel| td.select(&sel).next().is_some())
                .unwrap_or(false)
        });
        if !has_course {
            continue;
        }
        node += 1;

        // 数据行与表头的列对齐：
        //   长度一致          -> 一一对应
        //   数据行少一格      -> 少的是最左的节次标签列，数据行 col 对应表头 col+1
        //   其他任何不一致    -> 不猜，跳过（由 div id 兜底，见下）
        let col_offset: Option<usize> = if day_map.len() == cells.len() {
            Some(0)
        } else if cells.len() + 1 == day_map.len() {
            Some(1)
        } else {
            None
        };

        for (col, td) in cells.iter().enumerate() {
            // 第一优先：从课程 div 的 id 里读星期几（最可靠，不受列对齐影响）
            let day = day_from_cell_divs(*td).or_else(|| {
                col_offset.and_then(|off| day_map.get(col + off).copied())
            }).unwrap_or(0);
            if day <= 0 {
                continue; // 节次标签格 / 无法确定列，跳过
            }
            let mut divs: Vec<ElementRef<'a>> = Vec::new();
            if let Ok(sel) = Selector::parse("div.kbcontent, div.kbcontent1") {
                for div in td.select(&sel) {
                    // 跳过空块与页面自带的骨架模板 div（见 `is_skeleton`）
                    if element_content_text(div).trim().is_empty() || is_skeleton(div) {
                        continue;
                    }
                    divs.push(div);
                }
            }
            if divs.is_empty() {
                if element_text(*td).trim().is_empty() {
                    continue;
                }
                // 没有 kbcontent 的版本：整格当一个课程块
                out.push((day, node, vec![*td]));
            } else {
                // 同一格里可能有多个课程块，必须一起交给 `dedupe_cell`，
                // 否则隐藏摘要版与可见详情版会被当成两门课。
                out.push((day, node, divs));
            }
        }
    }
    out
}

/// 一格内容转纯文本（供判空）。
fn element_content_text(el: ElementRef<'_>) -> String {
    clean_text(&el.text().collect::<Vec<_>>().join(" "))
}

// ---------------------------------------------------------------------------
// `-----` 分隔符的深度感知切分
// ---------------------------------------------------------------------------

/// 把一格里的多门课按 `-----` 拆开。
///
/// 强智把分隔符写在课程内容里（形如 `<font color="red">--------------------</font>`），
/// 因此它可能出现在任意嵌套深度，不能要求「深度为 0」才切分。
///
/// 判据是**连续 4 个及以上的 `-`**：这样既能认出一长串的强智分隔符，
/// 又不会误伤课表里常见的 `1-2节`、`3-5周` 这类单短横。
/// 返回的每段都是完整的 HTML 片段，可独立交给 DOM 解析。
pub fn split_courses_by_dashes(html: &str) -> Vec<String> {
    let bytes = html.as_bytes();
    let mut chunks = Vec::new();
    let mut start = 0usize;
    let mut i = 0usize;

    while i < bytes.len() {
        if bytes[i] == b'-' {
            let mut k = i;
            while k < bytes.len() && bytes[k] == b'-' {
                k += 1;
            }
            if k - i >= 4 {
                chunks.push(html[start..i].to_string());
                start = k;
                i = k;
                continue;
            }
            i = k;
            continue;
        }
        i += 1;
    }
    chunks.push(html[start..].to_string());
    chunks
}

/// 把一格的 DOM 拆成多个课程块。
///
/// 强智有两种「一格多课」的写法：
/// 1. 多个 `-----` 分隔的课程块写在**同一个** `div.kbcontent` 里；
/// 2. 同一 `td` 里并列多个 `div.kbcontent`（例如隐藏的 `kbcontent`
///    与可见的 `kbcontent1`）—— 这种情况在 [`iter_cells`] 里已经拆开。
///
/// 这里处理情况 1：把 HTML 按 `-----` 切片后各自解析成独立文档。
/// 因为 `scraper` 的 `Html` 生命周期绑定在输入字符串上，无法在
/// `ElementRef<'a>` 上返回新文档的元素引用，所以切分结果以 HTML 片段
/// 返回给调用方，由调用方逐段解析。
fn split_course_html(div: ElementRef<'_>) -> Vec<String> {
    let html = div.html();
    if !html.contains("-----") {
        return vec![html];
    }
    // 同一 div 里若已有多个 kbcontent，说明是并列写法，不必再按横线切
    if let Ok(sel) = Selector::parse("div.kbcontent, div.kbcontent1") {
        if div.select(&sel).count() > 1 {
            return vec![html];
        }
    }
    split_courses_by_dashes(&html)
        .into_iter()
        .filter(|c| !element_html_text(c).is_empty())
        .collect()
}

/// 把一段 HTML 片段转成纯文本（用于判空与展示）。
fn element_html_text(html: &str) -> String {
    let frag = Html::parse_fragment(html);
    clean_text(&frag.root_element().text().collect::<Vec<_>>().join(" "))
}

// ---------------------------------------------------------------------------
// 策略 A：读 `title` 属性
// ---------------------------------------------------------------------------

fn parse_chunk_by_title(html: &str, day: i32, node: i32, raw: &str) -> Option<CourseEntry> {
    let doc = Html::parse_fragment(html);
    let chunk_root = doc.root_element();

    // 课程名：优先 title 属性；北邮版本没有这个属性，
    // 而是把课程名写成 `div` 的**直接文本子节点**，紧跟一个 `<br>`：
    //   <div class="kbcontent">课程名<br><font title="老师">…</font>…</div>
    let name = {
        let t = title_text(&doc, &["课程名称", "课程名"]);
        if !t.is_empty() {
            first_line(&t)
        } else {
            direct_name_text(chunk_root)
        }
    };
    if name.is_empty() {
        // 策略 A 在这一块上没拿到课程名 → 交给策略 B 再试
        return None;
    }

    let teacher = title_text(&doc, &["老师", "教师"]);
    let room = title_text(&doc, &["教室", "地点"]);
    let group = title_text(&doc, &["分组"]);

    // 周次与节次都藏在 `title="周次(节次)"` 里（也兼容只有 `title="周次"` / `"节次"` 的版本）
    let mut weeks: Vec<WeekRange> = Vec::new();
    let mut node_range: Option<(i32, i32)> = None;
    if let Ok(sel) = Selector::parse("[title]") {
        for el in doc.select(&sel) {
            let title = el.value().attr("title").unwrap_or("").trim().to_string();
            let text = element_text(el);
            if title.is_empty() {
                continue;
            }
            if title.contains("周次") {
                // 形如 `11-18(周)` 或 `3-18(周)[01-02-03节]`
                let week_part = text.split('[').next().unwrap_or(&text).to_string();
                if weeks.is_empty() {
                    weeks = parse_weeks(&week_part);
                }
                // 节次可能也在这个属性里
                if node_range.is_none() {
                    if let Some((s, e)) = parse_bracket_node(&text) {
                        node_range = Some((s, e));
                    }
                }
            } else if title.contains("节次") && node_range.is_none() {
                if let Some((s, e)) = parse_node_range(&text) {
                    node_range = Some((s, e));
                }
            }
        }
    }

    // 有的版本把周次放在 `<font title="周次">` 之外的裸文本里，兜底再试一次
    if weeks.is_empty() {
        weeks = parse_weeks(&text_all(&doc));
    }
    if weeks.is_empty() {
        return None;
    }

    let (start_node, end_node) = node_range.unwrap_or((node, node));
    let entry = CourseEntry {
        name,
        teacher,
        room,
        group,
        day,
        start_node,
        end_node,
        // 页面里没有节次信息时，暂时沿用单元格所在的大节序号
        nodes_explicit: node_range.is_some(),
        weeks,
        raw: raw.to_string(),
    };
    entry.is_valid().then_some(entry)
}

/// 取文本的第一行（`<br>` 之前的内容）。
fn first_line(text: &str) -> String {
    text.split('\n').next().unwrap_or("").trim().to_string()
}

/// 取课程块里 `<br>` 之前的**直接文本子节点**作为课程名。
///
/// 为什么不能用 `root_element().text()`：那样会把所有后代文本拼在一起，
/// `<br>` 本身不产生文本节点，于是「课程名」和「老师」会被粘成一行
/// （`课程名教师名` 粘成一行）。课程名只存在于 `<br>` 之前的直接文本里。
fn direct_name_text(root: ElementRef<'_>) -> String {
    let mut out = String::new();
    for child in root.children() {
        if let Some(el) = ElementRef::wrap(child) {
            if el.value().name() == "br" {
                break; // 遇到第一个 <br> 就结束，后面是老师/周次/教室
            }
            continue; // 其他元素（font 等）不属于课程名
        }
        if let Some(text) = child.value().as_text() {
            out.push_str(text);
        }
    }
    clean_text(&out)
}

/// 判断一个 `div` 是不是页面自带的**骨架模板**。
///
/// 北邮这套强智页面在每个单元格里除了放真实课程数据，还会放两个空的模板
/// div（`class="kbcontent1 sykb1"` / `class="kbcontent sykb2"`），供前端
/// JavaScript 克隆用。它们的 `id` 与真实课程 div 完全相同，如果不过滤，
/// 克隆出来会看起来像「同名的另一门课」，从而产生重复条目。
fn is_skeleton(div: ElementRef<'_>) -> bool {
    div.value()
        .classes()
        .any(|c| c.starts_with("sykb"))
}

/// 同一格内合并互补信息并去重。
///
/// 真实页面把同一门课拆成两个 div：
/// * 隐藏的 `div.kbcontent1` —— 课程名 + `周次(周)` + 教室，**没有节次、没有教师**
/// * 可见的 `div.kbcontent`  —— 课程名 + 教师 + `周次(周)[节次]` + 教室
///
/// 但同一格里的同一门课**可能横跨多个大节**（例如周一既在 1-2 节又在 3-4 节），
/// 那些是必须保留的独立条目。所以不能用「课程名」当键简单去重，否则会把
/// 不同大节的两条错误合并、又因 `node` 不同而留下重复。
///
/// 采用两遍算法：
/// 1. 所有**带节次**的块作为锚点，按「课程名 + 节次」保留（不同节次互不合并）；
/// 2. 不带节次的摘要块，只在「同名且同名只有一个锚点」时才并进那个锚点，
///    否则原样保留。
fn dedupe_cell(entries: Vec<CourseEntry>) -> Vec<CourseEntry> {
    let _ = &entries;
    dedupe_cell_impl(entries)
}

fn dedupe_cell_impl(entries: Vec<CourseEntry>) -> Vec<CourseEntry> {
    use std::collections::HashMap;

    // 第一遍：带节次的块按 (课程名, 节次) 去重
    let mut anchored: Vec<CourseEntry> = Vec::new();
    let mut anchor_of: HashMap<String, usize> = HashMap::new();
    let mut anchor_count: HashMap<String, usize> = HashMap::new();
    let mut loose: Vec<CourseEntry> = Vec::new();

    for e in entries {
        if e.has_explicit_nodes() {
            let key = format!("{}\u{1f}{}-{}", e.name, e.start_node, e.end_node);
            match anchor_of.get(&key) {
                Some(&idx) => merge_into(&mut anchored[idx], e),
                None => {
                    anchor_of.insert(key, anchored.len());
                    *anchor_count.entry(e.name.clone()).or_insert(0) += 1;
                    anchored.push(e);
                }
            }
        } else {
            loose.push(e);
        }
    }

    // 第二遍：摘要块能对上唯一锚点就并进去，否则单独保留
    for e in loose {
        let name = e.name.clone();
        if anchor_count.get(&name).copied().unwrap_or(0) == 1 {
            let unique = anchor_of
                .iter()
                .find(|(k, _)| k.starts_with(&format!("{name}\u{1f}")))
                .map(|(_, &idx)| idx);
            if let Some(idx) = unique {
                merge_into(&mut anchored[idx], e);
                continue;
            }
        }
        anchored.push(e);
    }

    anchored
}

/// 把 `src` 的互补信息并进 `dst`（`dst` 为信息更全的一方）。
fn merge_into(dst: &mut CourseEntry, src: CourseEntry) {
    // 周次取并集：摘要版可能是整学期，详情版可能是实际上课周，都要保留
    for w in &src.weeks {
        if !dst.weeks.contains(w) {
            dst.weeks.push(w.clone());
        }
    }
    dst.weeks.sort_by_key(|w| (w.start, w.end));
    if dst.teacher.trim().is_empty() {
        dst.teacher = src.teacher;
    }
    if dst.room.trim().is_empty() {
        dst.room = src.room;
    }
    if dst.group.trim().is_empty() {
        dst.group = src.group;
    }
}

/// 全表去重：删除所有字段完全一致的重复条目。
///
/// 真实页面里同一门课可能在多个位置被重复渲染（例如同一格里的隐藏版与
/// 可见版在合并后仍留下完全相同的记录，或跨行出现同样的条目）。
/// 只要「课程名 + 星期 + 节次 + 周次 + 教师 + 地点」全部相同，就认为
/// 是同一条课程，只保留一条 —— 否则会往系统日历里写入重复日程。
fn dedupe_global(entries: Vec<CourseEntry>) -> Vec<CourseEntry> {
    use std::collections::HashSet;

    let mut seen: HashSet<String> = HashSet::new();
    let mut out = Vec::with_capacity(entries.len());
    for e in entries {
        let weeks: Vec<String> = e
            .weeks
            .iter()
            .map(|w| format!("{}~{}@{:?}", w.start, w.end, w.week_type))
            .collect();
        let key = format!(
            "{}\u{1f}{}\u{1f}{}-{}\u{1f}{}\u{1f}{}\u{1f}{}",
            e.name.trim(),
            e.day,
            e.start_node,
            e.end_node,
            weeks.join(","),
            e.teacher.trim(),
            e.room.trim(),
        );
        if seen.insert(key) {
            out.push(e);
        }
    }
    out
}

/// 从 `3-18(周)[01-02-03节]` 这类文本里取出方括号内的节次范围。
fn parse_bracket_node(text: &str) -> Option<(i32, i32)> {
    let open = text.find('[')?;
    let close = text[open..].find(']')? + open;
    parse_node_range(&text[open + 1..close])
}

fn title_text(doc: &Html, titles: &[&str]) -> String {
    let Ok(sel) = Selector::parse("[title]") else {
        return String::new();
    };
    for el in doc.select(&sel) {
        let t = el.value().attr("title").unwrap_or("").trim();
        if titles.contains(&t) {
            return element_text(el).trim().to_string();
        }
    }
    String::new()
}

// ---------------------------------------------------------------------------
// 策略 B：按子节点出现顺序
// ---------------------------------------------------------------------------

/// 依据节点顺序解析，顺序为：课程名 / 教师 / 周次(节次) / 地点。
///
/// 与 `Qiangzhi_to_ics/get_kb.py` 的做法一致：跳过空节点与占位符，
/// 遇到第 3 个信息节点时它同时携带周次与节次。
fn parse_chunk_by_position(html: &str, day: i32, node: i32, raw: &str) -> Option<CourseEntry> {
    let frag = Html::parse_fragment(html);
    let chunk = frag.root_element();
    let parts = line_parts(chunk);

    if parts.is_empty() {
        return None;
    }

    let name = parts[0].clone();
    let mut teacher = String::new();
    let mut room = String::new();
    let mut weeks: Vec<WeekRange> = Vec::new();
    let mut start_node = node;
    let mut end_node = node;
    let mut nodes_explicit = false;

    for part in parts.iter().skip(1) {
        if part.contains('(') || part.contains('（') || part.contains('周') {
            if weeks.is_empty() {
                let (w, n) = split_week_and_node(part);
                weeks = parse_weeks(&w);
                if let Some((s, e)) = parse_node_range(&n) {
                    start_node = s;
                    end_node = e;
                    nodes_explicit = true;
                }
            }
            continue;
        }
        if part.contains("节") {
            if let Some((s, e)) = parse_node_range(part) {
                start_node = s;
                end_node = e;
                nodes_explicit = true;
            }
            continue;
        }
        if teacher.is_empty() {
            teacher = part.clone();
        } else if room.is_empty() {
            room = part.clone();
        }
    }

    if weeks.is_empty() {
        // 整块文本再试一次
        let whole = element_text(chunk);
        let (w, n) = split_week_and_node(&whole);
        weeks = parse_weeks(&w);
        if let Some((s, e)) = parse_node_range(&n) {
            start_node = s;
            end_node = e;
            nodes_explicit = true;
        }
    }
    if weeks.is_empty() {
        return None;
    }

    let entry = CourseEntry {
        name: name.trim().to_string(),
        teacher,
        room,
        group: String::new(),
        day,
        start_node,
        end_node,
        nodes_explicit,
        weeks,
        raw: raw.to_string(),
    };
    entry.is_valid().then_some(entry)
}

// ---------------------------------------------------------------------------
// 文本工具
// ---------------------------------------------------------------------------

/// 把一格课表内容按 `<br>` 切成若干「行」文本。
///
/// 强智的输出形如：
/// ```html
/// <font title="课程名称">高等数学</font><br>
/// <font title="老师">张教授</font><br>
/// <font title="周次(节次)">1-16(周)[01-02节]</font><br>
/// <font title="教室">教1-201</font>
/// ```
/// 即在每个信息之间插入 `<br>`。因此按 `<br>` 分行后，行的顺序就是
/// 课程名 / 教师 / 周次节次 / 地点 —— 这正是策略 B 依赖的顺序。
fn line_parts(chunk: ElementRef<'_>) -> Vec<String> {
    let mut lines: Vec<String> = Vec::new();
    let mut current: Vec<String> = Vec::new();
    for node_ref in chunk.descendants() {
        if let Some(el) = ElementRef::wrap(node_ref) {
            if el.value().name() == "br" {
                lines.push(clean_text(&current.join(" ")));
                current.clear();
            }
            continue; // 元素节点本身不贡献文本，文本在叶子节点上
        }
        if let Some(text) = node_ref.value().as_text() {
            current.push(text.to_string());
        }
    }
    lines.push(clean_text(&current.join(" ")));

    lines
        .into_iter()
        .filter(|l| !l.is_empty() && !is_placeholder(l))
        .collect()
}

fn is_placeholder(text: &str) -> bool {
    let t = text.trim();
    t == "-"
        || t == "--"
        || t == "&nbsp"
        || t == "\u{a0}"
        || t == "\u{a0}P"
        || t == "&nbspP"
        || t == "\u{a0}O"
        || t == "&nbspO"
}

fn clean_text(s: &str) -> String {
    s.replace('\u{a0}', " ")
        .replace("&nbsp;", " ")
        .replace("&nbsp", " ")
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .trim()
        .to_string()
}

fn truncate(s: &str, n: usize) -> String {
    let s = s.trim();
    if s.chars().count() <= n {
        return s.to_string();
    }
    s.chars().take(n).collect::<String>() + "…"
}

fn element_text(el: ElementRef<'_>) -> String {
    clean_text(&el.text().collect::<Vec<_>>().join(" "))
}

fn text_all(doc: &Html) -> String {
    clean_text(&doc.root_element().text().collect::<Vec<_>>().join(" "))
}

// ---------------------------------------------------------------------------
// 周次 / 节次字符串解析
// ---------------------------------------------------------------------------

/// 把 `1-16(周)[01-02节]`、`1-16周(1-2节)`、`1-4,6-8周` 等拆成 (周次部分, 节次部分)。
///
/// 返回的节次部分可能是空串。
pub fn split_week_and_node(text: &str) -> (String, String) {
    let s = text.trim();
    if s.is_empty() {
        return (String::new(), String::new());
    }

    // 情况 1：带方括号 —— 方括号里通常是节次
    if let Some(open) = s.find('[') {
        if let Some(close_rel) = s[open..].find(']') {
            let close = open + close_rel;
            let node = s[open + 1..close].to_string();
            let week = format!("{}{}", &s[..open], &s[close + 1..]);
            return (week.trim().to_string(), node.trim().to_string());
        }
    }

    // 情况 2：带圆括号 —— 括号里是单双周标记或节次
    if let Some(open) = s.find('(').or_else(|| s.find('（')) {
        let open_char = s[open..].chars().next().unwrap();
        let close_char = if open_char == '（' { '）' } else { ')' };
        if let Some(close_rel) = s[open..].find(close_char) {
            let close = open + close_rel;
            let inner = &s[open + open_char.len_utf8()..close];
            let after = &s[close + close_char.len_utf8()..];
            // 括号后还有内容（如 [01-02节]）→ 括号是周次标记
            if inner.contains('节') {
                let node = inner.to_string();
                let week = format!("{}{}", &s[..open], after);
                return (week.trim().to_string(), node.trim().to_string());
            }
            // 括号内是「周」或「单/双周」→ 周次标记，节次在别处
            if inner.contains('周') || inner.contains("单") || inner.contains("双") {
                return (s.to_string(), String::new());
            }
            // 其余情况：把括号内容当作节次候选
            return (s[..open].to_string(), inner.to_string());
        }
    }

    // 情况 3：无括号，可能是 `1-16周` 或 `1-2节`
    if s.contains('节') {
        return (String::new(), s.to_string());
    }
    (s.to_string(), String::new())
}

/// 解析周次字符串，例如 `1-16周`、`1-4,6-8周`、`3-5周(单)`、`1,3,5周`。
///
/// 会忽略字符串里混进来的节次括号内容。返回的区间按起始周排序并合并重叠部分。
pub fn parse_weeks(text: &str) -> Vec<WeekRange> {
    let mut ranges: Vec<WeekRange> = Vec::new();
    // 单双周标记对整段生效
    let global_type = if text.contains("单") {
        WeekType::Odd
    } else if text.contains("双") {
        WeekType::Even
    } else {
        WeekType::Every
    };

    for segment in text.split([',', '，', '、']) {
        // 去掉「周」「(周)」等修饰，只留数字与 -
        let seg = segment
            .replace('周', " ")
            .replace('(', " ")
            .replace(')', " ")
            .replace('（', " ")
            .replace('）', " ")
            .replace('[', " ")
            .replace(']', " ");
        // 去掉节次部分（含「节」的片段）
        let seg: String = seg
            .split_whitespace()
            .filter(|p| !p.contains('节'))
            .collect::<Vec<_>>()
            .join(" ");

        let nums = extract_ints(&seg);
        if nums.is_empty() {
            continue;
        }
        // 单双周标记也可能写在某一段里
        let local_type = if segment.contains("单") {
            WeekType::Odd
        } else if segment.contains("双") {
            WeekType::Even
        } else {
            global_type
        };

        match nums.len() {
            1 => ranges.push(WeekRange::new(nums[0], nums[0], local_type)),
            _ => {
                // 形如 1-16；若出现 1-16-18 这种异常，按相邻对处理
                for pair in nums.chunks(2) {
                    if pair.len() == 2 {
                        ranges.push(WeekRange::new(pair[0], pair[1], local_type));
                    } else {
                        ranges.push(WeekRange::new(pair[0], pair[0], local_type));
                    }
                }
            }
        }
    }

    // 过滤非法周次（<=0），北邮存在第 0 周，但课表周次从 1 开始
    ranges.retain(|r| r.start >= 1 && r.end >= 1);
    ranges.sort_by_key(|r| (r.start, r.end));
    ranges
}

/// 解析节次字符串，例如 `01-02节`、`1-2`、`[03-04节]`、`3节`、`09-10-11节`。
///
/// 注意三小节连排的写法：北邮把上午 3~5 节写成 `[03-04-05节]`，
/// `09-10-11节` 意思是**第 9 到第 11 节**（15:40–18:10），
/// 所以要取首尾两个数字（9 和 11），而不是前两个（9 和 10）。
pub fn parse_node_range(text: &str) -> Option<(i32, i32)> {
    let cleaned = text
        .replace('节', " ")
        .replace('[', " ")
        .replace(']', " ")
        .replace('(', " ")
        .replace(')', " ")
        .replace('（', " ")
        .replace('）', " ");
    let nums = extract_ints(&cleaned);
    match nums.len() {
        0 => None,
        1 => {
            if nums[0] >= 1 {
                Some((nums[0], nums[0]))
            } else {
                None
            }
        }
        _ => {
            let start = nums[0];
            let end = *nums.last().unwrap_or(&start);
            if start >= 1 && end >= start {
                Some((start, end))
            } else if start >= 1 {
                Some((start, start))
            } else {
                None
            }
        }
    }
}

/// 从字符串里提取所有整数（按出现顺序）。
fn extract_ints(s: &str) -> Vec<i32> {
    let mut out = Vec::new();
    let mut cur = String::new();
    for ch in s.chars() {
        if ch.is_ascii_digit() {
            cur.push(ch);
        } else if !cur.is_empty() {
            if let Ok(v) = cur.parse::<i32>() {
                out.push(v);
            }
            cur.clear();
        }
    }
    if !cur.is_empty() {
        if let Ok(v) = cur.parse::<i32>() {
            out.push(v);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_weeks_variants() {
        let r = parse_weeks("1-16周");
        assert_eq!(r.len(), 1);
        assert_eq!((r[0].start, r[0].end), (1, 16));
        assert_eq!(r[0].week_type, WeekType::Every);

        let r = parse_weeks("1-4,6-8,10周");
        assert_eq!(r.len(), 3);
        assert_eq!(r[0].expand(), vec![1, 2, 3, 4]);
        assert_eq!(r[2].expand(), vec![10]);

        let r = parse_weeks("3-5周(单)");
        assert_eq!(r[0].week_type, WeekType::Odd);
        assert_eq!(r[0].expand(), vec![3, 5]);

        let r = parse_weeks("2-8周(双)");
        assert_eq!(r[0].expand(), vec![2, 4, 6, 8]);
    }

    #[test]
    fn parses_node_ranges() {
        assert_eq!(parse_node_range("01-02节"), Some((1, 2)));
        assert_eq!(parse_node_range("[03-04节]"), Some((3, 4)));
        assert_eq!(parse_node_range("3节"), Some((3, 3)));
        assert_eq!(parse_node_range("1-2"), Some((1, 2)));
        assert_eq!(parse_node_range(""), None);
    }

    #[test]
    fn splits_week_and_node() {
        let (w, n) = split_week_and_node("1-16(周)[01-02节]");
        assert_eq!(n, "01-02节");
        assert!(w.contains("1-16"));

        let (w, n) = split_week_and_node("1-16周(1-2节)");
        assert_eq!(n, "1-2节");
        assert!(w.contains("1-16"));

        let (w, n) = split_week_and_node("1-16周");
        assert_eq!(n, "");
        assert!(w.contains("1-16"));
    }

    #[test]
    fn dash_split_respects_depth() {
        let html = r#"<font title="a">甲</font><br>--------------------<font title="b">乙</font>"#;
        let chunks = split_courses_by_dashes(html);
        assert_eq!(chunks.len(), 2);
        assert!(chunks[0].contains('甲'));
        assert!(chunks[1].contains('乙'));
        // 不应把闭合标签切坏
        assert!(!chunks[1].starts_with("</"));
    }

    #[test]
    fn dash_split_ignores_short_dashes() {
        let html = r#"<font>1-2节</font>"#;
        let chunks = split_courses_by_dashes(html);
        assert_eq!(chunks.len(), 1);
    }

    /// 用 title 属性构造的课表（策略 A）
    fn sample_by_title() -> String {
        r#"
<html><body>
<table id="kbtable">
  <tr><th></th><th>星期一</th><th>星期二</th><th>星期三</th><th>星期四</th><th>星期五</th><th>星期六</th><th>星期日</th></tr>
  <tr><td>1</td>
    <td>
      <div class="kbcontent" id="kbcontent_1-1">
        <font title="课程名称">高等数学</font><br>
        <font title="老师">张教授</font><br>
        <font title="周次(节次)">1-16(周)[01-02节]</font><br>
        <font title="教室">教1-201</font>
      </div>
    </td>
    <td>&nbsp;</td><td>&nbsp;</td>
    <td>
      <div class="kbcontent" id="kbcontent_4-1">
        <font title="课程名称">大学物理</font><br>
        <font title="老师">李教授</font><br>
        <font title="周次(节次)">3-8(周)[01-02节]</font><br>
        <font title="教室">教2-305</font>
      </div>
    </td>
    <td>&nbsp;</td><td>&nbsp;</td><td>&nbsp;</td>
  </tr>
  <tr><td>2</td>
    <td>
      <div class="kbcontent" id="kbcontent_1-3">
        <font title="课程名称">线性代数</font><br>
        <font title="老师">王教授</font><br>
        <font title="周次(节次)">1-8(周)[03-04节]</font><br>
        <font title="教室">教3-101</font>
      </div>
    </td>
    <td>&nbsp;</td><td>&nbsp;</td><td>&nbsp;</td><td>&nbsp;</td><td>&nbsp;</td><td>&nbsp;</td>
  </tr>
</table>
</body></html>"#
        .to_string()
    }

    #[test]
    fn parses_by_title_strategy() {
        let report = parse_timetable(&sample_by_title());
        assert!(report.found_table);
        assert_eq!(report.strategy, "title");
        assert_eq!(report.entries.len(), 3);

        let math = report
            .entries
            .iter()
            .find(|e| e.name == "高等数学")
            .expect("应解析出高等数学");
        assert_eq!(math.teacher, "张教授");
        assert_eq!(math.room, "教1-201");
        assert_eq!(math.day, 1);
        assert_eq!((math.start_node, math.end_node), (1, 2));
        assert_eq!(math.weeks[0].expand().len(), 16);

        let physics = report
            .entries
            .iter()
            .find(|e| e.name == "大学物理")
            .expect("应解析出大学物理");
        assert_eq!(physics.day, 4);
        assert_eq!(physics.weeks[0].expand(), vec![3, 4, 5, 6, 7, 8]);

        let la = report
            .entries
            .iter()
            .find(|e| e.name == "线性代数")
            .expect("应解析出线性代数");
        assert_eq!((la.start_node, la.end_node), (3, 4));
    }

    /// 真实的强智课表用 `font color="red"` 包住 `-----` 分隔符，
    /// 而且分隔符出现在 `font` **内部**（深度 1）。
    /// 这里还原这个形态，确保深度感知切分确实生效、不会把标签切坏。
    #[test]
    fn splits_multiple_courses_separated_by_red_dashes() {
        let html = r#"
<html><body>
<table id="kbtable">
  <tr><td>节次</td><td>星期一</td><td>星期二</td><td>星期三</td><td>星期四</td><td>星期五</td><td>星期六</td><td>星期日</td></tr>
  <tr><td>1</td>
    <td>
      <div class="kbcontent" id="kbcontent_1-1">
        <font title="课程名称">高等数学</font><br>
        <font title="老师">张教授</font><br>
        <font title="周次(节次)">1-8(周)[01-02节]</font><br>
        <font title="教室">教1-201</font>
        <font color="red">--------------------</font>
        <font title="课程名称">大学英语</font><br>
        <font title="老师">李老师</font><br>
        <font title="周次(节次)">9-16(周)[01-02节]</font><br>
        <font title="教室">教2-105</font>
      </div>
    </td>
    <td>&nbsp;</td><td>&nbsp;</td><td>&nbsp;</td><td>&nbsp;</td><td>&nbsp;</td><td>&nbsp;</td>
  </tr>
</table>
</body></html>"#;

        let report = parse_timetable(html);
        assert!(report.found_table);
        assert_eq!(report.entries.len(), 2, "一格两门课应拆成两条: {:?}", report.entries);

        let math = report.entries.iter().find(|e| e.name == "高等数学").unwrap();
        assert_eq!(math.weeks[0].expand(), vec![1, 2, 3, 4, 5, 6, 7, 8]);
        assert_eq!(math.room, "教1-201");

        let english = report.entries.iter().find(|e| e.name == "大学英语").unwrap();
        assert_eq!(english.weeks[0].expand(), vec![9, 10, 11, 12, 13, 14, 15, 16]);
        assert_eq!(english.room, "教2-105");
        assert_eq!(english.teacher, "李老师");
    }

    /// 同一格里同时存在隐藏的 `kbcontent` 与可见的 `kbcontent1` 时，
    /// 空的那些必须被跳过，不能产生空课程或重复课程。
    #[test]
    fn skips_empty_kbcontent_variants() {
        let html = r#"
<table id="kbtable">
  <tr><td>节次</td><td>星期一</td><td>星期二</td><td>星期三</td><td>星期四</td><td>星期五</td><td>星期六</td><td>星期日</td></tr>
  <tr><td>3</td>
    <td>
      <div class="kbcontent" id="kbcontent_1-3" style="display:none"></div>
      <div class="kbcontent1" id="kbcontent_1-3">
        <font title="课程名称">数据结构</font><br>
        <font title="老师">王教授</font><br>
        <font title="周次(节次)">1-16(周)[03-04节]</font><br>
        <font title="教室">教3-302</font>
      </div>
    </td>
    <td>&nbsp;</td><td>&nbsp;</td><td>&nbsp;</td><td>&nbsp;</td><td>&nbsp;</td><td>&nbsp;</td>
  </tr>
</table>"#;
        let report = parse_timetable(html);
        assert_eq!(report.entries.len(), 1, "只应解析出一条: {:?}", report.entries);
        assert_eq!(report.entries[0].name, "数据结构");
        assert_eq!((report.entries[0].start_node, report.entries[0].end_node), (3, 4));
    }

    /// title 属性缺失时必须退回按节点顺序解析（策略 B）。
    #[test]
    fn falls_back_to_position_strategy_without_titles() {
        let html = r#"
<table id="kbtable">
  <tr><td>节次</td><td>星期一</td><td>星期二</td><td>星期三</td><td>星期四</td><td>星期五</td><td>星期六</td><td>星期日</td></tr>
  <tr><td>1</td>
    <td>
      <div class="kbcontent" id="kbcontent_1-1">
        概率论与数理统计<br>赵教授<br>1-16(周)[01-02节]<br>教4-101
      </div>
    </td>
    <td>&nbsp;</td><td>&nbsp;</td><td>&nbsp;</td><td>&nbsp;</td><td>&nbsp;</td><td>&nbsp;</td>
  </tr>
</table>"#;
        let report = parse_timetable(html);
        assert_eq!(report.strategy, "position", "应走位置策略");
        assert_eq!(report.entries.len(), 1);
        let e = &report.entries[0];
        assert_eq!(e.name, "概率论与数理统计");
        assert_eq!(e.teacher, "赵教授");
        assert_eq!(e.room, "教4-101");
        assert_eq!(e.day, 1);
        assert_eq!((e.start_node, e.end_node), (1, 2));
        assert_eq!(e.weeks[0].expand().len(), 16);
    }

    /// 页面里没有课表时应给出明确告警（App 用它提示用户「先打开课表页」）。
    #[test]
    fn reports_missing_table() {
        let report = parse_timetable("<html><body><h1>登录</h1></body></html>");
        assert!(!report.found_table);
        assert!(report.entries.is_empty());
        assert!(!report.warnings.is_empty());
    }

    /// 强智的「第 0 周」以及单双周写法。
    #[test]
    fn parses_zero_week_and_parity_variants() {
        for (text, expected_first) in [
            ("1-5周", 1),
            ("1-5周(单)", 1),
            ("2-6周(双)", 2),
        ] {
            let r = parse_weeks(text);
            assert!(!r.is_empty(), "{text} 应能解析出周次");
            assert_eq!(r[0].start, expected_first, "{text} 起始周不对");
        }
        // 「第 0 周」这种写法在周次里不出现，但换算函数必须支持
        assert_eq!(parse_weeks("0-2周").len(), 0, "周次从 1 开始，0 应被丢弃");
    }
}

#[cfg(test)]
mod real_data_tests {
    use super::*;
    use crate::term::{bell_schedule, build_events, parse_date, SCHEDULE_MAINLAND};

    /// 三小节连排必须取首尾：`[09-10-11节]` = 第 9 到第 11 节。
    #[test]
    fn three_period_ranges_take_first_and_last() {
        assert_eq!(parse_node_range("09-10-11节"), Some((9, 11)));
        assert_eq!(parse_node_range("[01-02-03节]"), Some((1, 3)));
        assert_eq!(parse_node_range("[03-04-05节]"), Some((3, 5)));
        // 普通两小节写法不受影响
        assert_eq!(parse_node_range("[01-02节]"), Some((1, 2)));
    }

    /// 如果本地放了真实课表页（`fixtures/real_*.html`，**不进版本库**），
    /// 就用它做一次宽松但有力的回归。
    ///
    /// 这里刻意**不写死任何课程名、教师、教室和条目数量** —— 换学期、换人、
    /// 页面小改都不会误报，也不会把个人课表信息写进源码。
    /// 真实页面里那些**具体结构**的强断言放在用虚构数据构造的
    /// `sample_timetable_quirks.html` 上（见 `tests/real_page_parsing.rs`）。
    ///
    /// 最关键的一条断言是「**星期几的集合**必须和页面里课程 id 声明的星期集合一致」，
    /// 它能抓住「整列星期错位」这类最难发现的错误。
    #[test]
    fn parses_local_real_page_if_present() {
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures");
        let Ok(entries) = std::fs::read_dir(&dir) else {
            return;
        };
        // 找第一个 real_*.html
        let mut target: Option<std::path::PathBuf> = None;
        for item in entries.flatten() {
            let p = item.path();
            let name = p.file_name().unwrap_or_default().to_string_lossy().to_string();
            if name.starts_with("real_") && name.ends_with(".html") {
                target = Some(p);
                break;
            }
        }
        let Some(path) = target else {
            eprintln!("跳过：{} 下没有真实课表样本（属于个人数据，不进版本库）", dir.display());
            return;
        };

        let Ok(html) = std::fs::read_to_string(&path) else {
            return;
        };
        let report = parse_timetable(&html);

        assert!(report.found_table, "真实页面里应能找到课表表格");
        assert!(
            !report.entries.is_empty(),
            "应能解析出课程。告警: {:?}",
            report.warnings
        );

        // ---- 字段合理性 ----
        for e in &report.entries {
            assert!(!e.name.trim().is_empty(), "有空课程名");
            assert!(
                (1..=7).contains(&e.day),
                "星期越界: day={} name={:?}",
                e.day,
                e.name
            );
            assert!(
                e.start_node >= 1 && e.end_node >= e.start_node && e.end_node <= 20,
                "节次越界: {}-{} name={:?}",
                e.start_node,
                e.end_node,
                e.name
            );
            assert!(!e.weeks.is_empty(), "没有解析出周次: {:?}", e.name);
            for w in &e.weeks {
                assert!(
                    w.start >= 1 && w.end <= 30 && w.start <= w.end,
                    "周次区间不合理: {}-{} name={:?}",
                    w.start,
                    w.end,
                    e.name
                );
            }
        }

        // ---- 不能出现完全重复的条目 ----
        for (i, a) in report.entries.iter().enumerate() {
            for b in report.entries.iter().skip(i + 1) {
                assert!(
                    !(a.name == b.name
                        && a.day == b.day
                        && a.start_node == b.start_node
                        && a.end_node == b.end_node
                        && a.weeks == b.weeks),
                    "出现重复条目: {a:?} 与 {b:?}"
                );
            }
        }

        // ---- 最强的一条：星期几的集合要与页面里课程 id 声明的星期集合一致 ----
        // 整列错位（例如全部 -1 天）会让集合变化，从而被这条断言抓住。
        let doc = Html::parse_document(&html);
        let mut days_in_ids: std::collections::BTreeSet<i32> = std::collections::BTreeSet::new();
        if let Ok(sel) = Selector::parse("div.kbcontent, div.kbcontent1") {
            for div in doc.select(&sel) {
                if element_content_text(div).trim().is_empty() {
                    continue; // 空骨架 div 不参与
                }
                if let Some(d) = day_from_kbcontent_id(div.value().attr("id").unwrap_or("")) {
                    days_in_ids.insert(d);
                }
            }
        }
        let days_in_entries: std::collections::BTreeSet<i32> =
            report.entries.iter().map(|e| e.day).collect();

        if !days_in_ids.is_empty() {
            assert_eq!(
                days_in_entries, days_in_ids,
                "星期几的集合对不上：解析结果是 {days_in_entries:?}，\
                 而页面里课程 id 声明的是 {days_in_ids:?} —— 很可能整列错位了"
            );
        }
    }

    /// 这份真实页面的课程展开后应得到合理的日程条数。
    #[test]
    fn real_page_expands_to_reasonable_events() {
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures");
        let Ok(rd) = std::fs::read_dir(&dir) else {
            return;
        };
        let mut target: Option<std::path::PathBuf> = None;
        for item in rd.flatten() {
            let p = item.path();
            let name = p.file_name().unwrap_or_default().to_string_lossy().to_string();
            if name.starts_with("real_") && name.ends_with(".html") {
                target = Some(p);
                break;
            }
        }
        let Some(path) = target else {
            return;
        };
        let Ok(html) = std::fs::read_to_string(&path) else {
            return;
        };
        let report = parse_timetable(&html);
        if report.entries.is_empty() {
            return;
        }

        let term_start = parse_date("2026-03-02").unwrap();
        let schedule = bell_schedule(crate::term::SCHEDULE_MAINLAND);
        let events = build_events(&report.entries, term_start, &schedule, None);
        assert!(!events.is_empty(), "应能展开出日程");

        // 不写死条数：只要求每个条目都产出了日程（说明节次/周次都有效）
        assert!(
            events.len() >= report.entries.len(),
            "展开出的日程({}) 少于课程条目数({})",
            events.len(),
            report.entries.len()
        );

        for ev in &events {
            // 时间不能倒挂
            assert!(ev.start < ev.end, "时间倒挂: {ev:?}");
            assert!(ev.end_millis > ev.start_millis);

            // 最强的一条：算出的日期，星期几必须和课程声明的星期一致
            let entry = report
                .entries
                .get(ev.source_index)
                .unwrap_or_else(|| panic!("source_index 越界: {}", ev.source_index));
            let date = parse_date(&ev.start[..10]).unwrap();
            use chrono::Datelike;
            let actual = date.weekday().num_days_from_monday() as i32 + 1;
            assert_eq!(
                actual, entry.day,
                "错位！课程 {:?} 在课表里是周{}，但 {:?} 是周{}",
                entry.name, entry.day, ev.start, actual
            );

            // 开始时间必须落在北邮官方作息表的某一小节上
            let hhmm = &ev.start[11..16];
            let valid = [
                "08:00", "08:50", "09:50", "10:40", "11:30", "13:00", "13:50",
                "14:45", "15:40", "16:35", "17:25", "18:30", "19:20", "20:10",
            ];
            assert!(
                valid.contains(&hhmm),
                "开始时间不在作息表里: {hhmm} ({:?})",
                entry.name
            );
        }
    }
}

#[cfg(test)]
mod multi_document_tests {
    use super::*;

    /// 主页面（没有课表，只有一个框架占位）
    fn main_page() -> String {
        r#"<html><head><title>教务系统</title></head><body>
        <div id="menu"><a href="xskb_list.do" target="_blank">我的课表</a></div>
        <iframe id="mainFrame" src="xskb_list.do"></iframe>
        </body></html>"#
            .to_string()
    }

    /// 框架里的内容（真正的课表）
    fn frame_with_timetable() -> String {
        r#"<html><body>
        <table id="kbtable">
          <tr><td>节次</td><td>星期一</td><td>星期二</td><td>星期三</td><td>星期四</td><td>星期五</td><td>星期六</td><td>星期日</td></tr>
          <tr><td>1</td>
            <td><div class="kbcontent" id="a-1-1">
              <font title="课程名称">高等数学</font><br>
              <font title="老师">某某老师</font><br>
              <font title="周次(节次)">1-16(周)[01-02节]</font><br>
              <font title="教室">教1-101</font></div></td>
            <td>&nbsp;</td><td>&nbsp;</td><td>&nbsp;</td><td>&nbsp;</td><td>&nbsp;</td><td>&nbsp;</td>
          </tr>
        </table>
        </body></html>"#
            .to_string()
    }

    /// 这就是用户遇到的真实情形：课表在 iframe 里。
    /// 只看主文档必须找不到；主文档 + 框架一起给才能解析出来。
    #[test]
    fn timetable_inside_iframe_is_found_when_frames_are_included() {
        // 只给主文档 —— 找不到（正是导入失败时的表现）
        let only_main = parse_documents(&[main_page()]);
        assert!(
            !only_main.found_table,
            "主文档里没有课表，不应报告「找到了表格」"
        );
        assert!(only_main.entries.is_empty());
        assert!(!only_main.warnings.is_empty(), "应给出提示");

        // 主文档 + 框架 —— 找到
        let with_frame = parse_documents(&[main_page(), frame_with_timetable()]);
        assert!(with_frame.found_table, "带上框架后应能找到课表");
        assert_eq!(with_frame.entries.len(), 1);
        assert_eq!(with_frame.entries[0].name, "高等数学");
        assert_eq!(with_frame.entries[0].teacher, "某某老师");
        assert_eq!(with_frame.entries[0].room, "教1-101");
        assert_eq!(with_frame.entries[0].day, 1);
    }

    /// 主文档和框架里都有同一张课表时，不能出现重复课程。
    #[test]
    fn duplicate_content_across_documents_is_deduped() {
        // 模拟某些页面把课表同时渲染在主文档和框架里
        let both = parse_documents(&[frame_with_timetable(), frame_with_timetable()]);
        assert_eq!(both.entries.len(), 1, "跨文档重复内容应被去重");
    }

    /// 空文档 / 非法输入不能 panic。
    #[test]
    fn empty_and_blank_documents_are_safe() {
        let r = parse_documents(&[]);
        assert!(!r.found_table);
        assert!(r.entries.is_empty());

        let r = parse_documents(&["".to_string(), "   ".to_string()]);
        assert!(r.entries.is_empty());

        // 只有乱码也不是课表
        let r = parse_documents(&["not html at all".to_string()]);
        assert!(!r.found_table);
    }

    /// JSON 入口（Kotlin 侧走的就是这个）也要能吃数组。
    #[test]
    fn json_documents_entry_point_merges() {
        let docs = serde_json::to_string(&vec![main_page(), frame_with_timetable()]).unwrap();
        let out = crate::api::parse_html_documents_json(&docs);
        let v: serde_json::Value = serde_json::from_str(&out).unwrap();
        assert_eq!(v["entries"].as_array().unwrap().len(), 1);
        assert!(v["report"]["found_table"].as_bool().unwrap());
        assert_eq!(v["entries"][0]["name"], "高等数学");

        // 非法 JSON 不能崩
        let out = crate::api::parse_html_documents_json("这不是JSON");
        let v: serde_json::Value = serde_json::from_str(&out).unwrap();
        assert!(v["entries"].as_array().unwrap().is_empty());
    }
}

#[cfg(test)]
mod column_alignment_tests {
    use super::*;

    /// 数据行的「节次标签」是 `<th>`（而不是 `<td>`）。
    ///
    /// 旧实现只收集 `<td>`，会把这一列丢掉，整行向左错位一格：
    /// 周一课消失、周二课变周一、周三课变周二。
    /// 这正是用户报告的错位症状，必须锁死。
    #[test]
    fn th_node_labels_do_not_shift_days() {
        let html = r#"
<table id="kbtable">
  <tr><th></th><th>星期一</th><th>星期二</th><th>星期三</th><th>星期四</th><th>星期五</th><th>星期六</th><th>星期日</th></tr>
  <tr><th>1</th>
    <td><div class="kbcontent" id="A-1-1">周一课<br><font title="周次(节次)">1-16(周)[01-02节]</font></div></td>
    <td><div class="kbcontent" id="A-2-1">周二课<br><font title="周次(节次)">1-16(周)[01-02节]</font></div></td>
    <td><div class="kbcontent" id="A-3-1">周三课<br><font title="周次(节次)">1-16(周)[01-02节]</font></div></td>
    <td>&nbsp;</td><td>&nbsp;</td><td>&nbsp;</td><td>&nbsp;</td>
  </tr>
</table>"#;
        let report = parse_timetable(html);
        assert_eq!(report.entries.len(), 3, "{:#?}", report.entries);

        let monday = report.entries.iter().find(|e| e.name == "周一课").unwrap();
        assert_eq!(monday.day, 1, "周一的课不能被推到别处");
        let tuesday = report.entries.iter().find(|e| e.name == "周二课").unwrap();
        assert_eq!(tuesday.day, 2);
        let wednesday = report.entries.iter().find(|e| e.name == "周三课").unwrap();
        assert_eq!(wednesday.day, 3);
    }

    /// 数据行比表头少一列（最左的节次标签列缺失）时也不能错位。
    #[test]
    fn missing_node_label_column_does_not_shift_days() {
        let html = r#"
<table id="kbtable">
  <tr><td></td><td>星期一</td><td>星期二</td><td>星期三</td><td>星期四</td><td>星期五</td><td>星期六</td><td>星期日</td></tr>
  <tr>
    <td><div class="kbcontent" id="B-1-1">周一课<br><font title="周次(节次)">1-16(周)[01-02节]</font></div></td>
    <td><div class="kbcontent" id="B-2-1">周二课<br><font title="周次(节次)">1-16(周)[01-02节]</font></div></td>
    <td>&nbsp;</td><td>&nbsp;</td><td>&nbsp;</td><td>&nbsp;</td><td>&nbsp;</td>
  </tr>
</table>"#;
        let report = parse_timetable(html);
        assert_eq!(report.entries.len(), 2, "{:#?}", report.entries);
        let monday = report.entries.iter().find(|e| e.name == "周一课").unwrap();
        assert_eq!(monday.day, 1);
        let tuesday = report.entries.iter().find(|e| e.name == "周二课").unwrap();
        assert_eq!(tuesday.day, 2);
    }

    /// div 的 id 是星期几最可靠的来源（新、旧两种格式都要认）。
    #[test]
    fn day_from_id_supports_both_formats() {
        // 新格式：<32hex>-<星期>-<类型>
        assert_eq!(day_from_kbcontent_id("C63D887128C64AD79EFD17454B91A0DB-2-2"), Some(2));
        assert_eq!(day_from_kbcontent_id("C63D887128C64AD79EFD17454B91A0DB-7-1"), Some(7));
        // 旧格式：kbcontent_<星期>-<大节>
        assert_eq!(day_from_kbcontent_id("kbcontent_1-3"), Some(1));
        assert_eq!(day_from_kbcontent_id("kbcontent1_5-2"), Some(5));
        // 无效 id
        assert_eq!(day_from_kbcontent_id(""), None);
        assert_eq!(day_from_kbcontent_id("kbcontent"), None);
    }
}
