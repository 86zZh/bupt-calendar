# 本应用当前未启用混淆（isMinifyEnabled = false），这里保留占位。
#
# 将来若开启混淆，必须保留 JNI 入口类 —— 方法名参与生成 JNI 符号，
# 被混淆后 Rust 侧就找不到对应的 Java_com_bupt_calendar_core_BuptCore_* 了。
-keep class com.bupt.calendar.core.BuptCore { *; }

# WebView 的 @JavascriptInterface 方法也是按名字反射调用的，同样要保留
-keepclassmembers class * {
    @android.webkit.JavascriptInterface <methods>;
}
