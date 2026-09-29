package com.bupt.calendar.calendar

import android.Manifest
import android.content.ContentUris
import android.content.ContentValues
import android.content.Context
import android.content.pm.PackageManager
import android.provider.CalendarContract
import android.util.Log
import androidx.core.content.ContextCompat
import com.bupt.calendar.core.CalendarEvent
import com.bupt.calendar.core.EventRef
import java.util.TimeZone

/**
 * 系统日历读写。
 *
 * # 「只清除本 App 添加的日程」是怎么做到的
 *
 * 系统日历是一个公共数据库，任何人都可以往里写事件，而且**无法按来源筛选**——
 * 事件表里没有「哪个 App 写的」这一列。所以这里不依赖系统日历做归属判断，
 * 而是依赖 Rust 侧那份导入记录：
 *
 * 1. 导入时每写一条事件，就把返回的 `_ID` 连同业务指纹一起记进本地库；
 * 2. 清除时遍历本地记录，按 `_ID` 精确删除；
 * 3. 只有当 `_ID` 失效（用户手动删过）时，才退回用「标题 + 开始时间」匹配。
 *
 * 这样既不会误删用户自己建的日程，也不会因为事件被手动改动而残留垃圾。
 */
class CalendarRepository(private val context: Context) {

    companion object {
        private const val TAG = "CalendarRepo"

        /** 要写入的系统日历账户名；找不到就退回任意可写日历。 */
        private const val PREFERRED_ACCOUNT_NAME = "bupt.calendar"

        val REQUIRED_PERMISSIONS = arrayOf(
            Manifest.permission.READ_CALENDAR,
            Manifest.permission.WRITE_CALENDAR,
        )
    }

    private val resolver get() = context.contentResolver

    /** 是否已经拿到日历读写权限。 */
    fun hasPermission(): Boolean = REQUIRED_PERMISSIONS.all {
        ContextCompat.checkSelfPermission(context, it) == PackageManager.PERMISSION_GRANTED
    }

    /**
     * 找一个可写的日历。
     *
     * 优先用本 App 自己创建的日历（这样在系统日历 App 里可以单独开关它的显示，
     * 用户想「眼不见为净」时很方便）；没有就退回任意可写日历。
     *
     * @return 日历 `_ID`，找不到返回 -1
     */
    fun findWritableCalendarId(): Long {
        val projection = arrayOf(
            CalendarContract.Calendars._ID,
            CalendarContract.Calendars.ACCOUNT_NAME,
            CalendarContract.Calendars.CALENDAR_ACCESS_LEVEL,
            CalendarContract.Calendars.IS_PRIMARY,
        )
        // 只取可写（>= CAL_ACCESS_CONTRIBUTOR）的日历
        val selection = "${CalendarContract.Calendars.CALENDAR_ACCESS_LEVEL} >= ?"
        val args = arrayOf(CalendarContract.Calendars.CAL_ACCESS_CONTRIBUTOR.toString())

        var fallback = -1L
        runCatching {
            resolver.query(
                CalendarContract.Calendars.CONTENT_URI,
                projection,
                selection,
                args,
                null,
            )?.use { cursor ->
                val idIdx = cursor.getColumnIndexOrThrow(CalendarContract.Calendars._ID)
                val acctIdx = cursor.getColumnIndexOrThrow(CalendarContract.Calendars.ACCOUNT_NAME)
                while (cursor.moveToNext()) {
                    val id = cursor.getLong(idIdx)
                    val account = cursor.getString(acctIdx) ?: ""
                    if (fallback == -1L) fallback = id
                    if (account == PREFERRED_ACCOUNT_NAME) return id
                }
            }
        }.onFailure { Log.w(TAG, "查询可写日历失败", it) }

        return fallback
    }

    /**
     * 确保存在一个可写的日历，必要时创建一个本 App 专属的本地日历。
     *
     * @return 日历 `_ID`，失败返回 -1
     */
    fun ensureCalendar(): Long {
        val existing = findWritableCalendarId()
        if (existing != -1L) return existing

        // 创建一个本地日历（ACCOUNT_TYPE = LOCAL，不参与云端同步）
        val values = ContentValues().apply {
            put(CalendarContract.Calendars.ACCOUNT_NAME, PREFERRED_ACCOUNT_NAME)
            put(CalendarContract.Calendars.ACCOUNT_TYPE, CalendarContract.ACCOUNT_TYPE_LOCAL)
            put(CalendarContract.Calendars.NAME, "北邮课表")
            put(CalendarContract.Calendars.CALENDAR_DISPLAY_NAME, "北邮课表导入")
            put(CalendarContract.Calendars.CALENDAR_COLOR, 0xFF1565C0.toInt())
            put(
                CalendarContract.Calendars.CALENDAR_ACCESS_LEVEL,
                CalendarContract.Calendars.CAL_ACCESS_OWNER,
            )
            put(CalendarContract.Calendars.OWNER_ACCOUNT, PREFERRED_ACCOUNT_NAME)
            put(CalendarContract.Calendars.VISIBLE, 1)
            put(CalendarContract.Calendars.SYNC_EVENTS, 1)
        }
        val uri = CalendarContract.Calendars.CONTENT_URI
            .buildUpon()
            .appendQueryParameter(CalendarContract.CALLER_IS_SYNCADAPTER, "true")
            .appendQueryParameter(CalendarContract.Calendars.ACCOUNT_NAME, PREFERRED_ACCOUNT_NAME)
            .appendQueryParameter(
                CalendarContract.Calendars.ACCOUNT_TYPE,
                CalendarContract.ACCOUNT_TYPE_LOCAL,
            )
            .build()

        return runCatching {
            val result = resolver.insert(uri, values)
            result?.lastPathSegment?.toLongOrNull() ?: -1L
        }.onFailure { Log.w(TAG, "创建本地日历失败", it) }.getOrDefault(-1L)
    }

