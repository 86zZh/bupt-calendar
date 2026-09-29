package com.bupt.calendar.ui

import android.annotation.SuppressLint
import android.content.ActivityNotFoundException
import android.content.ClipData
import android.content.ClipboardManager
import android.content.Context
import android.content.Intent
import android.net.Uri
import android.os.Bundle
import android.provider.Settings
import android.util.Log
import android.view.View
import android.view.ViewGroup
import android.webkit.CookieManager
import android.webkit.JavascriptInterface
import android.webkit.SslErrorHandler
import android.webkit.WebChromeClient
import android.webkit.WebResourceRequest
import android.webkit.WebSettings
import android.webkit.WebView
import android.webkit.WebViewClient
import android.widget.EditText
import android.widget.NumberPicker
import android.widget.Toast
import androidx.activity.OnBackPressedCallback
import androidx.activity.enableEdgeToEdge
import androidx.activity.result.contract.ActivityResultContracts
import androidx.appcompat.app.AppCompatActivity
import androidx.core.view.ViewCompat
import androidx.core.view.WindowInsetsCompat
import androidx.core.view.WindowInsetsControllerCompat
import androidx.core.view.updatePadding
import androidx.lifecycle.lifecycleScope
import com.bupt.calendar.R
import com.bupt.calendar.calendar.CalendarRepository
import com.bupt.calendar.core.BuptCore
import com.bupt.calendar.core.CalendarEvent
import com.bupt.calendar.core.PlanResult
import com.bupt.calendar.core.RecordImportRequest
import com.bupt.calendar.databinding.ActivityMainBinding
import com.bupt.calendar.databinding.DialogConfirmWeekBinding
import com.bupt.calendar.web.TimetableScraperJs
import com.google.android.material.datepicker.MaterialDatePicker
import com.google.android.material.dialog.MaterialAlertDialogBuilder
import com.google.gson.Gson
import com.google.gson.JsonObject
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.launch
import kotlinx.coroutines.withContext
import java.io.File
import java.text.SimpleDateFormat
import java.util.Calendar
import java.util.Date
import java.util.Locale
import java.util.TimeZone

/**
 * 主界面：内置浏览器 + 悬浮导入按钮。
 *
 * 用户路径只有一条：打开 App → 在浏览器里登进教务 → 翻到课表页 →
 * 点悬浮按钮 → 确认学期起始日期 → 日程入库。
 * 课表变化时用右上角菜单「清空本 App 导入的日程」，再重新导一次。
 */
class MainActivity : AppCompatActivity() {

    companion object {
        private const val TAG = "MainActivity"

        /** 校园网直连入口（注意是 http，需要允许明文流量） */
        private const val URL_CAMPUS = "http://jwgl.bupt.edu.cn/jsxsd/"

        /** 校外 WebVPN 入口；登录后在页面内点进教务即可，代理会保持会话 */
        private const val URL_WEBVPN = "https://webvpn.bupt.edu.cn/"

        /**
         * 只接受来自北邮域名的页面内容。
         * `@JavascriptInterface` 注入的方法理论上能被页面里的任意脚本调用，
         * 限制来源可以避免用户不小心在别的站点上点了按钮就把内容当课表解析。
         */
        private val ALLOWED_HOSTS = Regex(
            """^https?://([\w-]+\.)*(bupt\.edu\.cn)(:\d+)?/""",
            RegexOption.IGNORE_CASE,
        )

        /**
         * User-Agent 伪装成桌面版。
         *
         * 做法与 WakeupSchedule 完全一致：**不换整条 UA，而是把里面的
         * `Mobile` 改成 `eliboM`、`Android` 改成 `diordnA`（反向拼写）**，
         * 让教务系统认不出这是手机浏览器。
         *
         * 为什么不用一条标准的桌面 UA：教务系统对 UA 的判定方式不止一种，
         * 学长这套改写是长期实测有效的；而且保留原始 UA 的其余部分，
         * 避免某些站点因为 UA 过于陌生而给异常页面。
         *
         * 效果是教务直接返回**桌面版页面**，从源头避开手机端那套
         * 「点课表就弹新窗口」的跳转 —— 这就是导入失败的根本原因。
         */
        private fun desktopUserAgent(original: String): String =
            original.replace("Mobile", "eliboM").replace("Android", "diordnA")
    }

    private lateinit var binding: ActivityMainBinding
    private val gson = Gson()
    private val calendarRepo by lazy { CalendarRepository(this) }
    private val storePath: String by lazy { File(filesDir, "imports.db").absolutePath }

    /** 正在等待授权结果的动作（导入或清空），授权回来后再执行 */
    private var pendingAfterPermission: (() -> Unit)? = null

    /** 防止一次抓取回调触发多次导入 */
    @Volatile
    private var extractInFlight = false

    /** 最近一次解析出的课表条目，供「核对推算结果」使用 */
    private var lastParsedEntries: List<com.bupt.calendar.core.CourseEntry>? = null

    private val permissionLauncher = registerForActivityResult(
        ActivityResultContracts.RequestMultiplePermissions(),
    ) { result ->
        val granted = result.values.all { it }
        if (granted) {
            pendingAfterPermission?.invoke()
        } else {
            toast(getString(R.string.need_calendar_permission))
        }
        pendingAfterPermission = null
    }

    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        // 让内容铺满整屏（含状态栏与导航栏区域），随后用 insets 自己把内容让开。
        // 不这么做的话，Android 15（targetSdk 35）会强制全屏，而老系统不会，
        // 两种情况下顶部位置就不一致了。
        enableEdgeToEdge()
        binding = ActivityMainBinding.inflate(layoutInflater)
        setContentView(binding.root)

