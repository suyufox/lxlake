//! # lxlake
//!
//! 跨平台应用框架，同时具备游戏引擎能力（定位、平台范围、拆包判据见仓库 `docs/`）。
//!
//! 分层落在**模块树**上，不落在 crate 边界上：
//!
//! - [`core`] —— 契约：事件、几何、窗口的数据与 trait。**硬约束：不出现 winit / wgpu 类型**
//! - [`runtime`] —— 事件循环、[`App`](runtime::App)、帧循环、外部事件源泵
//! - `platform` —— 平台后端，按 `cfg(target_os)` 分档；应用代码不该碰它，用 [`runtime::run`] 即可
//! - `render` —— wgpu 渲染管线，全部落在 `render` 特性之后
//!
//! 构建一律按包进行（`cargo build -p lxlake-demo`），不跑 `--workspace`。

pub mod core;
mod platform;
pub mod runtime;

#[cfg(feature = "render")]
pub mod render;

/// 应用入口宏：`#[lxlake::entry]`。把它标在 `main` 上，函数块的值就是应用实例。
pub use lxlake_macros::entry;