    /**
     * 批量写入日程。
     *
     * @return 与 [events] 等长的列表，元素是写入成功的事件 `_ID`；
     *         写入失败的项为 -1（调用方仍会记录该条，只是没有 `_ID`）
     */
    fun insertEvents(calendarId: Long, events: List<CalendarEvent>): List<Long> {
        if (calendarId <= 0) return List(events.size) { -1L }
        val tz = TimeZone.getDefault().id
        return events.map { ev ->
            val values = ContentValues().apply {
                put(CalendarContract.Events.CALENDAR_ID, calendarId)
                put(CalendarContract.Events.TITLE, ev.title)
                put(CalendarContract.Events.DESCRIPTION, ev.description)
                put(CalendarContract.Events.EVENT_LOCATION, ev.location)
                put(CalendarContract.Events.DTSTART, ev.startMillis)
                put(CalendarContract.Events.DTEND, ev.endMillis)
                put(CalendarContract.Events.ALL_DAY, if (ev.allDay) 1 else 0)
                put(CalendarContract.Events.EVENT_TIMEZONE, tz)
                // 标成忙碌，避免给别人排会时撞课
                put(CalendarContract.Events.AVAILABILITY, CalendarContract.Events.AVAILABILITY_BUSY)
            }
            runCatching {
                val uri = resolver.insert(CalendarContract.Events.CONTENT_URI, values)
                uri?.lastPathSegment?.toLongOrNull() ?: -1L
            }.onFailure { Log.w(TAG, "写入日程失败: ${ev.title} @ ${ev.start}", it) }
                .getOrDefault(-1L)
        }
    }

    /** 结果统计：`(成功删除数, 失败数)`。 */
    data class DeleteResult(val deleted: Int, val failed: Int)

    /**
     * 删除本 App 记录过的日程。
     *
     * 优先按记录的 `_ID` 精确删除；`_ID` 缺失或已失效时，
     * 退回用「标题 + 开始时间」匹配——这两个字段一起基本可以唯一确定一条课程日程。
     */
    fun deleteEvents(refs: List<EventRef>): DeleteResult {
        var deleted = 0
        var failed = 0
        for (ref in refs) {
            val ok = when {
                ref.calendarEventId > 0 -> deleteById(ref.calendarEventId)
                else -> deleteByTitleAndStart(ref.title, ref.start)
            }
            if (ok) deleted++ else failed++
        }
        return DeleteResult(deleted, failed)
    }

    private fun deleteById(eventId: Long): Boolean = runCatching {
        val uri = ContentUris.withAppendedId(CalendarContract.Events.CONTENT_URI, eventId)
        resolver.delete(uri, null, null) > 0
    }.onFailure { Log.w(TAG, "按 id 删除日程失败: $it", it) }.getOrDefault(false)

    private fun deleteByTitleAndStart(title: String, start: String): Boolean = runCatching {
        // start 形如 2026-03-02T08:00:00，转成本地时间毫秒后用于匹配
        val millis = parseLocalMillis(start) ?: return false
        val selection = "${CalendarContract.Events.TITLE} = ? AND " +
            "${CalendarContract.Events.DTSTART} = ?"
        val args = arrayOf(title, millis.toString())
        resolver.delete(CalendarContract.Events.CONTENT_URI, selection, args) > 0
    }.onFailure { Log.w(TAG, "按标题+时间删除日程失败", it) }.getOrDefault(false)

    /** 把 `YYYY-MM-DDTHH:MM:SS` 按本机时区转成 epoch 毫秒。 */
    private fun parseLocalMillis(local: String): Long? = runCatching {
        val fmt = java.text.SimpleDateFormat("yyyy-MM-dd'T'HH:mm:ss", java.util.Locale.US)
        fmt.timeZone = TimeZone.getDefault()
        fmt.parse(local)?.time
    }.getOrNull()
}
