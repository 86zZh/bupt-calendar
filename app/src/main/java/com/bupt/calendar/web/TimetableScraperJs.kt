package com.bupt.calendar.web

/**
 * 注入到内置 WebView 里的课表抓取脚本。
 *
 * # 为什么要「先展开课表」
 *
 * 强智教务的课表页默认可能是**折叠**的：未选中的课表模式只渲染一个空表格，
 * 或者课程区被 `style="display:none"` 隐藏。直接取 HTML 会拿到空课表，
 * 所以脚本会先尝试点「展开」「显示全部」之类的按钮并移除隐藏样式，
 * 轮询等到表格里出现 `[title*="周次"]`（强智用 `title="周次(节次)"` 标记
 * 周次节次）后才把 HTML 交回原生侧。
 *
 * # 为什么取多个候选表格
 *
 * 页面里可能存在多个课表（当前周 / 全学期、主修 / 辅修）。脚本会按
 * 「含周次信息的单元格数量」排序，优先交回最像个人课表的那一个，
 * 同时把其余候选一并带上，方便排查选错表的情况。
 */
object TimetableScraperJs {

    /** JS 接口名：`window.BuptBridge.onExtractResult(json)`。 */
    const val BRIDGE_NAME = "BuptBridge"

    /** 入口函数名：调用 `BuptProbe.run()` 即可触发抓取。 */
    const val FUNC_NAME = "BuptProbe"

