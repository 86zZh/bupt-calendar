package com.bupt.calendar.core

import android.util.Log
import com.google.gson.Gson
import com.google.gson.annotations.SerializedName

/**
 * Rust 核心（`libbupt_core.so`）的 Kotlin 门面。
 *
 * 所有解析、日程换算、导入记录的读写都在 Rust 侧完成，
 * 这里只做「JSON 字符串 ↔ Kotlin 数据类」的转换。
 * 这样业务逻辑可以在桌面端用 `cargo test` 快速验证，不需要装模拟器。
 *
 * 对应的 Rust 实现在 `core/src/jni_bridge.rs` 与 `core/src/api.rs`。
 */
object BuptCore {

    private const val TAG = "BuptCore"

    /** 加载失败时的原因，供 UI 提示（通常说明 .so 没打进 APK）。 */
    var loadError: String? = null
        private set

    private val gson = Gson()

    init {
        loadError = try {
            System.loadLibrary("bupt_core")
            null
        } catch (t: Throwable) {
            Log.e(TAG, "加载 libbupt_core.so 失败", t)
            t.message ?: t.toString()
        }
    }

    val isAvailable: Boolean get() = loadError == null

    // ---------------------------------------------------------------------
    // 原生方法
    // ---------------------------------------------------------------------

    private external fun nativeParseHtml(html: String): String
    private external fun nativeParseHtmlDocuments(docsJson: String): String
    private external fun nativeGetSchedule(scheduleId: String): String
    private external fun nativeSuggestTermStart(today: String): String
    private external fun nativePlanEvents(
        entriesJson: String,
        termStart: String,
        scheduleId: String,
        weekFilterJson: String,
        storePath: String,
    ): String

    private external fun nativePreviewEvents(
        entriesJson: String,
        termStart: String,
        scheduleId: String,
    ): String

    private external fun nativeRecordImport(storePath: String, requestJson: String): String
    private external fun nativeListImportedEvents(storePath: String): String
    private external fun nativeListImportBatches(storePath: String): String
    private external fun nativeImportedEventCount(storePath: String): Long
    private external fun nativeClearRecords(storePath: String): Boolean
    private external fun nativeDeleteRecords(storePath: String, fingerprintsJson: String): Long
    private external fun nativeAttachCalendarId(
        storePath: String,
        fingerprint: String,
        calendarId: Long,
    ): Boolean

    // ---------------------------------------------------------------------
    // 类型安全的包装
    // ---------------------------------------------------------------------

    /** 解析课表 HTML。解析失败时 [ParseHtmlResult.report] 里会有告警信息。 */
    fun parseHtml(html: String): ParseHtmlResult =
        gson.fromJson(requireAvailable { nativeParseHtml(html) }, ParseHtmlResult::class.java)

    /**
     * 解析「主页面 + 各个内嵌框架」的多个 HTML 文档，结果合并。
     *
     * 课表经常显示在 iframe / frame 里，主文档没有课表，所以抓取时要把
     * 每一层框架的内容都带上，否则会误报「找不到课表」。
     */
    fun parseHtmlDocuments(docs: List<String>): ParseHtmlResult {
        val filtered = docs.filter { it.isNotBlank() }
        if (filtered.isEmpty()) return ParseHtmlResult()
        if (filtered.size == 1) return parseHtml(filtered[0])
        return gson.fromJson(
            requireAvailable { nativeParseHtmlDocuments(gson.toJson(filtered)) },
            ParseHtmlResult::class.java,
        )
    }

    /** 取作息表；[scheduleId] 传空串表示北邮本部作息。 */
    fun getSchedule(scheduleId: String = ""): BellSchedule =
        gson.fromJson(requireAvailable { nativeGetSchedule(scheduleId) }, BellSchedule::class.java)

    /** 依据今天日期建议学期第一周周一（`YYYY-MM-DD`）。 */
    fun suggestTermStart(today: String?): String =
        requireAvailable { nativeSuggestTermStart(today ?: "") }

