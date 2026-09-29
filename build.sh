#!/usr/bin/env bash
# 一键构建 APK。
#
# 本项目是**自包含**的：Rust 工具链、Android SDK/NDK、Gradle 全部放在项目目录内
# （因为本机 $HOME 不可写，用不了 ~/.rustup 之类的默认位置）。
# 所以这个脚本不依赖工作区里其他目录，整个文件夹可以直接搬走。
#
# 用法：
#   ./build.sh            # 构建 debug APK
#   ./build.sh release    # 构建 release APK
#   ./build.sh test       # 只跑 Rust 核心测试
#   ./build.sh clean      # 清理构建产物
set -euo pipefail

PROJECT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
cd "$PROJECT_DIR"

# ---------------------------------------------------------------------------
# 工具链位置
#
# 两种使用方式都支持：
#   A. 项目目录里自带工具链（本机就是这种：$HOME 不可写，所以 SDK/NDK/Rust/Gradle
#      都装在项目内），此时下面这些路径都存在，直接用。
#   B. 用你系统里已经装好的工具链（普通开发者的常规情况）——
#      只要 cargo / gradle / Android SDK 在 PATH 或环境变量里即可，
#      也可以用下面的环境变量显式指定。
# ---------------------------------------------------------------------------
: "${RUSTUP_HOME:=$PROJECT_DIR/.rustup}"
: "${CARGO_HOME:=$PROJECT_DIR/.cargo_home}"
: "${ANDROID_HOME:=$PROJECT_DIR/android-sdk}"
: "${ANDROID_USER_HOME:=$PROJECT_DIR/.android}"
: "${GRADLE_USER_HOME:=$PROJECT_DIR/.gradle}"

# 项目内自带的才 export；否则保留系统默认（用 ~/.cargo、~/.gradle 等）
if [ -d "$RUSTUP_HOME" ]; then export RUSTUP_HOME; fi
if [ -x "$CARGO_HOME/bin/cargo" ]; then
  export CARGO_HOME
  export PATH="$CARGO_HOME/bin:$PATH"
fi
if [ -d "$ANDROID_HOME" ]; then
  export ANDROID_HOME
  export ANDROID_USER_HOME
fi
if [ -d "$GRADLE_USER_HOME" ]; then export GRADLE_USER_HOME; fi

# JDK：优先用环境变量；没设就试常见位置，都没有就让 gradle 自己找
if [ -z "${JAVA_HOME:-}" ]; then
  for j in /usr/lib/jvm/java-21-openjdk /usr/lib/jvm/java-17-openjdk \
           /usr/lib/jvm/default-java; do
    if [ -x "$j/bin/java" ]; then
      export JAVA_HOME="$j"
      break
    fi
  done
fi
if [ -n "${JAVA_HOME:-}" ]; then export PATH="$JAVA_HOME/bin:$PATH"; fi

# Gradle：项目内自带的 → 否则 gradlew → 否则 PATH 里的 gradle
if [ -n "${GRADLE_BIN:-}" ]; then
  :
elif [ -x "$PROJECT_DIR/tools/gradle/bin/gradle" ]; then
  GRADLE_BIN="$PROJECT_DIR/tools/gradle/bin/gradle"
elif [ -x "$PROJECT_DIR/gradlew" ]; then
  GRADLE_BIN="$PROJECT_DIR/gradlew"
elif command -v gradle >/dev/null 2>&1; then
  GRADLE_BIN="$(command -v gradle)"
else
  echo "找不到 Gradle。" >&2
  echo "请任选一种：安装 Gradle 并加进 PATH，或用 GRADLE_BIN=/path/to/gradle 指定。" >&2
  exit 1
fi

if ! command -v cargo >/dev/null 2>&1; then
  echo "找不到 cargo（Rust）。请先安装 Rust：https://rustup.rs" >&2
  exit 1
fi

case "${1:-debug}" in
  test)
    echo "==> 检查注入 WebView 的 JS 脚本（语法 + 关键逻辑）"
    if command -v node >/dev/null 2>&1; then
      node "$PROJECT_DIR/tools/check-scraper-js.mjs"
    else
      echo "  跳过：没装 node（可选，装了就会自动检查）"
    fi
    echo
    echo "==> 运行 Rust 核心测试"
    (cd "$PROJECT_DIR/core" && cargo test)
    ;;
  release)
    echo "==> 构建 release APK"
    "$GRADLE_BIN" -p "$PROJECT_DIR" assembleRelease
    ;;
  debug)
    echo "==> 构建 debug APK"
    "$GRADLE_BIN" -p "$PROJECT_DIR" assembleDebug
    ;;
  clean)
    echo "==> 清理构建产物"
    "$GRADLE_BIN" -p "$PROJECT_DIR" clean || true
    rm -rf "$PROJECT_DIR/core/target"
    ;;
  *)
    echo "用法: $0 [debug|release|test|clean]" >&2
    exit 2
    ;;
esac

echo
# clean 之后 APK/ 里的成品仍然保留，所以这里先看它，别让人以为东西没了
if [ "${1:-debug}" = "clean" ]; then
  echo "==> 安装包（APK/ 目录，未被清理，可直接安装）："
  if compgen -G "$PROJECT_DIR/APK/*.apk" > /dev/null; then
    for f in "$PROJECT_DIR/APK"/*.apk; do
      printf '  %s (%s)\n' "$f" "$(du -h "$f" | cut -f1)"
    done
  else
    echo "  APK/ 目录是空的，请先运行 ./build.sh release 生成安装包"
  fi
  exit 0
fi

echo "==> 产物："
found=0
for d in "$PROJECT_DIR/app/build/outputs/apk/release" "$PROJECT_DIR/app/build/outputs/apk/debug"; do
  [ -d "$d" ] || continue
  while IFS= read -r f; do
    [ -n "$f" ] || continue
    printf '  %s (%s)\n' "$f" "$(du -h "$f" | cut -f1)"
    found=1
  done < <(find "$d" -name '*.apk' 2>/dev/null)
done
[ "$found" -eq 1 ] || echo "  （没有找到 APK，请检查上面的构建输出）"

# 同步一份到项目根部的 APK/ 目录，让「交付包」始终保持最新
if [ "$found" -eq 1 ]; then
  mkdir -p "$PROJECT_DIR/APK"
  for d in "$PROJECT_DIR/app/build/outputs/apk/release" "$PROJECT_DIR/app/build/outputs/apk/debug"; do
    [ -d "$d" ] || continue
    find "$d" -name '*.apk' -exec cp -f {} "$PROJECT_DIR/APK/" \; 2>/dev/null || true
  done
  echo "  已同步到 $PROJECT_DIR/APK/"
fi