        applyWindowInsets()

        if (!BuptCore.isAvailable) {
            newDialog()
                .setTitle(R.string.core_unavailable_title)
                .setMessage(getString(R.string.core_unavailable_message, BuptCore.loadError ?: "?"))
                .setPositiveButton(android.R.string.ok, null)
                .show()
        }

        setupToolbar()
        setupUrlBar()
        setupWebView()
        setupFab()
    }

    /**
     * 处理「灵动岛 / 挖孔 / 状态栏 / 导航栏」占用的空间。
     *
     * 这是顶部内容被挡住的原因：Android 15 起系统强制全屏显示，
     * 内容会一直画到屏幕最顶端，于是被灵动岛或状态栏压住。
     *
     * 做法：
     * * 给 `topArea` 加上等于「状态栏 + 挖孔」高度的上内边距 ——
     *   因为内边距区域同样会被背景填充，**状态栏那一条仍然是紫色**，
     *   只是里面的标题和右上角三个点整体下移，不再被遮住；
     * * 悬浮导入按钮抬高一个导航栏的高度，避免贴在系统手势条上。
     */
    private fun applyWindowInsets() {
        val baseFabMargin = (16 * resources.displayMetrics.density).toInt()

        ViewCompat.setOnApplyWindowInsetsListener(binding.root) { _, insets ->
            val bars = insets.getInsets(
                WindowInsetsCompat.Type.systemBars() or WindowInsetsCompat.Type.displayCutout(),
            )
            binding.topArea.updatePadding(
                left = bars.left,
                top = bars.top,
                right = bars.right,
            )
            (binding.fabImport.layoutParams as? ViewGroup.MarginLayoutParams)?.let { lp ->
                lp.bottomMargin = baseFabMargin + bars.bottom
                binding.fabImport.layoutParams = lp
            }
            // 不消费 insets，让其他子视图也能正常拿到
            insets
        }

        // 顶栏是深紫，状态栏/导航栏图标必须用浅色才看得见
        WindowInsetsControllerCompat(window, binding.root).apply {
            isAppearanceLightStatusBars = false
            isAppearanceLightNavigationBars = false
        }

        ViewCompat.requestApplyInsets(binding.root)
    }

    // ---------------------------------------------------------------------
    // 界面初始化
    // ---------------------------------------------------------------------

    private fun setupToolbar() {
        binding.toolbar.setOnMenuItemClickListener { item ->
            when (item.itemId) {
                R.id.action_clear -> {
                    confirmClearImported()
                    true
                }
                R.id.action_status -> {
                    showImportStatus()
                    true
                }
                R.id.action_export -> {
                    exportDiagnostics()
                    true
                }
                R.id.action_help -> {
                    showHelp()
                    true
                }
                else -> false
            }
        }
    }

    private fun setupUrlBar() {
        binding.chipGroupSource.setOnCheckedStateChangeListener { _, checked ->
            val url = if (checked.contains(R.id.chipWebVpn)) URL_WEBVPN else URL_CAMPUS
            binding.editUrl.setText(url)
            loadUrl(url)
        }
        binding.chipCampus.isChecked = true
        binding.editUrl.setText(URL_CAMPUS)

        binding.editUrl.setOnEditorActionListener { v, _, _ ->
            val text = (v as EditText).text.toString().trim()
            if (text.isNotEmpty()) {
                loadUrl(normalizeUrl(text))
            }
            true
        }
    }

    private fun normalizeUrl(raw: String): String =
        if (raw.startsWith("http://") || raw.startsWith("https://")) raw else "http://$raw"

    private fun setupFab() {
        binding.fabImport.setOnClickListener { onImportClicked() }
    }

    @SuppressLint("SetJavaScriptEnabled")
    private fun setupWebView() {
        with(binding.webView.settings) {
            javaScriptEnabled = true
            // domStorageEnabled 已覆盖 WebView 的本地存储需求
            // （databaseEnabled 自 API 30 起废弃，不再设置）
            domStorageEnabled = true
            // 教务页面有不少 http 资源，必须允许混合内容
            mixedContentMode = WebSettings.MIXED_CONTENT_ALWAYS_ALLOW
            loadWithOverviewMode = true
            useWideViewPort = true
            builtInZoomControls = true
            displayZoomControls = false
            // 用桌面版 UA：强智的手机版页面结构不同、且没有完整课表
            userAgentString = desktopUserAgent(userAgentString ?: "")
        }
        CookieManager.getInstance().setAcceptCookie(true)
        CookieManager.getInstance().setAcceptThirdPartyCookies(binding.webView, true)

        binding.webView.addJavascriptInterface(JsBridge(), TimetableScraperJs.BRIDGE_NAME)

        binding.webView.webViewClient = object : WebViewClient() {
            override fun shouldOverrideUrlLoading(
                view: WebView,
                request: WebResourceRequest,
            ): Boolean = false

            override fun onPageFinished(view: WebView, url: String) {
                super.onPageFinished(view, url)
                binding.progressBar.visibility = View.GONE
                // 把「开新窗口」的链接改成当前页打开，免得课表跳进我们看不见的窗口
                view.evaluateJavascript(TimetableScraperJs.neutralizeNewWindowScript(), null)
                // 页面加载完探测一次，用于提示用户是否已经到课表页
                view.evaluateJavascript(TimetableScraperJs.probeScript(), null)
            }

            @Suppress("DEPRECATION")
            override fun onReceivedSslError(
                view: WebView?,
                handler: SslErrorHandler?,
                error: android.net.http.SslError?,
            ) {
                // WebVPN / 教务的证书链在部分 Android 版本上校验不过，
                // 这里放行（与参考实现一致），否则用户会卡在白屏
                Log.w(TAG, "SSL 错误，已放行: ${error?.url}")
                handler?.proceed()
            }
        }

        binding.webView.webChromeClient = object : WebChromeClient() {
            override fun onProgressChanged(view: WebView?, newProgress: Int) {
                binding.progressBar.visibility = if (newProgress in 1..99) View.VISIBLE else View.GONE
                binding.progressBar.progress = newProgress
            }
        }

        // 关于「新窗口」：这里**刻意不覆盖** onCreateWindow，也**刻意不设置**
        // setSupportMultipleWindows —— 保持 WebView 默认行为。
        //
        // 这是参照 WakeupSchedule 的做法：它的 webViewClient / webChromeClient
        // 都是空壳（只处理 SSL 证书错误），不去跟页面的开窗行为较劲。
        // 默认配置下 window.open / target=_blank 会落到当前这个 WebView 里加载。
        //
        // 真正解决问题的是另外两件事：
        //   1. 桌面版 User-Agent（见上面的 desktopUserAgent）—— 让教务系统直接返回
        //      桌面版页面，从源头避开手机端那套「弹新窗口」的跳转；
        //   2. 抓取时**连 iframe 里的内容一起取** —— 课表往往内嵌在框架里，
        //      这也是 WakeupSchedule 明确做了而我们原先漏掉的一步。

        // 返回键优先让 WebView 后退，避免用户一点返回就退出 App
        onBackPressedDispatcher.addCallback(this, object : OnBackPressedCallback(true) {
            override fun handleOnBackPressed() {
                if (binding.webView.canGoBack()) {
                    binding.webView.goBack()
                } else {
                    isEnabled = false
                    onBackPressedDispatcher.onBackPressed()
                }
            }
        })

        loadUrl(URL_CAMPUS)
    }

    private fun loadUrl(url: String) {
        binding.webView.loadUrl(url)
    }

    // ---------------------------------------------------------------------
    // 导入流程
    // ---------------------------------------------------------------------

    private fun onImportClicked() {
        if (!BuptCore.isAvailable) {
            toast(getString(R.string.core_unavailable_title))
            return
        }
        extractInFlight = false
        toast(getString(R.string.extracting))
        binding.webView.evaluateJavascript(TimetableScraperJs.script(), null)
    }

    /** JS 抓取完成后的回调（在 WebView 的 JS 线程调用，需切回主线程）。 */
    private inner class JsBridge {
        @JavascriptInterface
        fun onExtractResult(json: String) {
            runOnUiThread { handleExtractResult(json) }
        }

        @JavascriptInterface
        fun onProbeResult(json: String) {
            runOnUiThread {
                runCatching {
                    val obj = gson.fromJson(json, JsonObject::class.java)
                    val hasTable = obj.get("hasTable")?.asBoolean ?: false
                    val weekNo = obj.get("weekNo")?.takeIf { !it.isJsonNull }?.asInt
                    if (hasTable) {
                        val hint = if (weekNo != null) {
                            getString(R.string.hint_on_timetable_page_week, weekNo)
                        } else {
                            getString(R.string.hint_on_timetable_page)
                        }
                        binding.fabImport.text = hint
                    } else {
                        binding.fabImport.text = getString(R.string.fab_import)
                    }
                }
            }
        }
    }

    private fun handleExtractResult(json: String) {
        if (extractInFlight) return
        extractInFlight = true

        // 无论成功失败都要把标志复位，否则用户点第二次不会有任何反应
        try {
            processExtractResult(json)
        } finally {
            extractInFlight = false
        }
    }

    private fun processExtractResult(json: String) {
        val obj = runCatching { gson.fromJson(json, JsonObject::class.java) }.getOrNull()
        if (obj == null) {
            toast(getString(R.string.extract_failed_parse))
            return
        }

        // 只信任北邮域名下的页面内容
        val pageUrl = obj.getAsJsonObject("page")?.get("url")?.asString.orEmpty()
        if (pageUrl.isNotEmpty() && !ALLOWED_HOSTS.containsMatchIn(pageUrl)) {
            toast(getString(R.string.wrong_site, pageUrl))
            return
        }

        val ok = obj.get("ok")?.asBoolean ?: false
        if (!ok) {
            val reason = obj.get("reason")?.asString.orEmpty()
            val msg = obj.get("message")?.asString ?: getString(R.string.extract_failed_unknown)
            newDialog()
                .setTitle(R.string.extract_failed_title)
                .setMessage(
                    buildString {
                        appendLine(msg)
                        appendLine()
                        appendLine(getString(R.string.fail_reason_label, reasonLabel(reason)))
                        if (pageUrl.isNotEmpty()) {
                            appendLine(getString(R.string.fail_page_label, pageUrl))
                        }
                        appendLine()
                        append(getString(R.string.fail_hint_export))
                    },
                )
                .setPositiveButton(android.R.string.ok, null)
                .show()
            return
        }

        val html = obj.get("html")?.asString.orEmpty()
        if (html.isBlank()) {
            toast(getString(R.string.extract_empty_html))
            return
        }

        // 课表可能在 iframe / frame 里，抓取脚本会把各层框架的完整 HTML 一起带回。
        // 注意 JS 侧回传的 frames 是一段**拼接的 HTML 字符串**（多个框架连在一起），
        // 不是 JSON 数组 —— 直接作为一个额外文档交给 Rust 一起解析。
        val framesHtml = obj.get("frames")?.asString.orEmpty()
        val documents = buildList {
            add(html)
            if (framesHtml.isNotBlank()) add(framesHtml)
        }

        // 用 Rust 解析（多个文档会合并结果并跨文档去重）
        val parsed = runCatching { BuptCore.parseHtmlDocuments(documents) }.getOrElse {
            newDialog().setTitle(R.string.extract_failed_title)
                .setMessage(it.message ?: it.toString())
                .setPositiveButton(android.R.string.ok, null).show()
            return
        }

        if (parsed.entries.isEmpty()) {
            val detail = buildString {
                appendLine(getString(R.string.no_course_title))
                appendLine()
                appendLine(getString(R.string.fail_found_table, parsed.report.foundTable))
                appendLine(getString(R.string.fail_strategy, parsed.report.strategy))
                if (pageUrl.isNotEmpty()) {
                    appendLine(getString(R.string.fail_page_label, pageUrl))
                }
                if (parsed.report.warnings.isNotEmpty()) {
                    appendLine()
                    appendLine(getString(R.string.fail_details_label))
                    parsed.report.warnings.forEach { appendLine("· $it") }
                } else {
                    appendLine()
                    appendLine(getString(R.string.no_course_hint))
                }
                appendLine()
                append(getString(R.string.fail_hint_export))
            }
            newDialog()
                .setTitle(R.string.no_course_title)
                .setMessage(detail)
                .setPositiveButton(android.R.string.ok, null)
                .show()
            return
        }

        val weekNo = obj.getAsJsonObject("page")?.get("weekNo")?.takeIf { !it.isJsonNull }?.asInt
        val suggested = resolveTermStart(weekNo)
        // 记住条目，后面「核对推算结果」要用
        lastParsedEntries = parsed.entries
        askTermStartThenImport(parsed.entries.size, parsed.summary.size, suggested, documents, weekNo)
    }

    /**
     * 推算学期第一周周一。
     *
     * 优先用课表页上的「第 N 周」反推：本周一 − (N−1)×7 天。
     * 拿不到就用 Rust 侧的建议值（按春夏/秋冬学期给一个大致日期），
     * 最终**必须由用户确认**——差一周整份课表就全错位了。
     */
    private fun resolveTermStart(pageWeek: Int?): Date {
        if (pageWeek != null && pageWeek >= 0) {
            val monday = mondayOfThisWeek()
            val cal = Calendar.getInstance().apply {
                time = monday
                add(Calendar.DAY_OF_YEAR, -(pageWeek - 1) * 7)
            }
            return cal.time
        }
        val today = SimpleDateFormat("yyyy-MM-dd", Locale.US).format(Date())
        val suggested = runCatching { BuptCore.suggestTermStart(today) }.getOrNull()
        return runCatching {
            SimpleDateFormat("yyyy-MM-dd", Locale.US).parse(suggested!!)!!
        }.getOrDefault(mondayOfThisWeek())
    }

    /**
     * 取「本周的周一」。
     *
     * ⚠️ 这里的星期换算是很容易出错的地方：Java 的 `Calendar.DAY_OF_WEEK`
     * 把**周日当成 1**（`SUNDAY=1, MONDAY=2, ..., SATURDAY=7`），
     * 而北邮的课表周次是以**周一**为一周起点的。
     *
     * 之前的写法 `(DAY_OF_WEEK + 5) % 7` 在周日会得到「回退 6 天」，
     * 也就是算出**上周一**，整整差一周 —— 会让整份课表错位。
     *
     * 现在改成按「距离周一的偏移」直接算：
     * 周一→0、周二→1、…、周六→5、**周日→6（往后推到下周的周一）**。
     * 也就是采用「周一是一周第一天、周日是最后一天」的口径，
     * 与学校按周一编号周次的方式一致。
     */
    private fun mondayOfThisWeek(): Date {
        val cal = Calendar.getInstance()
        val offsetFromMonday = when (cal.get(Calendar.DAY_OF_WEEK)) {
            Calendar.MONDAY -> 0
            Calendar.TUESDAY -> 1
            Calendar.WEDNESDAY -> 2
            Calendar.THURSDAY -> 3
            Calendar.FRIDAY -> 4
            Calendar.SATURDAY -> 5
            else -> 6 // Calendar.SUNDAY：本周最后一天，回到它前面的那个周一
        }
        cal.add(Calendar.DAY_OF_YEAR, -offsetFromMonday)
        cal.set(Calendar.HOUR_OF_DAY, 0)
        cal.set(Calendar.MINUTE, 0)
        cal.set(Calendar.SECOND, 0)
        cal.set(Calendar.MILLISECOND, 0)
        return cal.time
    }

    /**
     * 由「当前是第几周」反推「第 1 周周一」。
     *
     * 这是参考 WakeupSchedule 的做法：**不让用户去算日期，只让他说出今天第几周**
     * （这个信息教务页面上通常直接写着），日期由程序倒推。
     * 用户少填一个容易错的东西，就少一处错位的来源。
     */
    private fun termStartFromWeekNo(weekNo: Int): Date {
        val cal = Calendar.getInstance().apply {
            time = mondayOfThisWeek()
            add(Calendar.DAY_OF_YEAR, -(weekNo - 1) * 7)
        }
        return cal.time
    }

    /**
     * 弹出日期选择器确认学期第一周周一，然后导入。
     *
     * 日期必须由用户过一眼：差一周整份课表就整体错位。
     * 选择器里预填推算结果，用户直接点「确定」即可，也可以改成实际日期。
     */
    private fun askTermStartThenImport(
        courseCount: Int,
        courseKinds: Int,
        suggested: Date,
        documents: List<String>,
        weekNo: Int?,
    ) {
        val entries = lastParsedEntries
        if (entries == null) {
            // 兜底：拿不到条目就退回原来的日期选择器
            showDatePicker(suggested) { picked ->
                doImport(documents, SimpleDateFormat("yyyy-MM-dd", Locale.US).format(picked))
            }
            return
        }

        // 优先用页面上写着的当前周次；页面没说就按照当前日期猜一个
        val currentWeek = weekNo?.takeIf { it >= 0 }
            ?: guessCurrentWeek(suggested)

        askConfirmWeek(documents, entries, courseCount, courseKinds, currentWeek, fromPage = weekNo != null)
    }

    /** 页面没给出当前周次时，用推得的「第 1 周周一」反推今天是第几周。 */
    private fun guessCurrentWeek(termStartFromSuggestion: Date): Int {
        val fmt = SimpleDateFormat("yyyy-MM-dd", Locale.US)
        val start = fmt.parse(fmt.format(termStartFromSuggestion)) ?: return 1
        val diffDays = ((mondayOfThisWeek().time - start.time) / 86_400_000L).toInt()
        return (diffDays / 7 + 1).coerceIn(1, 30)
    }

    /**
     * 核心确认界面：**只让用户确认「今天是第几周」**。
     *
     * 这是参考 WakeupSchedule 的思路：用户不需要、也容易算错「第 1 周周一」，
     * 但「今天第几周」教务页面上通常直接写着，用户看一眼就知道。
     * 日期完全由程序倒推，减少一处出错来源。
     *
     * 同时给出「推算结果核对表」：列出前几门课各自会落在哪个日期，
     * 用户扫一眼就能发现整份课表有没有错位。
     */
    private fun askConfirmWeek(
        documents: List<String>,
        entries: List<com.bupt.calendar.core.CourseEntry>,
        courseCount: Int,
        courseKinds: Int,
        currentWeek: Int,
        fromPage: Boolean,
    ) {
        val fmt = SimpleDateFormat("yyyy-MM-dd", Locale.US)
        val week1Monday = termStartFromWeekNo(currentWeek)
        val weekMonday = mondayOfThisWeek()

        val header = getString(
            if (fromPage) R.string.confirm_week_from_page else R.string.confirm_week_guessed,
        )
        val message = buildString {
            appendLine(header)
            appendLine()
            appendLine(getString(R.string.confirm_week_span, currentWeek, fmt.format(weekMonday)))
            appendLine(getString(R.string.confirm_week1_monday, fmt.format(week1Monday)))
            appendLine()
            appendLine(getString(R.string.confirm_week_check_table))
            appendLine(verifyDateMapping(entries, week1Monday))
            appendLine()
            append(getString(R.string.confirm_week_tip))
        }

        // 三个选项横排成按钮（自定义布局），而不是 AlertDialog 默认的竖排文字按钮
        val view = DialogConfirmWeekBinding.inflate(layoutInflater)
        view.confirmMessage.text = message
        // 内容可能很长（含核对表），限制高度上限，保证按钮始终在屏幕内
        view.confirmScroll.post {
            val maxHeight = (resources.displayMetrics.heightPixels * 0.55f).toInt()
            if (view.confirmScroll.height > maxHeight) {
                view.confirmScroll.layoutParams =
                    view.confirmScroll.layoutParams.also { it.height = maxHeight }
            }
        }

        val dialog = newDialog()
            .setTitle(getString(R.string.confirm_week_title, courseKinds, courseCount))
            .setView(view.root)
            .create()

        view.btnConfirmImport.setOnClickListener {
            dialog.dismiss()
            doImport(documents, fmt.format(week1Monday))
        }
        view.btnEditWeek.setOnClickListener {
            dialog.dismiss()
            askWeekNumber(documents, entries, courseCount, courseKinds, currentWeek)
        }
        view.btnPickDate.setOnClickListener {
            dialog.dismiss()
            showDatePicker(week1Monday) { picked -> doImport(documents, fmt.format(picked)) }
        }

        dialog.show()
    }

    /** 用数字选择器让用户改「今天是第几周」，然后重新核对。 */
    private fun askWeekNumber(
        documents: List<String>,
        entries: List<com.bupt.calendar.core.CourseEntry>,
        courseCount: Int,
        courseKinds: Int,
        currentWeek: Int,
    ) {
        val picker = NumberPicker(this).apply {
            minValue = 1
            maxValue = 30
            value = currentWeek.coerceIn(1, 30)
            wrapSelectorWheel = false
        }
        val container = android.widget.FrameLayout(this).apply {
            val pad = (16 * resources.displayMetrics.density).toInt()
            setPadding(pad, pad, pad, pad)
            addView(
                picker,
                android.widget.FrameLayout.LayoutParams(
                    android.widget.FrameLayout.LayoutParams.WRAP_CONTENT,
                    android.widget.FrameLayout.LayoutParams.WRAP_CONTENT,
                    android.view.Gravity.CENTER,
                ),
            )
        }

        newDialog()
            .setTitle(R.string.dialog_week_title)
            .setMessage(R.string.dialog_week_message)
            .setView(container)
            .setPositiveButton(android.R.string.ok) { _, _ ->
                askConfirmWeek(
                    documents, entries, courseCount, courseKinds, picker.value, fromPage = false,
                )
            }
            .setNegativeButton(android.R.string.cancel, null)
            .show()
    }

    /**
     * 生成「推算结果核对表」：每门课第一节课会落在哪天。
     *
     * 这是给用户自查错位用的：如果星期几对不上，一眼就能看出来。
     */
    private fun verifyDateMapping(
        entries: List<com.bupt.calendar.core.CourseEntry>,
        week1Monday: Date,
    ): String {
        val weekNames = listOf("周一", "周二", "周三", "周四", "周五", "周六", "周日")
        // 按「星期 + 节次」排序，方便逐行核对
        val sorted = entries.sortedWith(
            compareBy({ it.day }, { it.startNode }),
        ).take(6)

        return sorted.joinToString("\n") { e ->
            val week = e.weeks.minOfOrNull { it.start } ?: 1
            val dayLabel = weekNames.getOrElse(e.day - 1) { "?" }
            val date = eventDateOf(week1Monday, week, e.day)
            val dateLabel = if (date != null) formatDateWithWeekday(date) else "?"
            val name = if (e.name.length > 12) e.name.take(12) + "…" else e.name
            "· 第${week}周 $dayLabel（$dateLabel）  ${e.startNode}-${e.endNode}节  $name"
        }
    }

    /** 取「第 1 周周一 + (周-1)*7 + (星期-1)」那一天。 */
    private fun eventDateOf(week1Monday: Date, week: Int, day: Int): Date? {
        val cal = Calendar.getInstance().apply {
            time = week1Monday
            add(Calendar.DAY_OF_YEAR, (week - 1) * 7 + (day - 1))
        }
        return cal.time
    }

    /** 格式化成「9月14日 周一」，便于核对。 */
    private fun formatDateWithWeekday(date: Date): String {
        val cal = Calendar.getInstance().apply { time = date }
        val offsetFromMonday = when (cal.get(Calendar.DAY_OF_WEEK)) {
            Calendar.MONDAY -> 0
            Calendar.TUESDAY -> 1
            Calendar.WEDNESDAY -> 2
            Calendar.THURSDAY -> 3
            Calendar.FRIDAY -> 4
            Calendar.SATURDAY -> 5
            else -> 6
        }
        val weekNames = listOf("周一", "周二", "周三", "周四", "周五", "周六", "周日")
        val md = SimpleDateFormat("M月d日", Locale.CHINA).format(date)
        return "$md ${weekNames[offsetFromMonday]}"
    }

    private fun showDatePicker(initial: Date, onPicked: (Date) -> Unit) {
        val picker = MaterialDatePicker.Builder.datePicker()
            .setTitleText(R.string.dialog_pick_week1_monday)
            .setSelection(initial.time)
            .build()
        picker.addOnPositiveButtonClickListener { selection ->
            // MaterialDatePicker 返回的是 UTC 当天 00:00，转成本地日期字符串
            val utc = Calendar.getInstance(TimeZone.getTimeZone("UTC")).apply { timeInMillis = selection }
            val local = Calendar.getInstance().apply {
                clear()
                set(
                    utc.get(Calendar.YEAR),
                    utc.get(Calendar.MONTH),
                    utc.get(Calendar.DAY_OF_MONTH),
                )
            }
            onPicked(local.time)
        }
        picker.show(supportFragmentManager, "term-start")
    }

    /** 真正执行导入：Rust 展开日程 → 写系统日历 → 登记记录。 */
    private fun doImport(documents: List<String>, termStart: String) {
        if (!ensureCalendarPermission()) {
            pendingAfterPermission = { doImport(documents, termStart) }
            return
        }

        lifecycleScope.launch {
            val result: Result<PlanResult> =
                withContext(Dispatchers.Default) {
                    runCatching {
                        val parsed = BuptCore.parseHtmlDocuments(documents)
                        val entriesJson = gson.toJson(parsed.entries)
                        val plan = BuptCore.planEvents(
                            entriesJson = entriesJson,
                            termStart = termStart,
                            scheduleId = "",
                            weekFilterJson = "",
                            storePath = storePath,
                        )
                        // Rust 侧出错时返回 {"error": "..."}，这里统一转成异常
                        plan.error?.let { throw IllegalStateException(it) }
                        plan
                    }
                }

            result.onFailure { err ->
                newDialog().setTitle(R.string.import_failed_title)
                    .setMessage(err.message ?: err.toString())
                    .setPositiveButton(android.R.string.ok, null).show()
            }.onSuccess { plan ->
                if (plan.events.isEmpty()) {
                    newDialog().setTitle(R.string.import_nothing_title)
                        .setMessage(getString(R.string.import_nothing_message, plan.alreadyImported))
                        .setPositiveButton(android.R.string.ok, null).show()
                } else {
                    writeEvents(plan.events, termStart)
                }
            }
        }
    }

    private fun writeEvents(events: List<CalendarEvent>, termStart: String) {
        lifecycleScope.launch {
            val outcome = withContext(Dispatchers.IO) {
                runCatching {
                    val calendarId = calendarRepo.ensureCalendar()
                    if (calendarId <= 0) {
                        throw IllegalStateException(getString(R.string.no_writable_calendar))
                    }
                    val ids = calendarRepo.insertEvents(calendarId, events)
                    val source = if (binding.chipWebVpn.isChecked) "WebVPN" else "校园网"
                    BuptCore.recordImport(
                        storePath,
                        RecordImportRequest(
                            termStart = termStart,
                            source = source,
                            calendarEventIds = ids,
                            events = events,
                            replaceExisting = false,
                        ),
                    )
                }
            }

            outcome.onFailure { err ->
                newDialog().setTitle(R.string.import_failed_title)
                    .setMessage(err.message ?: err.toString())
                    .setPositiveButton(android.R.string.ok, null).show()
            }.onSuccess { record ->
                // 出错的情况已经在 BuptCore.recordImport 里转成异常了，
                // 走到这里就一定是成功
                newDialog().setTitle(R.string.import_done_title)
                    .setMessage(
                        getString(
                            R.string.import_done_message,
                            record.inserted,
                            record.skipped,
                            BuptCore.importedEventCount(storePath),
                        ),
                    )
                    .setPositiveButton(android.R.string.ok, null)
                    .show()
            }
        }
    }

    // ---------------------------------------------------------------------
    // 清空
    // ---------------------------------------------------------------------

    private fun confirmClearImported() {
        if (!ensureCalendarPermission()) {
            pendingAfterPermission = { confirmClearImported() }
            return
        }
        val count = runCatching { BuptCore.importedEventCount(storePath) }.getOrDefault(0L)
        if (count <= 0L) {
            toast(getString(R.string.nothing_to_clear))
            return
        }
        newDialog()
            .setTitle(R.string.menu_clear)
            .setMessage(getString(R.string.confirm_clear_message, count))
            .setPositiveButton(R.string.action_clear_confirm) { _, _ -> clearImported() }
            .setNegativeButton(android.R.string.cancel, null)
            .show()
    }

    private fun clearImported() {
        lifecycleScope.launch {
            val result = withContext(Dispatchers.IO) {
                runCatching {
                    val refs = BuptCore.listImportedEvents(storePath)
                    val del = calendarRepo.deleteEvents(refs)
                    // 只有全部删干净了才清记录；否则保留记录，用户再点一次可以补齐
                    if (del.failed == 0) {
                        BuptCore.clearRecords(storePath)
                    }
                    del
                }
            }
            result.onFailure { err: Throwable ->
                newDialog().setTitle(R.string.clear_failed_title)
                    .setMessage(err.message ?: err.toString())
                    .setPositiveButton(android.R.string.ok, null).show()
            }.onSuccess { del ->
                val msg = if (del.failed == 0) {
                    getString(R.string.clear_done_message, del.deleted)
                } else {
                    getString(R.string.clear_partial_message, del.deleted, del.failed)
                }
                newDialog().setTitle(R.string.menu_clear).setMessage(msg)
                    .setPositiveButton(android.R.string.ok, null).show()
            }
        }
    }

    private fun showImportStatus() {
        lifecycleScope.launch {
            val text = withContext(Dispatchers.IO) {
                runCatching {
                    val batches = BuptCore.listImportBatches(storePath)
                    val count = BuptCore.importedEventCount(storePath)
                    if (batches.isEmpty()) {
                        getString(R.string.status_empty)
                    } else {
                        buildString {
                            appendLine(getString(R.string.status_total, count))
                            appendLine()
                            batches.take(10).forEach { b ->
                                appendLine(
                                    getString(
                                        R.string.status_line,
                                        formatTime(b.createdAt),
                                        b.termStart,
                                        b.source,
                                        b.eventCount,
                                    ),
                                )
                            }
                        }
                    }
                }.getOrElse { it.message ?: it.toString() }
            }
            newDialog().setTitle(R.string.menu_status).setMessage(text)
                .setPositiveButton(android.R.string.ok, null).show()
        }
    }

    private fun formatTime(epochSeconds: Long): String {
        if (epochSeconds <= 0) return "?"
        val sdf = SimpleDateFormat("yyyy-MM-dd HH:mm", Locale.getDefault())
        return sdf.format(Date(epochSeconds * 1000))
    }

    private fun showHelp() {
        newDialog().setTitle(R.string.menu_help).setMessage(R.string.help_message)
            .setPositiveButton(android.R.string.ok, null).show()
    }

    // ---------------------------------------------------------------------
    // 诊断导出
    // ---------------------------------------------------------------------

    /**
     * 把当前页面的情况导出成一个文件，用于排查「导入失败」。
     *
     * 导入失败的原因往往藏在页面结构里（比如 WebVPN 把页面改写过、
     * 课表在 iframe 里、字段名和预期不同），光看手机屏幕上的提示看不出来。
     * 这里把**原始 HTML** 和一组结构统计一起存下来，发回电脑就能精确复现。
     */
    private fun exportDiagnostics() {
        toast(getString(R.string.exporting))
        binding.webView.evaluateJavascript(TimetableScraperJs.diagnosticScript()) { encoded ->
            val payload = runCatching {
                gson.fromJson(encoded, String::class.java)
            }.getOrElse { "" }

            if (payload.isNullOrBlank() || payload == "null") {
                toast(getString(R.string.export_failed_js))
                return@evaluateJavascript
            }

            val data = runCatching { gson.fromJson(payload, JsonObject::class.java) }.getOrNull()
            if (data == null) {
                toast(getString(R.string.export_failed_js))
                return@evaluateJavascript
            }

            // 顺便跑一遍我们自己的解析，看它到底认得出来什么
            val html = data.get("html")?.asString.orEmpty()
            val frames = data.get("frames")?.asString.orEmpty()
            val documents = buildList {
                if (html.isNotBlank()) add(html)
                if (frames.isNotBlank()) add(frames)
            }
            val parseInfo = if (documents.isEmpty()) {
                "（页面内容为空）"
            } else {
                runCatching {
                    val parsed = BuptCore.parseHtmlDocuments(documents)
                    buildString {
                        appendLine("找到课表表格 : ${parsed.report.foundTable}")
                        appendLine("使用的解析策略: ${parsed.report.strategy}")
                        appendLine("解析出的课程数: ${parsed.entries.size}")
                        if (parsed.report.warnings.isNotEmpty()) {
                            appendLine("解析告警:")
                            parsed.report.warnings.forEach { appendLine("  - $it") }
                        }
                    }
                }.getOrElse { "解析时出错: ${it.message}" }
            }

            val report = buildString {
                appendLine("===== 北邮课表导入 · 诊断报告 =====")
                appendLine("生成时间   : ${formatNow()}")
                appendLine("App 版本   : ${appVersion()}")
                appendLine("入口模式   : ${if (binding.chipWebVpn.isChecked) "WebVPN" else "校园网直连"}")
                appendLine()
                appendLine("----- 页面信息 -----")
                appendLine("URL        : ${data.get("url")?.asString.orEmpty()}")
                appendLine("标题       : ${data.get("title")?.asString.orEmpty()}")
                appendLine("页面字节数 : ${html.length}")
                appendLine()
                appendLine("----- 结构统计 -----")
                appendLine("table 总数        : ${data.get("tables")?.asString.orEmpty()}")
                appendLine("iframe 总数       : ${data.get("iframes")?.asString.orEmpty()}")
                appendLine("#kbtable 存在     : ${data.get("hasKbtable")?.asString.orEmpty()}")
                appendLine("iframe 内有 kbtable: ${data.get("kbtableInIframe")?.asString.orEmpty()}")
                appendLine("取到的框架数       : ${data.get("frameCount")?.asString.orEmpty()}")
                appendLine("框架内容字节数     : ${data.get("frameHtmlLength")?.asString.orEmpty()}")
                appendLine("kbcontent 块数    : ${data.get("kbcontents")?.asString.orEmpty()}")
                appendLine("非空 kbcontent 块 : ${data.get("nonEmptyKb")?.asString.orEmpty()}")
                appendLine("含「周次」的单元格 : ${data.get("weekCells")?.asString.orEmpty()}")
                appendLine("含「节」的文本数   : ${data.get("nodeTexts")?.asString.orEmpty()}")
                appendLine("「开新窗口」链接数 : ${data.get("blankLinks")?.asString.orEmpty()}")
                appendLine("新窗口改写是否生效 : ${data.get("neutralized")?.asString.orEmpty()}")
                appendLine("页面提示的周次     : ${data.get("weekNo")?.asString.orEmpty()}")
                appendLine("body 前 200 字     : ${data.get("bodySnippet")?.asString.orEmpty()}")
                appendLine()
                appendLine("----- 本 App 的解析结果 -----")
                appendLine(parseInfo.trimEnd())
                appendLine()
                appendLine("----- 页面原始 HTML（从下一行开始）-----")
                appendLine(html)
                if (frames.isNotBlank()) {
                    appendLine()
                    appendLine("----- 内嵌框架的 HTML（从下一行开始）-----")
                    appendLine(frames)
                }
            }

            val file = File(getExternalFilesDir(null), "diagnostic-${fileStamp()}.txt")
            runCatching {
                file.parentFile?.mkdirs()
                file.writeText(report)
            }.onFailure {
                toast(getString(R.string.export_failed_write, it.message ?: "?"))
                return@evaluateJavascript
            }

            // 同时放进剪贴板，方便用微信/邮件直接发回去
            val clipboard =
                getSystemService(Context.CLIPBOARD_SERVICE) as? ClipboardManager
            clipboard?.setPrimaryClip(
                ClipData.newPlainText("北邮课表导入诊断", report),
            )

            newDialog()
                .setTitle(R.string.export_done_title)
                .setMessage(getString(R.string.export_done_message, file.absolutePath))
                .setPositiveButton(android.R.string.ok, null)
                .show()
        }
    }

    private fun formatNow(): String =
        SimpleDateFormat("yyyy-MM-dd HH:mm:ss", Locale.US).format(Date())

    /** 把 JS 返回的失败原因码翻成人话。 */
    private fun reasonLabel(reason: String): String = when (reason) {
        "no_table" -> getString(R.string.reason_no_table)
        "table_empty" -> getString(R.string.reason_table_empty)
        "exception" -> getString(R.string.reason_exception)
        "" -> getString(R.string.reason_unknown)
        else -> reason
    }

    private fun appVersion(): String = runCatching {
        packageManager.getPackageInfo(packageName, 0).versionName ?: "?"
    }.getOrDefault("?")

    private fun fileStamp(): String =
        SimpleDateFormat("yyyyMMdd-HHmmss", Locale.US).format(Date())

    // ---------------------------------------------------------------------
    // 权限
    // ---------------------------------------------------------------------

    private fun ensureCalendarPermission(): Boolean {
        if (calendarRepo.hasPermission()) return true
        permissionLauncher.launch(CalendarRepository.REQUIRED_PERMISSIONS)
        return false
    }

    // ---------------------------------------------------------------------
    // 杂项
    // ---------------------------------------------------------------------

    private fun newDialog() = MaterialAlertDialogBuilder(this)

    private fun toast(msg: String) {
        Toast.makeText(this, msg, Toast.LENGTH_LONG).show()
    }

    /** 打开系统设置里的本应用权限页（用户拒绝日历时引导用）。 */
    @Suppress("unused")
    private fun openAppSettings() {
        runCatching {
            startActivity(
                Intent(
                    Settings.ACTION_APPLICATION_DETAILS_SETTINGS,
                    Uri.fromParts("package", packageName, null),
                ),
            )
        }.onFailure { err: Throwable ->
            Log.w(TAG, "打不开应用设置", err)
        }
    }
}
