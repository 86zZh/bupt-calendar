//! JNI 薄壳：把 [`crate::api`] 的 JSON 门面暴露给 Kotlin。
//!
//! 这里只做「Java `String` ↔ Rust `String`」的搬运，不含任何业务逻辑，
//! 目的是让全部可测逻辑都留在纯 Rust 的 [`crate::api`] 里。
//!
//! 对应的 Kotlin 侧声明见 `app/src/main/java/com/bupt/calendar/core/BuptCore.kt`。

use jni::objects::{JClass, JString};
use jni::sys::{jboolean, jlong, jstring};
use jni::JNIEnv;

use crate::api;

/// 把 Rust `String` 转成 Java `String`；失败时返回 `null`。
fn to_java(env: &mut JNIEnv<'_>, s: String) -> jstring {
    match env.new_string(s) {
        Ok(js) => js.into_raw(),
        Err(_) => std::ptr::null_mut(),
    }
}

/// 读取 Java `String` 参数；`null` 或转换失败时返回空串。
fn from_java(env: &mut JNIEnv<'_>, s: &JString<'_>) -> String {
    env.get_string(s).map(|v| v.into()).unwrap_or_default()
}

/// 解析课表 HTML，返回 [`api::parse_html`] 的 JSON。
#[no_mangle]
pub extern "system" fn Java_com_bupt_calendar_core_BuptCore_nativeParseHtml<'local>(
    mut env: JNIEnv<'local>,
    _class: JClass<'local>,
    html: JString<'local>,
) -> jstring {
    let html = from_java(&mut env, &html);
    to_java(&mut env, api::parse_html(&html))
}

/// 解析多个 HTML 文档（主页面 + 内嵌框架），参数是 JSON 字符串数组。
#[no_mangle]
pub extern "system" fn Java_com_bupt_calendar_core_BuptCore_nativeParseHtmlDocuments<'local>(
    mut env: JNIEnv<'local>,
    _class: JClass<'local>,
    docs_json: JString<'local>,
) -> jstring {
    let docs = from_java(&mut env, &docs_json);
    to_java(&mut env, api::parse_html_documents_json(&docs))
}

/// 返回作息表 JSON（`id` 为空时使用北邮本部作息表）。
#[no_mangle]
pub extern "system" fn Java_com_bupt_calendar_core_BuptCore_nativeGetSchedule<'local>(
    mut env: JNIEnv<'local>,
    _class: JClass<'local>,
    schedule_id: JString<'local>,
) -> jstring {
    let id = from_java(&mut env, &schedule_id);
    to_java(&mut env, api::get_schedule(&id))
}

/// 依据当前日期建议学期第一周周一（`YYYY-MM-DD`）。
#[no_mangle]
pub extern "system" fn Java_com_bupt_calendar_core_BuptCore_nativeSuggestTermStart<'local>(
    mut env: JNIEnv<'local>,
    _class: JClass<'local>,
    today: JString<'local>,
) -> jstring {
    let today = from_java(&mut env, &today);
    to_java(&mut env, api::suggest_term_start(&today))
}

/// 生成导入计划（展开日程 + 按记录库去重），返回 JSON。
#[no_mangle]
pub extern "system" fn Java_com_bupt_calendar_core_BuptCore_nativePlanEvents<'local>(
    mut env: JNIEnv<'local>,
    _class: JClass<'local>,
    entries_json: JString<'local>,
    term_start: JString<'local>,
    schedule_id: JString<'local>,
    week_filter_json: JString<'local>,
    store_path: JString<'local>,
) -> jstring {
    let entries = from_java(&mut env, &entries_json);
    let term = from_java(&mut env, &term_start);
    let sid = from_java(&mut env, &schedule_id);
    let filter = from_java(&mut env, &week_filter_json);
    let store = from_java(&mut env, &store_path);
    to_java(
        &mut env,
        api::plan_events(&entries, &term, &sid, &filter, &store),
    )
}

