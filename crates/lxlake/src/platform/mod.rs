//! 平台后端。
//!
//! 分档用 `cfg(target_os)`，**不用 feature**：Rust 本来就按 target 编译，平台后端天然
//! 互斥，这也避开了「同一 target 上两个后端都自称平台」的歧义（见 `docs/architecture.md`）。
//!
//! 桌面三平台与 android 共用 winit 的事件循环与窗口抽象，故共用 `winit` 一个后端——差异
//! （Windows 的 DPI 通知、android 的 activity 生命周期与 surface 销毁）由 winit 自己吸收，
//! 本层只在 `resumed` / `suspended` 两处把 android 的语义补上。ios 将来独立接入，方式相同。

#[cfg(any(
  target_os = "windows",
  target_os = "linux",
  target_os = "macos",
  target_os = "android"
))]
mod winit;

#[cfg(any(target_os = "windows", target_os = "linux", target_os = "macos"))]
pub(crate) use self::winit::run;

/// android 入口：多一个由系统递进来的 `AndroidApp`（winit 再导出，见 `platform::winit`）。
#[cfg(target_os = "android")]
pub(crate) use self::winit::run_android;

/// 由 crate 根再导出给应用（`#[lxlake::entry]` 生成的 `android_main` 用它收参数）。
#[cfg(target_os = "android")]
pub use self::winit::AndroidApp;

#[cfg(not(any(
  target_os = "windows",
  target_os = "linux",
  target_os = "macos",
  target_os = "android"
)))]
compile_error!(
  "该平台的 platform 后端尚未实现（ios 排在实装队列后，见 docs/architecture.md 平台范围）"
);
