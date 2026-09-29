# 北邮课表导入（BUPT Calendar）

把北邮新教务系统（强智 jsxsd）的课表一键导入 Android 系统日历，并且**只清除自己添加的日程**。

架构：**Kotlin 薄壳 + Rust 核心**。爬取、解析、周次换算、日程生成、导入记录全部用 Rust 写；Kotlin 只负责 WebView、悬浮按钮、系统日历读写这三件必须在 Android 侧做的事。

---

## 目录结构

```
bupt-calendar/
├── core/                     # Rust 核心（可独立编译、独立测试，不需要 Android）
│   ├── src/
│   │   ├── models.rs         # 数据模型：课表条目 / 周次区间 / 作息表 / 日历日程
│   │   ├── parser.rs         # 强智 #kbtable 解析（双策略 + 容错）
│   │   ├── term.rs           # 北邮官方 14 节作息表 + 周次→日期换算 + 日程展开
│   │   ├── store.rs          # 导入记录（SQLite）：去重 + 「只删自己加的」依据
│   │   ├── api.rs            # 给 Kotlin 的高层门面（纯 JSON，可单测）
│   │   └── jni_bridge.rs     # JNI 导出层（只在 Android 目标下编译）
│   ├── build-android.sh      # 交叉编译到 Android（可脱离 Gradle 单独跑）
│   ├── examples/parse_html.rs# 离线校验工具：拿真实页面 HTML 验证解析
│   └── tests/
│       ├── real_page_parsing.rs  # 数据驱动的真实页面回归测试
│       └── fixtures/             # 把真实课表 HTML 丢这里就会被自动校验
└── app/                      # Android 应用
    └── src/main/java/com/bupt/calendar/
        ├── core/BuptCore.kt      # JNI 门面 + 数据类（字段与 Rust serde 对齐）
        ├── web/TimetableScraperJs.kt  # 注入 WebView 的抓取脚本
        ├── calendar/CalendarRepository.kt  # 系统日历读写
        └── ui/MainActivity.kt    # WebView + 悬浮导入按钮 + 全流程
```

---

## 工作原理

### 1. 抓取

内置 WebView 打开教务系统（校园网直连或 WebVPN），用户正常登录、翻到课表页，然后点右下角一直悬浮的导入按钮。

