//! 平台后端。
//!
//! 分档用 `cfg(target_os)`，**不用 feature**：Rust 本来就按 target 编译，平台后端天然
//! 互斥，这也避开了「同一 target 上两个后端都自称平台」的歧义（见 `docs/architecture.md`）。
//!
//! 三个桌面平台共用 winit 的事件循环与窗口抽象，故共用 `desktop` 一个后端——差异（如
//! Windows 的 DPI 通知）由 winit 自己吸收。android / ios 将来各自独立后端，按同样的
//! cfg 方式接入。

#[cfg(any(target_os = "windows", target_os = "linux", target_os = "macos"))]
mod desktop;

#[cfg(any(target_os = "windows", target_os = "linux", target_os = "macos"))]
pub(crate) use desktop::run;

#[cfg(not(any(target_os = "windows", target_os = "linux", target_os = "macos")))]
compile_error!("该平台的 platform 后端尚未实现（见 docs/architecture.md 平台范围）");
