# 环境

本页记录**本机实测**的工具链基线与构建前置。路径是某台机器的具体值，换机器要按同口径重新探测。

## 工具链基线

| 工具          | 位置                                                      | 版本                                     | PATH |
| ------------- | --------------------------------------------------------- | ---------------------------------------- | ---- |
| Rust          | `C:\Users\suyufox\.cargo\bin`                             | rustc / cargo 1.98.1                     | ✅   |
| Node.js       | —                                                         | v26.3.1                                  | ✅   |
| pnpm          | —                                                         | 12.4.2                                   | ✅   |
| git           | `C:\Program Files\Git\cmd\git.exe`                        | —                                        | ✅   |
| cmake         | `C:\Users\suyufox\.lxlake\path\Build\4.4.3\bin\cmake.exe` | 4.4.3                                    | ✅   |
| vcpkg         | `C:\Users\suyufox\.lxlake\path\vcpkg\latest\vcpkg.exe`    | —                                        | ✅   |
| Visual Studio | `C:\Program Files\Microsoft Visual Studio\18\Community`   | MSVC 18.10.12201.205                     | ⚠️   |
| LLVM (clang)  | `D:\Path\LLVM\bin\clang.exe`                              | clang 22.1.8（`x86_64-pc-windows-msvc`） | ⚠️   |
| libclang      | `D:\Path\LLVM\bin\libclang.dll`                           | —                                        | —    |
| Android NDK   | `d:\Path\AndroidSdk\ndk\30.0.16248370`                    | 仅 `windows-x86_64` prebuilt             | —    |
| cargo-ndk     | `C:\Users\suyufox\.cargo\bin\cargo-ndk.exe`               | —                                        | ✅   |

⚠️ 的含义：

- **clang 不在 PATH**——**这不阻塞**。vcpkg 默认走 MSVC 工具链，bindgen 只需要 `libclang.dll`。LLVM 是全量发行版，lld 全套（`lld-link.exe` / `llvm-lib.exe` / `llvm-rc.exe`）都在 `D:\Path\LLVM\bin`
- **`cl.exe` 不在 PATH**——需要经 `vcvars64.bat` 或 VS Developer PowerShell 激活 MSVC 环境。纯 Rust 部分不需要

## 环境变量

带 C 依赖（vcpkg 那批）时需要：

```
LIBCLANG_PATH = D:\Path\LLVM\bin
VCPKG_ROOT    = C:\Users\suyufox\.lxlake\path\vcpkg\latest
```

`LIBCLANG_PATH` 是 bindgen 用的。参考项目实测：设好它之后，带 ffmpeg 特性的 `cargo test` 能编译通过并跑绿。

## rustup targets

当前已装 `x86_64-pc-windows-msvc`（宿主）与 `aarch64-linux-android`（android 类型检查用）。
按平台补齐：

```
# windows
rustup target add x86_64-pc-windows-msvc aarch64-pc-windows-msvc

# linux
rustup target add x86_64-unknown-linux-gnu aarch64-unknown-linux-gnu

# android（四 ABI，与 vcpkg 三元组一一对应）
rustup target add aarch64-linux-android armv7-linux-androideabi i686-linux-android x86_64-linux-android
```

android 四 ABI 与 vcpkg 三元组的映射：

| Rust target               | ABI           | vcpkg triplet      |
| ------------------------- | ------------- | ------------------ |
| `aarch64-linux-android`   | `arm64-v8a`   | `arm64-android`    |
| `armv7-linux-androideabi` | `armeabi-v7a` | `arm-neon-android` |
| `i686-linux-android`      | `x86`         | `x86-android`      |
| `x86_64-linux-android`    | `x86_64`      | `x64-android`      |

android 的类型检查**走 cargo-ndk，不裸跑 `cargo check --target`**——后者会让 `cc` 的构建脚本去找
`aarch64-linux-android-clang++`（本机没有），报「找不到链接器」；cargo-ndk 会按 NDK 注入 `CC` / `CXX`。
本机实测可过：

```
$env:ANDROID_NDK_HOME = "d:\Path\AndroidSdk\ndk\30.0.16248370"
cargo ndk -t arm64-v8a check -p lxlake
cargo ndk -t arm64-v8a check -p lxlake-demo --lib   # android 无 bin，打包只取 lib，故加 --lib
```

apple 平台的 target 需要时再加；C 依赖清单不含 apple（见[架构](architecture.md)）。

## webview 覆盖层前置（Windows）

覆盖层走 `webview-wry` 特性。该特性**只在 Windows 目标上成立**（wry 的 Windows 后端 = WebView2）；
其余 target 上这个特性不拉任何依赖，`create_overlay` 在运行期 fail-closed 返回 `Unsupported`——
linux 因此**不需要** webkit2gtk 的系统依赖，android 也不拉东西。

```
# demo 的 Cargo.toml 已把 webview-wry 列进 features，直接跑就带上覆盖层
cargo run -p lxlake-demo

# lxlake 本体单独验证覆盖层
cargo test -p lxlake --features webview-wry
```

运行前置是 **WebView2 Runtime（Evergreen）**：Windows 11 自带，Win10 需装一次官方
Evergreen Bootstrapper。它由机器共享、**不随发行物出 dll**，与 `native/` 下那批 C 依赖不是一回事
（wry 经 `webview2-com` 调系统的 loader）。

## 缺失项

| 项               | 现状                                                                        | 处置                                 |
| ---------------- | --------------------------------------------------------------------------- | ------------------------------------ |
| ninja            | 只在 vcpkg 的下载缓存里（`downloads\tools\ninja-1.13.2-windows`），会被清理 | 补一个正式安装                       |
| ccache / sccache | 均无                                                                        | 建议上 **sccache**，Rust 与 C 都受益 |

## 构建约定

**按包构建，不跑 `--workspace`。**

```
cargo build -p lxlake-editor
cargo build -p lxlake-demo
```

原因：`cargo build --workspace` 会把 `lxlake-demo` 的 `render` 特性统一进 `lxlake-editor`，让「框架主线不依赖渲染」这条约束失去验证意义。按包构建时各自只见自己的依赖图。详见[架构](architecture.md)。