    /** 生成导入计划（展开日程 + 按记录库去重）。 */
    fun planEvents(
        entriesJson: String,
        termStart: String,
        scheduleId: String = "",
        weekFilterJson: String = "",
        storePath: String = "",
    ): PlanResult = gson.fromJson(
        requireAvailable {
            nativePlanEvents(entriesJson, termStart, scheduleId, weekFilterJson, storePath)
        },
        PlanResult::class.java,
    )

    /**
     * 预览日程（不做去重）。
     *
     * 用于导入前给用户核对：「第 N 周的第 1 天是不是这个日期」，
     * 一眼就能看出有没有错位。
     */
    fun previewEvents(entriesJson: String, termStart: String, scheduleId: String = ""): PlanResult {
        val raw = requireAvailable {
            nativePreviewEvents(entriesJson, termStart, scheduleId)
        }
        ensureNoError(raw)
        return gson.fromJson(raw, PlanResult::class.java)
    }

    /**
     * 把写入系统日历的结果登记到本地记录库。
     *
     * Rust 侧失败时返回的是 `{"error": "..."}` 这种**整体错误载荷**，
     * 而不是一个带 error 字段的 ImportOutcome，所以这里必须先检查再反序列化
     * ——否则出错时会反序列化出一个全是默认值的对象，静默当成成功。
     */
    fun recordImport(storePath: String, request: RecordImportRequest): ImportOutcome {
        val raw = requireAvailable { nativeRecordImport(storePath, gson.toJson(request)) }
        ensureNoError(raw)
        return gson.fromJson(raw, ImportOutcome::class.java)
    }

    /** 列出本 App 记录过的全部日程（一键清空的依据）。 */
    fun listImportedEvents(storePath: String): List<EventRef> =
        gson.fromJson(
            requireAvailable { nativeListImportedEvents(storePath) },
            Array<EventRef>::class.java,
        ).toList()

    fun listImportBatches(storePath: String): List<ImportBatch> =
        gson.fromJson(
            requireAvailable { nativeListImportBatches(storePath) },
            Array<ImportBatch>::class.java,
        ).toList()

    fun importedEventCount(storePath: String): Long =
        requireAvailable { nativeImportedEventCount(storePath) }

    /** 清空本地记录（**系统日历事件必须由调用方先删除**）。 */
    fun clearRecords(storePath: String): Boolean =
        requireAvailable { nativeClearRecords(storePath) }

    fun deleteRecords(storePath: String, fingerprints: List<String>): Long =
        requireAvailable { nativeDeleteRecords(storePath, gson.toJson(fingerprints)) }

    fun attachCalendarId(storePath: String, fingerprint: String, calendarId: Long): Boolean =
        requireAvailable { nativeAttachCalendarId(storePath, fingerprint, calendarId) }

    // ---------------------------------------------------------------------

    private inline fun <T> requireAvailable(block: () -> T): T {
        val err = loadError
        if (err != null) {
            throw IllegalStateException("Rust 核心未加载：$err")
        }
        return block()
    }

    /** 把 Rust 返回的错误载荷转成人话。Rust 出错时返回 `{"error":"..."}`。 */
    fun errorOf(rawJson: String): String? = runCatching {
        gson.fromJson(rawJson, ErrorPayload::class.java)?.error
    }.getOrNull()

    /** 解析 JSON 里的 error 字段，没有就返回 null。 */
    fun ensureNoError(json: String) {
        errorOf(json)?.let { throw IllegalStateException(it) }
    }

    /** 解析 `planEvents` / `previewEvents` 的返回。 */
    fun parsePlanResult(json: String): PlanResult =
        gson.fromJson(json, PlanResult::class.java)
}

// ---------------------------------------------------------------------------
// 与 Rust `models.rs` / `api.rs` 一一对应的数据类
// 字段名必须与 serde 的默认输出保持一致（snake_case）
// ---------------------------------------------------------------------------

private data class ErrorPayload(val error: String?)