    /**
     * 构建注入脚本。
     *
     * @param maxRounds 最多尝试展开并轮询多少轮（每轮 500ms）
     */
    fun script(maxRounds: Int = 14): String = """
(function () {
  var MAX_ROUNDS = $maxRounds;
  var BRIDGE = '$BRIDGE_NAME';
  var rounds = 0;
  var expandClicked = false;

  function emit(payload) {
    try {
      var s = JSON.stringify(payload);
      if (window[BRIDGE] && typeof window[BRIDGE].onExtractResult === 'function') {
        window[BRIDGE].onExtractResult(s);
      }
    } catch (e) {
      // 序列化失败时至少让原生侧知道失败了
      try {
        if (window[BRIDGE] && typeof window[BRIDGE].onExtractResult === 'function') {
          window[BRIDGE].onExtractResult('{"error":"\\u5e8f\\u5217\\u5316\\u5931\\u8d25: ' + String(e) + '"}');
        }
      } catch (e2) { /* 无能为力 */ }
    }
  }

  /* ---------- 工具 ---------- */

  function norm(s) { return (s || '').replace(/\s+/g, ' ').trim(); }
  function visible(el) {
    if (!el) return false;
    var st = window.getComputedStyle(el);
    if (!st || st.display === 'none' || st.visibility === 'hidden') return false;
    return el.offsetWidth > 0 || el.offsetHeight > 0;
  }

  /**
   * 表格是否是「课表」：必须有单元格带 title 含「周次」，
   * 或者单元格文本里出现形如「1-16周」/「1-16(周)」的周次描述。
   */
  function looksLikeTimetable(t) {
    var titles = t.querySelectorAll('[title*="\u5468\u6b21"], [title*="\u8282\u6b21"]');
    if (titles.length > 0) return true;
    var cells = t.querySelectorAll('td');
    for (var i = 0; i < cells.length; i++) {
      var txt = norm(cells[i].textContent);
      if (txt.length > 400) continue;
      if (/\d+\s*-\s*\d+\s*[\u5468(（]/.test(txt)) return true;
    }
    return false;
  }

  /**
   * 找出页面上所有像课表的表格，按「周次单元格数」降序排序。
   *
   * 搜索范围包含**主文档 + 所有可访问的内嵌框架**，因为课表可能在任何一层。
   * 每个文档独立 try/catch，跨域框架不会影响主文档的搜索。
   */
  function findTables() {
    var roots = [document];
    try {
      var frames = [];
      frames = frames.concat(Array.prototype.slice.call(document.getElementsByTagName('iframe')));
      frames = frames.concat(Array.prototype.slice.call(document.getElementsByTagName('frame')));
      for (var f = 0; f < frames.length; f++) {
        try {
          if (frames[f].contentDocument) roots.push(frames[f].contentDocument);
        } catch (e) { /* 跨域受限，忽略 */ }
      }
    } catch (e) { /* 忽略 */ }

    var cands = [];
    for (var r = 0; r < roots.length; r++) {
      var all;
      try { all = roots[r].querySelectorAll('table'); } catch (e) { continue; }
      for (var i = 0; i < all.length; i++) {
        var t = all[i];
        if (!looksLikeTimetable(t)) continue;
        var score = t.querySelectorAll('[title*="\u5468\u6b21"]').length;
        if (score === 0) {
          // 退而求其次：用含“周”且含数字区间的单元格数量当分数
          var cells = t.querySelectorAll('td');
          for (var j = 0; j < cells.length; j++) {
            if (/\d+\s*-\s*\d+\s*[\u5468(（]/.test(norm(cells[j].textContent))) score++;
          }
        }
        cands.push({ el: t, score: score, id: t.id || '' });
      }
    }
    cands.sort(function (a, b) { return b.score - a.score; });
    return cands;
  }

  /** 尝试点击「展开 / 显示全部 / 查询」之类的控件，或解除隐藏。 */
  function tryExpand() {
    var keywords = ['\u5c55\u5f00', '\u663e\u793a\u5168\u90e8', '\u67e5\u8be2', '\u5237\u65b0'];
    var candidates = document.querySelectorAll('a, button, input[type="button"], input[type="submit"], span');
    var clicked = [];
    for (var i = 0; i < candidates.length; i++) {
      var el = candidates[i];
      var label = norm(el.textContent || el.value || '');
      if (!label || label.length > 20) continue;
      for (var k = 0; k < keywords.length; k++) {
        if (label.indexOf(keywords[k]) === 0 || label === keywords[k]) {
          // 只点可见的、且在课表区域附近的控件，避免误点「退出」之类
          if (visible(el)) {
            try {
              el.click();
              clicked.push(label);
            } catch (e) { /* 忽略 */ }
          }
          break;
        }
      }
      if (clicked.length >= 2) break;
    }
    // 把 display:none 的课程块放出来
    var hidden = document.querySelectorAll('#kbtable [style*="display:none"], #kbtable [style*="display: none"], .kbcontent[style*="none"]');
    for (var h = 0; h < hidden.length; h++) {
      try { hidden[h].style.display = ''; } catch (e) { /* 忽略 */ }
    }
    return clicked;
  }

  function pageInfo() {
    var termText = '';
    var body = document.body ? norm(document.body.innerText).slice(0, 4000) : '';
    var m = body.match(/(\d{4}-\d{4}\s*\u5b66\u5e74\s*\u7b2c?\s*\d+\s*\u5b66\u671f[^\n]{0,20})/);
    if (m) termText = m[1];
    if (!termText) {
      var sel = document.querySelector('.xnxq, #xnxq01id, select[name="xnxq01id"], select#xnxq01id');
      if (sel) termText = norm(sel.value || sel.textContent || '');
    }
    var weekText = '';
    var mw = body.match(/\u7b2c\s*(\d+)\s*\u5468/);
    if (mw) weekText = mw[0];
    return {
      url: location.href,
      title: document.title || '',
      term: termText,
      weekText: weekText,
      weekNo: mw ? parseInt(mw[1], 10) : null
    };
  }

  /* ---------- 主流程 ---------- */

  function run() {
    var cands = findTables();
    var hasGood = cands.length > 0 && cands[0].score > 0;

    if (!hasGood && rounds < MAX_ROUNDS) {
      if (!expandClicked) {
        var clicked = tryExpand();
        expandClicked = true;
        if (clicked.length > 0) {
          rounds++;
          setTimeout(run, 700);
          return;
        }
      } else {
        // 已经点过，等页面刷新出表格
        rounds++;
        setTimeout(run, 500);
        return;
      }
    }

    var info = pageInfo();
    if (cands.length === 0) {
      emit({
        ok: false,
        reason: 'no_table',
        message: '\u6ca1\u6709\u5728\u5f53\u524d\u9875\u9762\u627e\u5230\u8bfe\u8868\u8868\u683c\u3002\u8bf7\u5148\u767b\u5f55\u6559\u52a1\u7cfb\u7edf\uff0c\u5e76\u6253\u5f00\u300c\u6211\u7684\u8bfe\u8868 / \u8bfe\u8868\u67e5\u8be2\u300d\u9875\u9762\u3002',
        page: info
      });
      return;
    }

    if (!hasGood) {
      emit({
        ok: false,
        reason: 'table_empty',
        message: '\u627e\u5230\u4e86\u8bfe\u8868\u8868\u683c\uff0c\u4f46\u91cc\u9762\u6ca1\u6709\u8bfe\u7a0b\u6570\u636e\u3002\u8bf7\u786e\u8ba4\uff1a1) \u5df2\u9009\u4e2d\u6b63\u786e\u7684\u5b66\u671f\uff1b2) \u70b9\u51fb\u4e86\u300c\u5c55\u5f00\u8bfe\u8868\u300d\uff1b3) \u5f53\u524d\u8d26\u53f7\u786e\u5b9e\u6709\u8bfe\u3002',
        page: info
      });
      return;
    }

    emit({
      ok: true,
      html: cands[0].el.outerHTML,
      tablesFound: cands.length,
      cellCount: cands[0].el.querySelectorAll('td').length,
      frames: collectFrameHtml().html,
      page: info
    });
  }

  /**
   * 收集页面里所有**内嵌框架**的 HTML，拼成一大段交回原生侧。
   *
   * 为什么必须做这件事：北邮教务的课表经常是显示在 iframe / frame 里的，
   * 主文档本身并没有课表。如果只取主文档，就会出现「人明明在课表页上，
   * 程序却说找不到课表」的情况。
   *
   * 参考实现 WakeupSchedule 正是这么做的：
   *   for (i...) iframeContent += ifrs[i].contentDocument.body.parentElement.outerHTML
   *
   * 比它多做的两点加固：
   *   1. 每一帧单独 try/catch —— 跨域框架访问 contentDocument 会抛异常，
   *      不能因为一个取不到就把整段脚本打断；
   *   2. 记住已经取过的 document，避免同一份内容被重复拼接
   *      （nested frame 可能同时出现在 iframe 和 frame 两个列表里）。
   */
  function collectFrameHtml() {
    var docs = {};
    var html = '';
    var count = 0;

    function take(doc) {
      if (!doc) return;
      var key = doc.location && doc.location.href ? doc.location.href : String(Math.random());
      if (docs[key]) return;
      docs[key] = true;
      try {
        var root = doc.documentElement;
        if (root) {
          html += root.outerHTML;
          count++;
        }
      } catch (e) { /* 跨域或已卸载，跳过 */ }
    }

    var frames = [];
    try { frames = frames.concat(Array.prototype.slice.call(document.getElementsByTagName('iframe'))); } catch (e) {}
    try { frames = frames.concat(Array.prototype.slice.call(document.getElementsByTagName('frame'))); } catch (e) {}

    for (var i = 0; i < frames.length; i++) {
      try { take(frames[i].contentDocument); } catch (e) { /* 跨域受限 */ }
    }
    return { html: html, count: count };
  }

  try {
    run();
  } catch (e) {
    emit({ ok: false, reason: 'exception', message: String(e) });
  }
})();
""".trimIndent()

