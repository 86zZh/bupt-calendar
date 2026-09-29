pluginManagement {
    repositories {
        // 优先走国内镜像：Kotlin/AGP 的部分构件在 Maven Central 上会被重定向到
        // GitHub Releases，国内直连经常超时。阿里云 public 聚合了 central+jcenter。
        maven {
            url = uri("https://maven.aliyun.com/repository/public")
            content {
                includeGroupByRegex("com\\.google.*")
                includeGroupByRegex("org\\.jetbrains.*")
                includeGroupByRegex("org\\.ow2.*")
                includeGroupByRegex("com\\.android.*")
            }
        }
        maven { url = uri("https://maven.aliyun.com/repository/gradle-plugin") }
        maven { url = uri("https://maven.aliyun.com/repository/google") }
        google()
        mavenCentral()
        gradlePluginPortal()
    }
}

dependencyResolutionManagement {
    repositoriesMode.set(RepositoriesMode.FAIL_ON_PROJECT_REPOS)
    repositories {
        maven { url = uri("https://maven.aliyun.com/repository/public") }
        maven { url = uri("https://maven.aliyun.com/repository/google") }
        google()
        mavenCentral()
    }
}

rootProject.name = "BUPT课表导入"
include(":app")