/** 单双周标记，对应 Rust 的 `WeekType`（serde 序列化为小写）。 */
enum class WeekType {
    @SerializedName("every")
    EVERY,

    @SerializedName("odd")
    ODD,

    @SerializedName("even")
    EVEN,
}

/** 一段周次区间。 */
data class WeekRange(
    val start: Int = 1,
    val end: Int = 1,
    @SerializedName("week_type") val weekType: WeekType = WeekType.EVERY,
)

/** 一条课表条目。 */
data class CourseEntry(
    val name: String = "",
    val teacher: String = "",
    val room: String = "",
    val group: String = "",
    val day: Int = 1,
    @SerializedName("start_node") val startNode: Int = 1,
    @SerializedName("end_node") val endNode: Int = 1,
    val weeks: List<WeekRange> = emptyList(),
    val raw: String = "",
)

/** 解析诊断报告。 */
data class ParseReport(
    val entries: List<CourseEntry> = emptyList(),
    val warnings: List<String> = emptyList(),
    val strategy: String = "none",
    @SerializedName("found_table") val foundTable: Boolean = false,
)

/** 按课程名聚合的摘要。 */
data class CourseSummary(
    val name: String = "",
    val teacher: String = "",
    val room: String = "",
    val entries: Int = 0,
    val events: Int = 0,
)

/** `parseHtml` 的返回。 */
data class ParseHtmlResult(
    val report: ParseReport = ParseReport(),
    val entries: List<CourseEntry> = emptyList(),
    val summary: List<CourseSummary> = emptyList(),
)

/** 一条待写入系统日历的日程。 */
data class CalendarEvent(
    val fingerprint: String = "",
    val title: String = "",
    val description: String = "",
    val location: String = "",
    val start: String = "",
    val end: String = "",
    /** 开始时间的 epoch 毫秒，Rust 已按本机时区算好，可直接写系统日历 */
    @SerializedName("start_millis") val startMillis: Long = 0,
    /** 结束时间的 epoch 毫秒 */
    @SerializedName("end_millis") val endMillis: Long = 0,
    @SerializedName("all_day") val allDay: Boolean = false,
    @SerializedName("source_index") val sourceIndex: Int = 0,
)

/** `planEvents` 的返回。 */
data class PlanResult(
    val events: List<CalendarEvent> = emptyList(),
    @SerializedName("already_imported") val alreadyImported: Int = 0,
    @SerializedName("total_events") val totalEvents: Int = 0,
    val summary: List<CourseSummary> = emptyList(),
    val error: String? = null,
)

/** 记录一次导入的请求。 */
data class RecordImportRequest(
    @SerializedName("term_start") val termStart: String,
    val source: String,
    @SerializedName("calendar_event_ids") val calendarEventIds: List<Long>,
    val events: List<CalendarEvent>,
    @SerializedName("replace_existing") val replaceExisting: Boolean = false,
)

/** 导入结果统计。出错时 [BuptCore.recordImport] 会抛异常，而不是返回这个对象。 */
data class ImportOutcome(
    @SerializedName("batch_id") val batchId: Long = 0,
    val inserted: Int = 0,
    val skipped: Int = 0,
)

/** 记录库里的一条日程引用。 */
data class EventRef(
    val fingerprint: String = "",
    @SerializedName("calendar_event_id") val calendarEventId: Long = -1,
    val title: String = "",
    val start: String = "",
    val end: String = "",
    val location: String = "",
)

/** 历史导入批次。 */
data class ImportBatch(
    val id: Long = 0,
    @SerializedName("created_at") val createdAt: Long = 0,
    @SerializedName("term_start") val termStart: String = "",
    val source: String = "",
    @SerializedName("event_count") val eventCount: Long = 0,
)

/** 作息表里的一小节。 */
data class Section(
    val index: Int = 0,
    val start: String = "",
    val end: String = "",
)

/** 一整套作息表。 */
data class BellSchedule(
    val id: String = "",
    val name: String = "",
    val sections: List<Section> = emptyList(),
)