    /**
     * 只做「当前页面是否已有可取课表」的探测，不点击任何控件。
     * 用于页面加载完成后决定是否把悬浮按钮点亮。
     */
    fun probeScript(): String = """
(function () {
  try {
    var tables = document.querySelectorAll('table');
    var found = 0;
    for (var i = 0; i < tables.length; i++) {
      if (tables[i].querySelectorAll('[title*="\u5468\u6b21"]').length > 0) found++;
    }
    var body = document.body ? document.body.innerText : '';
    var m = body.match(/\u7b2c\s*(\d+)\s*\u5468/);
    var payload = {
      hasTable: found > 0,
      tables: found,
      weekNo: m ? parseInt(m[1], 10) : null,
      url: location.href
    };
    if (window['$BRIDGE_NAME'] && typeof window['$BRIDGE_NAME'].onProbeResult === 'function') {
      window['$BRIDGE_NAME'].onProbeResult(JSON.stringify(payload));
    }
  } catch (e) { /* 忽略 */ }
})();
""".trimIndent()

    /**
     * 把页面里所有「开新窗口/新标签」的链接改写成「在当前窗口打开」。
     *
     * 为什么需要：北邮教务（特别是经 WebVPN 时）的「我的课表」是
     * `target="_blank"` 链接。点下去如果真开了新窗口，那个新窗口里的内容
     * 抓取脚本就看不见了 —— 用户人在课表页上，我们却以为他没到课表页。
     *
     * Kotlin 侧已经用 `onCreateWindow` 拦了一道，但有些页面是用
     * JavaScript 里写死 `window.open` 或动态生成链接的，这里再兜一层：
     * 把已经存在于 DOM 里的链接的 target 改掉，并监听后续新增的节点。
     */
    fun neutralizeNewWindowScript(): String = """
(function () {
  if (window.__buptNeutralized) return;
  window.__buptNeutralized = true;

  function patch(root) {
    try {
      var links = root.querySelectorAll ? root.querySelectorAll('a[target]') : [];
      for (var i = 0; i < links.length; i++) {
        var t = (links[i].getAttribute('target') || '').toLowerCase();
        if (t === '_blank' || t === '_new' || t === 'blank') {
          links[i].setAttribute('target', '_self');
        }
      }
    } catch (e) { /* 忽略 */ }
  }

  patch(document);

  // 页面可能是异步渲染的，后续新增的链接也要改
  try {
    var mo = new MutationObserver(function (records) {
      for (var i = 0; i < records.length; i++) {
        var added = records[i].addedNodes;
        for (var j = 0; j < added.length; j++) {
          if (added[j].nodeType === 1) patch(added[j].parentNode || added[j]);
        }
      }
    });
    mo.observe(document.documentElement, { childList: true, subtree: true });
  } catch (e) { /* 忽略 */ }
})();
""".trimIndent()

