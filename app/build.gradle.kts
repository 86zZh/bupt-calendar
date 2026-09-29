import java.io.ByteArrayOutputStream
import java.util.Properties

plugins {
    id("com.android.application")
    id("org.jetbrains.kotlin.android")
}

// 发布签名：从 keystore.properties 读取。文件不存在时 release 就产出未签名包，
// 不会让构建失败。
val keystorePropsFile = rootProject.file("keystore.properties")
val keystoreProps = Properties().apply {
    if (keystorePropsFile.exists()) {
        keystorePropsFile.inputStream().use { load(it) }
    }
}

android {
    namespace = "com.bupt.calendar"
    compileSdk = 35

    defaultConfig {
        applicationId = "com.bupt.calendar"
        minSdk = 24
        targetSdk = 35
        versionCode = 7
        versionName = "1.0.6"

        ndk {
            // 与 core/.cargo/config.toml 里配置的 Rust 目标保持一致
            abiFilters += listOf("arm64-v8a", "armeabi-v7a", "x86_64")
        }
    }

    signingConfigs {
        if (keystoreProps.getProperty("storeFile") != null) {
            create("release") {
                storeFile = rootProject.file(keystoreProps.getProperty("storeFile"))
                storePassword = keystoreProps.getProperty("storePassword")
                keyAlias = keystoreProps.getProperty("keyAlias")
                keyPassword = keystoreProps.getProperty("keyPassword")
            }
        }
    }

    buildTypes {
        release {
            // 出于可复现构建考虑不启用混淆；需要时把下面改为 true 并补 keep 规则
            isMinifyEnabled = false
            proguardFiles(
                getDefaultProguardFile("proguard-android-optimize.txt"),
                "proguard-rules.pro",
            )
            signingConfig = signingConfigs.findByName("release")
            // 这个 release 是给个人直接装到手机上用的，保留 debuggable 方便抓日志
            isDebuggable = true
        }
    }

    compileOptions {
        sourceCompatibility = JavaVersion.VERSION_17
        targetCompatibility = JavaVersion.VERSION_17
    }

    kotlinOptions {
        jvmTarget = "17"
    }

    buildFeatures {
        viewBinding = true
    }
}

dependencies {
    implementation("androidx.core:core-ktx:1.13.1")
    implementation("androidx.appcompat:appcompat:1.7.0")
    implementation("androidx.activity:activity-ktx:1.9.3")
    implementation("com.google.android.material:material:1.12.0")
    implementation("androidx.constraintlayout:constraintlayout:2.1.4")
    implementation("androidx.lifecycle:lifecycle-runtime-ktx:2.8.7")
    implementation("org.jetbrains.kotlinx:kotlinx-coroutines-android:1.8.1")
    implementation("com.google.code.gson:gson:2.11.0")
}

// ---------------------------------------------------------------------------
// Rust 核心的交叉编译
// ---------------------------------------------------------------------------

/** Rust 目标三元组 -> Android ABI 目录名。 */
val rustTargets = mapOf(
    "aarch64-linux-android" to "arm64-v8a",
    "armv7-linux-androideabi" to "armeabi-v7a",
    "x86_64-linux-android" to "x86_64",
)

/**
 * Rust 工具链位置。
 *
 * 优先用项目目录内自带的 `.rustup` / `.cargo_home`（本机因为 `$HOME` 不可写，
 * 工具链装在项目里，整个目录可以整体搬走）；
 * 如果这两个目录不存在（普通开发者的常规情况），就退回系统安装的
 * `cargo` / `rustup` —— 也就是什么都不用管，只要 `cargo` 在 PATH 里。
 */
val bundledRustupHome = File(rootProject.projectDir, ".rustup").takeIf { it.isDirectory }
val bundledCargoHome = File(rootProject.projectDir, ".cargo_home")
    .takeIf { File(it, "bin/cargo").exists() }

// NDK 的定位与 CC/AR/链接器设置都在 core/build-android.sh 里完成，
// 那个脚本可以脱离 Gradle 单独运行，便于排错。

val rustBuildAll by tasks.registering {
    group = "rust"
    description = "把所有 ABI 的 Rust 核心编译成 .so 并放进 jniLibs"
}

rustTargets.forEach { (target, abi) ->
    val taskName = "rustBuild" + abi.split("-").joinToString("") { it.replaceFirstChar(Char::uppercase) }
    val outDir = layout.buildDirectory.dir("rustJniLibs/$abi").get().asFile

    val task = tasks.register<Exec>(taskName) {
        group = "rust"
        description = "交叉编译 Rust 核心 ($target -> $abi)"
        val coreDir = rootProject.file("core")

        inputs.dir(File(coreDir, "src"))
        inputs.file(File(coreDir, "Cargo.toml"))
        inputs.file(File(coreDir, ".cargo/config.toml"))
        inputs.file(File(coreDir, "build-android.sh"))
        outputs.file(File(outDir, "libbupt_core.so"))

        workingDir = coreDir
        // 具体的 CC/AR/链接器设置都在脚本里完成 —— 这里只管把工具链位置传进去。
        // 脚本可以在 Gradle 之外单独运行，出问题时更容易定位。
        commandLine("bash", File(coreDir, "build-android.sh").absolutePath, target)

        // 自带工具链存在时才覆盖这两个变量，否则让脚本用系统的 cargo
        bundledRustupHome?.let { environment("RUSTUP_HOME", it.absolutePath) }
        bundledCargoHome?.let { environment("CARGO_HOME", it.absolutePath) }
        environment("ANDROID_HOME", System.getenv("ANDROID_HOME") ?: "")
        environment("ANDROID_SDK_ROOT", System.getenv("ANDROID_SDK_ROOT") ?: "")
        val cargoBin = bundledCargoHome?.let { File(it, "bin").absolutePath }
        environment("PATH", listOfNotNull(cargoBin, System.getenv("PATH")).joinToString(":"))

        doLast {
            val so = File(coreDir, "target/$target/release/libbupt_core.so")
            if (!so.exists()) {
                throw GradleException("Rust 构建没有产出 $so，请检查上面的 cargo 输出")
            }
            outDir.mkdirs()
            so.copyTo(File(outDir, "libbupt_core.so"), overwrite = true)
            logger.lifecycle("Rust 核心已就绪: $abi -> ${File(outDir, "libbupt_core.so").length()} bytes")
        }
    }
    rustBuildAll.configure { dependsOn(task) }
}

// 把 Rust 产物并进 jniLibs
android.sourceSets.getByName("main") {
    jniLibs.srcDir(layout.buildDirectory.dir("rustJniLibs"))
}

tasks.named("preBuild") {
    dependsOn(rustBuildAll)
}
