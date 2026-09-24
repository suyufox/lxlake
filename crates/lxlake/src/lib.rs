//! # lxlake
//!
//! 跨平台应用框架，同时具备游戏引擎能力（定位、平台范围、拆包判据见仓库 `docs/`）。
//!
//! 分层落在**模块树**上，不落在 crate 边界上：
//!
//! - [`core`] —— 契约：事件、几何、窗口、Widget 的数据与 trait。**硬约束：不出现 winit / wgpu 类型**
//! - [`runtime`] —— 装配（[`Builder`]）、帧循环、[`App`](runtime::App)、外部事件源泵、作业池
//! - `platform` —— 平台后端，按 `cfg(target_os)` 分档；应用代码不该碰它，用 [`runtime::run`] 即可
//! - [`path`] —— 系统目录与应用目录的**唯一判断处**（[`Paths`](path::Paths) + [`BaseDirectory`](path::BaseDirectory)）
//! - [`world`] —— 体素世界：方块注册表、区块、浮岛生成、区块流式
//! - [`meshing`] —— 区块 → 网格（纯 CPU，POD 顶点）
//! - [`camera`] —— 自由飞行相机
//! - [`ui`] —— 自绘 UI：布局、命中测试、文本排版（纯 CPU）
//! - `render` —— wgpu 渲染管线，全部落在 `render` 特性之后
//!
//! `world` / `meshing` / `camera` / `ui` 是引擎侧概念，但都是**纯 CPU**（不含任何 GPU 类型），
//! 因此不加 feature 门——feature 只用来隔离 wgpu 这类重依赖（见 `docs/architecture.md`）。
//!
//! 构建一律按包进行（`cargo build -p lxlake-demo`），不跑 `--workspace`。

pub mod camera;
pub mod core;
pub mod meshing;
pub mod path;
mod platform;
pub mod runtime;
pub mod ui;
pub mod world;

#[cfg(feature = "render")]
pub mod render;

/// 应用装配器：`Builder::new().window(..).manage(..).on_frame(..)`。
pub use runtime::Builder;

/// 应用入口宏族。
///
/// - [`entry`](macro@entry)：标在**工厂函数**上，同时产出桌面 `run()` 与 Android `android_main`；
/// - [`entry_desktop`](macro@entry_desktop) / [`entry_mobile`](macro@entry_mobile)：只要其中一端；
/// - [`desktop`](macro@desktop) / [`mobile`](macro@mobile)：平台 `cfg` 简写，作用在任意条目上。
///
/// 工厂函数无参数、非 `async`，函数块的值就是装配好的 [`Builder`]。
pub use lxlake_macros::{desktop, entry, entry_desktop, entry_mobile, mobile};