    /**
     * 诊断脚本：把当前页面的**原始 HTML** 与一组结构统计打包回原生侧。
     *
     * 「导入失败」的原因通常藏在页面结构里，而手机屏幕上看不出来。
     * 这个脚本不去猜，直接把事实取回来：
     * * 页面 URL / 标题 / 正文开头
     * * table、iframe、`#kbtable`、`kbcontent` 的数量
     * * **iframe 内部**是否藏着课表（WebVPN 或某些教务版本会用框架布局）
     * * 含「周次」「节」的单元格数量
     *
     * 返回一个 JSON 字符串（而非对象），由 Kotlin 侧直接写文件。
     */
    fun diagnosticScript(): String = """
(function () {
  function countWeekTitles(root) {
    try { return root.querySelectorAll('[title*="\u5468\u6b21"]').length; } catch (e) { return 0; }
  }
  function norm(s) { return (s || '').replace(/\s+/g, ' ').trim(); }

  try {
    var doc = document;
    var body = doc.body || doc.documentElement;

    // 页面本体统计
    var tables = body.querySelectorAll('table');
    var kb = body.querySelectorAll('.kbcontent, .kbcontent1');
    var kbNonEmpty = 0;
    for (var i = 0; i < kb.length; i++) {
      if (norm(kb[i].textContent).length > 0) kbNonEmpty++;
    }
    var weekCells = countWeekTitles(body);
    var hasKbtable = !!body.querySelector('#kbtable');

    // 同一域名下的 iframe 也要看 —— 课表很可能在里面
    var iframes = doc.querySelectorAll('iframe');
    var kbtableInIframe = false;
    var iframeWeekCells = 0;
    var iframeNote = '';
    for (var f = 0; f < iframes.length; f++) {
      try {
        var idoc = iframes[f].contentDocument;
        if (!idoc) { iframeNote += '[无法访问] '; continue; }
        if (idoc.querySelector('#kbtable')) kbtableInIframe = true;
        iframeWeekCells += countWeekTitles(idoc);
      } catch (e) {
        iframeNote += '[跨域受限] ';
      }
    }

    // 含「节」的文本片段数量，用来判断节次信息在不在
    var nodeTexts = 0;
    var all = body.querySelectorAll('font, span, div');
    for (var n = 0; n < all.length; n++) {
      var t = norm(all[n].textContent);
      if (t.length > 0 && t.length < 60 && t.indexOf('\u8282') >= 0) nodeTexts++;
    }

    var text = body.innerText || '';
    var m = text.match(/\u7b2c\s*(\d+)\s*\u5468/);

    // 有多少链接是「开新窗口」的 —— 如果很多，说明页面习惯用新窗口，
    // 那种情况下课表很可能跳到了我们看不见的地方
    var blankLinks = 0;
    try {
      var as = body.querySelectorAll('a[target]');
      for (var a = 0; a < as.length; a++) {
        var tv = (as[a].getAttribute('target') || '').toLowerCase();
        if (tv === '_blank' || tv === '_new') blankLinks++;
      }
    } catch (e) { /* 忽略 */ }

    // 收集各层框架的 HTML —— 诊断时必须带上，否则会误判成「没有课表」
    var frameHtml = '';
    var frameCount = 0;
    var frames = [];
    try { frames = frames.concat(Array.prototype.slice.call(doc.getElementsByTagName('iframe'))); } catch (e) {}
    try { frames = frames.concat(Array.prototype.slice.call(doc.getElementsByTagName('frame'))); } catch (e) {}
    var seen = {};
    for (var fi = 0; fi < frames.length; fi++) {
      try {
        var fdoc = frames[fi].contentDocument;
        if (!fdoc) continue;
        var key = (fdoc.location && fdoc.location.href) ? fdoc.location.href : String(fi);
        if (seen[key]) continue;
        seen[key] = true;
        var froot = fdoc.documentElement;
        if (froot) { frameHtml += froot.outerHTML; frameCount++; }
      } catch (e) { /* 跨域受限 */ }
    }

    var html = '';
    try { html = doc.documentElement.outerHTML; } catch (e) { html = '<!-- 取 HTML 失败: ' + e + ' -->'; }

    return JSON.stringify({
      url: location.href,
      title: doc.title || '',
      tables: String(tables.length),
      iframes: String(iframes.length) + (iframeNote ? ' ' + iframeNote : ''),
      hasKbtable: String(hasKbtable),
      kbtableInIframe: String(kbtableInIframe) + ' (\u5468\u6b21\u5355\u5143\u683c ' + iframeWeekCells + ')',
      kbcontents: String(kb.length),
      nonEmptyKb: String(kbNonEmpty),
      weekCells: String(weekCells),
      nodeTexts: String(nodeTexts),
      blankLinks: String(blankLinks),
      neutralized: String(!!window.__buptNeutralized),
      frameCount: String(frameCount),
      frameHtmlLength: String(frameHtml.length),
      weekNo: m ? m[1] : '',
      bodySnippet: norm(text).slice(0, 200),
      html: html,
      frames: frameHtml
    });
  } catch (e) {
    return JSON.stringify({ url: location.href, title: '', html: '', bodySnippet: '\u8bca\u65ad\u811a\u672c\u51fa\u9519: ' + String(e) });
  }
})();
""".trimIndent()
}
