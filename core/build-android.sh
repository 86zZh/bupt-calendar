#!/usr/bin/env bash
# 交叉编译 Rust 核心到 Android。
#
# 单独抽成脚本（而不是把逻辑写在 Gradle 里）的原因是：
#   1. 可以在 Gradle 之外单独跑，出问题时好定位；
#   2. cc-rs 需要 CC_<target-with-dashes> 这种带横线的环境变量名，
#      Gradle 的 Exec.environment 对这类 key 支持不可靠，shell 里 export 就没事。
#
# 用法: ./build-android.sh <rust-target-triple>
#   ./build-android.sh aarch64-linux-android
#   ./build-android.sh armv7-linux-androideabi
#   ./build-android.sh x86_64-linux-android
set -euo pipefail

TARGET="${1:?用法: $0 <rust-target-triple>}"
shift               # 剩下的参数原样转给 cargo
MIN_SDK="${MIN_SDK:-24}"

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

# NDK 位置：优先用环境变量，其次从 local.properties 的 sdk.dir 推
if [[ -z "${ANDROID_NDK_HOME:-}" ]]; then
  SDK_DIR="${ANDROID_HOME:-${ANDROID_SDK_ROOT:-}}"
  if [[ -z "$SDK_DIR" && -f "$SCRIPT_DIR/../local.properties" ]]; then
    SDK_DIR="$(grep -m1 '^sdk\.dir=' "$SCRIPT_DIR/../local.properties" | cut -d= -f2- | tr -d '[:space:]')"
  fi
  if [[ -z "$SDK_DIR" || ! -d "$SDK_DIR/ndk" ]]; then
    echo "找不到 Android NDK：请设置 ANDROID_HOME 或在 local.properties 里写 sdk.dir" >&2
    exit 1
  fi
  # 取版本号最大的 NDK
  NDK_VERSION="$(ls "$SDK_DIR/ndk" | sort -V | tail -1)"
  ANDROID_NDK_HOME="$SDK_DIR/ndk/$NDK_VERSION"
fi

NDK_BIN="$ANDROID_NDK_HOME/toolchains/llvm/prebuilt/linux-x86_64/bin"
if [[ ! -d "$NDK_BIN" ]]; then
  echo "NDK 工具链目录不存在: $NDK_BIN" >&2
  exit 1
fi

# Rust 目标三元组 -> NDK 的 clang 前缀
case "$TARGET" in
  aarch64-linux-android)      CLANG_PREFIX="aarch64-linux-android" ;;
  armv7-linux-androideabi)    CLANG_PREFIX="armv7a-linux-androideabi" ;;
  x86_64-linux-android)       CLANG_PREFIX="x86_64-linux-android" ;;
  i686-linux-android)         CLANG_PREFIX="i686-linux-android" ;;
  *)
    echo "未支持的 Rust 目标: $TARGET" >&2
    exit 1
    ;;
esac

CC="$NDK_BIN/${CLANG_PREFIX}${MIN_SDK}-clang"
if [[ ! -x "$CC" ]]; then
  echo "找不到编译器: $CC" >&2
  exit 1
fi

# 链接器通过环境变量传给 cargo，因此 .cargo/config.toml 里不用写死本机 NDK 路径
TARGET_ENV="$(echo "$TARGET" | tr 'a-z-' 'A-Z_')"
export "CARGO_TARGET_${TARGET_ENV}_LINKER=$CC"
export "CARGO_TARGET_${TARGET_ENV}_AR=$NDK_BIN/llvm-ar"

# cc-rs（rusqlite 的 bundled SQLite C 代码要用它）默认会去找
# `<target>-clang` 这个**不带 API level** 的名字，而 NDK 只提供带 API 后缀的
# 文件。所以在 PATH 最前面放一层同名包装脚本，转发到真正的编译器。
#
# 注意：不能写成 export CC_aarch64-linux-android=... ——
# bash 不接受带横线的变量名。
WRAP_DIR="$(mktemp -d)"
trap 'rm -rf "$WRAP_DIR"' EXIT
for suffix in clang clang++ gcc g++; do
  wrapper="$WRAP_DIR/${CLANG_PREFIX}-${suffix}"
  cat > "$wrapper" <<EOF
#!/bin/sh
exec "$CC" "\$@"
EOF
  chmod +x "$wrapper"
done

# cc-rs 还会去找 `<target>-ar`
ar_wrapper="$WRAP_DIR/${CLANG_PREFIX}-ar"
cat > "$ar_wrapper" <<EOF
#!/bin/sh
exec "$NDK_BIN/llvm-ar" "\$@"
EOF
chmod +x "$ar_wrapper"

# armv7 有个命名坑：Rust 目标叫 armv7-linux-androideabi，但 cc-rs 会去找
# arm-linux-androideabi-clang（没有 v7a）。这里补一个别名。
if [[ "$TARGET" == "armv7-linux-androideabi" ]]; then
  for suffix in clang clang++ gcc g++ ar; do
    alias_wrapper="$WRAP_DIR/arm-linux-androideabi-${suffix}"
    cat > "$alias_wrapper" <<EOF
#!/bin/sh
exec "$WRAP_DIR/${CLANG_PREFIX}-${suffix}" "\$@"
EOF
    chmod +x "$alias_wrapper"
  done
fi

export PATH="$WRAP_DIR:$PATH"

echo "==> 交叉编译 $TARGET"
echo "    NDK : $ANDROID_NDK_HOME"
echo "    CC  : $CC"

cd "$SCRIPT_DIR"
cargo build --release --target "$TARGET" "$@"

echo "==> 产物: $SCRIPT_DIR/target/$TARGET/release/libbupt_core.so"