**User-Agent 伪装成桌面版。** 做法与参考实现
[`WakeupSchedule_BUPT`](https://github.com/xianfei/WakeupSchedule_BUPT) 一致：不换整条 UA，
而是把里面的 `Mobile` 改成 `eliboM`、`Android` 改成 `diordnA`（反向拼写），
让教务系统认不出这是手机浏览器，直接返回**桌面版页面**。
这样能避开手机端那套「点课表就弹新窗口」的跳转 —— 那条路径会让课表跑到
我们抓不到的窗口里，是导入失败最常见的原因。

按钮会注入一段 JS：

1. **在「主文档 + 所有可访问的内嵌框架（iframe / frame）」里**找出所有「像课表」的表格
   （含 `[title*="周次"]` 的单元格）；
   *课表经常显示在 iframe 里*，只搜主文档会出现「人明明在课表页上却找不到课表」；
2. 如果课表是**折叠**状态，自动点「展开/显示全部」并移除 `display:none`，轮询等待；
3. 把最像个人课表的那张表的 `outerHTML`，**连同各层框架的完整 HTML** 一起交回 Android 侧。

原生侧把「主页面 + 各框架」这些文档一并交给 Rust 解析，Rust 会合并结果并跨文档去重
（`parse_documents`）。

页面来源被限制在北邮域名（`*.bupt.edu.cn`）下，避免在别的站点误点按钮就把内容当课表解析。

> 关于「新窗口」：本项目**刻意不覆盖** `WebChromeClient.onCreateWindow`、
> 也**刻意不设置** `setSupportMultipleWindows` —— 保持 WebView 默认行为即可
> （默认情况下 `window.open` / `target=_blank` 会落到当前 WebView 里加载）。
> 这是照搬参考实现的做法：它的 `webViewClient` / `webChromeClient` 都是空壳，
> 真正管用的是上面两点（桌面 UA + 搜索框架）。
> 另外还会把页面里带 `target="_blank"` 的链接改写成当前窗口打开，作为兜底。

### 2. 解析（Rust）

强智课表的结构（`table#kbtable`，行=大节，列=星期），以**北邮真实页面**为准：

```html
<td>
  <!-- 隐藏的摘要版：有课程名和周次，但没有教师和节次 -->
  <div class="kbcontent1" style="display: none;">课程名<br>
    <font title="周次(节次)">3-18(周)</font><br>
    <font title="教室">某教室</font><br></div>
  <!-- 可见的详情版：字段齐全 -->
  <div class="kbcontent">课程名<br>
    <font title="老师">某老师</font><br>
    <font title="周次(节次)">3-18(周)[01-02-03节]</font><br>
    <font title="教室">某教室</font><br></div>
  <!-- 另外还有两个空的骨架模板 div（class="... sykb1/sykb2"），id 与上面完全相同 -->
</td>
```

解析器实现了**两条独立通道**并交叉验证：

* **策略 A（优先）**：读 `title` 属性取字段（`周次(节次)` / `老师` / `教室`）；
* **策略 B（兜底）**：`title` 缺失时按 `<br>` 分行、按节点顺序取
  「课程名 / 教师 / 周次节次 / 地点」。

处理了这些从真实页面里发现的坑：

| 情况 | 处理方式 |
|---|---|
| 课程名是 `div` 的**直接文本**，没有 `title="课程名称"` | 只取第一个 `<br>` 之前的直接文本子节点（不能用整块 `text()`，那样会把老师粘进课程名） |
| **星期几从哪来** | **优先读课程 div 的 `id`**（强智官方结构：`<教学班id>-<星期>-<类型>`，如 `…-2-2` = 星期二），再退到表头映射。id 与列数无关，是防错位的根本手段 |
| 同一门课拆成「隐藏摘要版 + 可见详情版」两个 div | **按单元格聚合**后再合并：节次取有节次的那条，周次取并集，教师/教室取非空的那份 |
| 页面自带的**空骨架模板** div（`class="... sykb1/sykb2"`，id 与真实课程重复） | 按 class 前缀 `sykb` 识别并跳过，否则会被当成「同名的另一门课」 |
| 同一门课横跨多个大节（周一 1-2 节与 3-4 节） | 用「课程名 + 节次」当锚点去重，不同节次不合并 |
| 三小节连排：`[01-02-03节]`、`[09-10-11节]` | 意思是**第 1 到第 3 节**，取首尾数字（9→11 即 15:40–18:10），不是取前两个 |
| 多段周次：`3,5-18周` | 拆成多个区间 |
| 单双周：`3-5周(单)`、`2-8周(双)` | 解析成 `WeekType::Odd/Even` |
| 一格多门课用 `-----` 分隔（部分学校版本） | 识别**连续 ≥4 个 `-`** 切分，不误伤 `1-2节`、`3-5周` |
| 每行开头有「节次标签格」 | 用**表头**建立「列→星期」映射；数据行与表头列数不一致时**不瞎猜**，宁缺勿错 |
| 全表重复渲染的相同条目 | 按「课程名+星期+节次+周次+教师+地点」全表去重 |
| 北邮有**第 0 周** | `date_of()` 对 `week = 0` 正确工作 |

> ⚠️ **列对齐的血案**：早期版本在「数据行列数与表头不一致」时按列号猜星期几，
> 会把整行往左错一位——周一课消失、周二课变周一、周三课与周二重叠。
> 真机用户报告过这个症状。修复方法是上面的「id 优先」，并有专门的回归测试
> （`parser.rs` 里的 `column_alignment_tests`）。

### 3. 周次 → 具体日期

作息表来自学校官方教学日历（[jwbs.bupt.edu.cn/info/1007/1264.htm](https://jwbs.bupt.edu.cn/info/1007/1264.htm)），一天 **14 小节**：

| | 时间 | | 时间 |
|---|---|---|---|
| 1 | 08:00-08:45 | 8 | 14:45-15:30 |
| 2 | 08:50-09:35 | 9 | 15:40-16:25 |
| 3 | 09:50-10:35 | 10 | 16:35-17:20 |
| 4 | 10:40-11:25 | 11 | 17:25-18:10 |
| 5 | 11:30-12:15 | 12 | 18:30-19:15 |
| 6 | 13:00-13:45 | 13 | 19:20-20:05 |
| 7 | 13:50-14:35 | 14 | 20:10-20:55 |

换算公式：`日期 = 第1周周一 + (周次-1)×7 + (星期-1)`。

「第 1 周周一」优先由页面上的「第 N 周」反推（`本周一 − (N−1)×7`），拿不到就用内置建议值，**最终一定弹日期选择器让用户确认**——差一周整份课表就全错位了。

### 4. 写入系统日历

每条日程用 Rust 算好的 **epoch 毫秒**直接写入，不在 Kotlin 侧重新解析时间字符串（避免两边对时区/格式的理解不一致）。

### 5. 「只清除本 App 添加的日程」是怎么做到的

系统日历是公共数据库，事件表里**没有「哪个 App 写的」这一列**，无法按来源筛选。所以归属判断不依赖系统日历，而是依赖 Rust 侧那份记录：

1. 导入时每写一条事件，把系统返回的 `_ID` 连同业务**指纹**一起记进本地 SQLite；
2. 清除时遍历记录，按 `_ID` 精确删除；
3. 只有 `_ID` 失效（用户手动删过）时才退回用「标题 + 开始时间」匹配。

由此得到两个特性：

* **不会误删**用户自己建的日程，也不会残留垃圾；
* **重复导入是幂等的**——指纹相同的日程只会写入一次；课表有变化时先「清空」再导入即可。

---

## 构建

### 环境要求

| 组件 | 说明 |
|---|---|
| Rust | 需要 `aarch64-linux-android` 等目标（`rustup target add ...`） |
| Android SDK | `platforms;android-35`、`build-tools;35.0.0`、`platform-tools` |
| Android NDK | r27（`ndk;27.3.13750724`） |
| JDK | 17 或以上 |
| Gradle | 8.11.x |

本机（Arch Linux 沙箱）因为 `$HOME` 不可写，工具链都装在项目同级目录：

```
<工作区>/.rustup          RUSTUP_HOME
<工作区>/.cargo_home      CARGO_HOME
<工作区>/android-sdk      ANDROID_HOME
<工作区>/.gradle          GRADLE_USER_HOME
```

`app/build.gradle.kts` 会自动从 `local.properties` 的 `sdk.dir` 或 `ANDROID_HOME` 找 NDK；`core/build-android.sh` 同样支持这两种来源，所以换机器只要改 `local.properties` 即可。

### 命令

```bash
# 检查注入 WebView 的 JS（语法 + 关键逻辑）+ 跑 Rust 全部测试
./build.sh test

# 构建 debug APK
./build.sh

# 构建 release APK
./build.sh release

# 清理中间产物（APK/ 里的安装包会保留）
./build.sh clean
```

只跑 Rust 测试（不检查 JS）：

```bash
cd core && cargo test
```

测试分三层：

| 文件 | 覆盖内容 |
|---|---|
| `core/src/**` 里的 38 个单元测试 | 解析器、周次换算、作息表、记录库、JNI 门面各自的行为 |
| `core/tests/app_flow.rs`（4 个） | **端到端流程**：模拟 App 真实的调用顺序跑「导入 → 去重 → 清空 → 重导」 |
| `core/tests/real_page_parsing.rs`（3 个） | 遍历 `fixtures/` 下所有真实页面做校验 |
| `tools/check-scraper-js.mjs`（13 项） | 从 Kotlin 源码抽出真实 JS，做语法解析与逻辑断言 |

产物在 `app/build/outputs/apk/`，并会自动同步一份到项目根部的 `APK/`。

### 网络慢怎么办

* crates.io 直连很慢：`core/.cargo/config.toml` 已配好 **USTC 镜像**。
* Maven 依赖：`settings.gradle.kts` 已优先使用**阿里云镜像**（Kotlin 的部分构件在 Central 上会被重定向到 GitHub Releases，国内直连经常超时）。

---

## 用真实页面校准解析器

解析器是**用真实北邮课表页逐项校准**出来的，真实页面里那些难缠的结构
（骨架模板 div、隐藏/可见配对、课程 id 里的星期段、三小节连排、多段周次……）
都在 [`core/tests/fixtures/sample_timetable_quirks.html`](core/tests/fixtures/sample_timetable_quirks.html)
里用**虚构数据**复刻了一份，`cargo test` 会对它做强断言。

> 真实的课表页面**没有放进版本库**：它含教学班号等信息，能关联到具体班级和个人。
> 需要本地校验时按下文自己导出一份即可（已在 `.gitignore` 里）。

如果你换了学期/校区，页面细节可能有差异，校准流程不需要手机：

```bash
cd core

# 1. 把教务页面另存为（或复制源码）到 fixtures
cp ~/Downloads/xskb_list.html tests/fixtures/mine.html

# 2. 直接看解析结果（--debug 会打印每条记录的原始 HTML，便于排查字段错位）
cargo run --example parse_html -- tests/fixtures/mine.html
cargo run --example parse_html -- tests/fixtures/mine.html --debug

# 想看展开后的日程、并导出 .ics 用手机导入验证
cargo run --example parse_html -- tests/fixtures/mine.html 2026-09-14 ics

# 3. 跑回归测试（会自动校验 fixtures 里所有 HTML）
cargo test
```

`cargo run --example parse_html` 会输出每门课的星期、节次、周次、教师、地点，
以及**每天条目数**——如果星期整体错位或某列被漏掉，一眼就能看出来。

---

## 怎么确定「第 1 周周一」

**App 里只需要你填「今天是第几周」**（这个数字教务页面上通常直接写着，
例如「2026-2027-1 第3周」），日期由程序倒推：`第1周周一 = 本周一 − (N−1)×7`。
不用自己去算日期 —— 少一个容易填错的东西，就少一处错位来源。
这一步是参考 [WakeupSchedule](https://github.com/xianfei/WakeupSchedule_BUPT) 的做法。

点导入后会弹出一个确认框，显示：

* 今天所在的一周（第 N 周，本周一 X 月 X 日）
* 由此倒推的「第 1 周周一」
* **推算结果核对表**：前几门课各自会落在哪个日期、星期几

只要核对表里的**星期几和你课表上的一致**，就说明没有错位。不对就点「改周次」。
也可以点「直接按日期选」用日历自己指定。

### 关于星期换算的两个坑

这一块特别容易错，处理时踩过两个坑，都记在这里：

1. **Java 的 `Calendar.DAY_OF_WEEK` 把周日当成 1**
   （`SUNDAY=1, MONDAY=2, …, SATURDAY=7`），而北邮的课表周次以**周一**为起点。
   早期用 `(DAY_OF_WEEK + 5) % 7` 反推本周一，**在周日会算成回退 6 天（上周一）**，
   整份课表差一周。现在按「距离周一的偏移」直接算，口径统一为
   「周一是一周第一天、周日是最后一天」。
2. **系统日历 App 可能把周日显示为一周第一天**（取决于语言/地区设置）。
   这只是**显示**习惯，不影响事件本身的日期。判断有没有错位要看
   **事件的星期几**，而不是它在日历网格里排第几列。

数据库层面（`term.rs`）的换算是：`日期 = 第1周周一 + (周次−1)×7 + (星期−1)`，
其中星期 `1=周一 … 7=周日`，并且支持北邮的**第 0 周**（第 1 周周一的前一周）。
`tests/app_flow.rs` 里有逐条比对「星期几 + 周次」的强校验测试。

### 校历参考

北邮官方教学日历（[jwbs.bupt.edu.cn/info/1007/1264.htm](https://jwbs.bupt.edu.cn/info/1007/1264.htm)）
给出每学期每周的起止日期。经验规律：

* 秋季学期：第 1 周周一 ≈ 9 月的第 2 个周一（如 2025 秋季是 09-08）
* 春季学期：第 1 周周一 ≈ 3 月的第 1 个周一（如 2026 春季是 03-02）

---

## 已知限制

* **海南校区作息表未确认**：官方教学日历脚注说明「海南校区教学节次按照海南试验区教学节次执行」，
  但未取得该套时间。目前海南校区与本部共用同一张作息表（见 `term.rs` 里的 TODO）。
* **尚未在真机 + 真实页面跑过完整流程**：解析逻辑已用真实页面离线验证，
  但 WebView 抓取 → 写系统日历 → 一键清空这条链路还需要在手机上实测。
* **未使用移动教务 JSON 接口**：`https://jwglweixin.bupt.edu.cn/bjyddx` 的 `/student/curriculum`
  实测校外可达，返回结构化 JSON，理论上比爬 HTML 更稳。但它需要把学号密码交给 App
  做 AES 加密登录，出于凭证安全考虑没有采用。如果后续想加「自动登录导入」，这是现成的路径。
* **`jwgl.bupt.edu.cn` 校外无 A 记录**：只能在校园网内解析，校外必须走 `webvpn.bupt.edu.cn`。
* **节假日调休未处理**：课表只给周次，无法知道法定假日调休，需要你自己在系统日历里调整。