/// 预览日程（不做去重），返回 JSON。
#[no_mangle]
pub extern "system" fn Java_com_bupt_calendar_core_BuptCore_nativePreviewEvents<'local>(
    mut env: JNIEnv<'local>,
    _class: JClass<'local>,
    entries_json: JString<'local>,
    term_start: JString<'local>,
    schedule_id: JString<'local>,
) -> jstring {
    let entries = from_java(&mut env, &entries_json);
    let term = from_java(&mut env, &term_start);
    let sid = from_java(&mut env, &schedule_id);
    to_java(&mut env, api::preview_events(&entries, &term, &sid))
}

/// 记录一次导入（含系统日历事件 id），返回 JSON。
#[no_mangle]
pub extern "system" fn Java_com_bupt_calendar_core_BuptCore_nativeRecordImport<'local>(
    mut env: JNIEnv<'local>,
    _class: JClass<'local>,
    store_path: JString<'local>,
    request_json: JString<'local>,
) -> jstring {
    let store = from_java(&mut env, &store_path);
    let req = from_java(&mut env, &request_json);
    to_java(&mut env, api::record_import(&store, &req))
}

/// 列出本 App 记录过的全部日程（用于一键清空）。
#[no_mangle]
pub extern "system" fn Java_com_bupt_calendar_core_BuptCore_nativeListImportedEvents<'local>(
    mut env: JNIEnv<'local>,
    _class: JClass<'local>,
    store_path: JString<'local>,
) -> jstring {
    let store = from_java(&mut env, &store_path);
    to_java(&mut env, api::list_imported_events(&store))
}

/// 列出历史导入批次。
#[no_mangle]
pub extern "system" fn Java_com_bupt_calendar_core_BuptCore_nativeListImportBatches<'local>(
    mut env: JNIEnv<'local>,
    _class: JClass<'local>,
    store_path: JString<'local>,
) -> jstring {
    let store = from_java(&mut env, &store_path);
    to_java(&mut env, api::list_import_batches(&store))
}

/// 记录库里的日程条数。
#[no_mangle]
pub extern "system" fn Java_com_bupt_calendar_core_BuptCore_nativeImportedEventCount<'local>(
    mut env: JNIEnv<'local>,
    _class: JClass<'local>,
    store_path: JString<'local>,
) -> jlong {
    let store = from_java(&mut env, &store_path);
    api::imported_event_count(&store)
}

/// 清空记录库（系统日历事件需由 Kotlin 侧先删除）。
#[no_mangle]
pub extern "system" fn Java_com_bupt_calendar_core_BuptCore_nativeClearRecords<'local>(
    mut env: JNIEnv<'local>,
    _class: JClass<'local>,
    store_path: JString<'local>,
) -> jboolean {
    let store = from_java(&mut env, &store_path);
    if api::clear_records(&store) {
        1
    } else {
        0
    }
}

/// 按指纹删除记录，返回删除条数。
#[no_mangle]
pub extern "system" fn Java_com_bupt_calendar_core_BuptCore_nativeDeleteRecords<'local>(
    mut env: JNIEnv<'local>,
    _class: JClass<'local>,
    store_path: JString<'local>,
    fingerprints_json: JString<'local>,
) -> jlong {
    let store = from_java(&mut env, &store_path);
    let fps = from_java(&mut env, &fingerprints_json);
    api::delete_records(&store, &fps)
}

/// 回填单条记录对应的系统日历事件 id。
#[no_mangle]
pub extern "system" fn Java_com_bupt_calendar_core_BuptCore_nativeAttachCalendarId<'local>(
    mut env: JNIEnv<'local>,
    _class: JClass<'local>,
    store_path: JString<'local>,
    fingerprint: JString<'local>,
    calendar_id: jlong,
) -> jboolean {
    let store = from_java(&mut env, &store_path);
    let fp = from_java(&mut env, &fingerprint);
    if api::attach_calendar_id(&store, &fp, calendar_id) {
        1
    } else {
        0
    }
}
