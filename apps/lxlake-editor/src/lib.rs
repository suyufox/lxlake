//! lxlake-editor：框架侧应用的**库**。
//!
//! M0 与 demo 一样只是一个空窗口——它的作用是**验证依赖图**：不启用 `render` 时，
//! `cargo build -p lxlake-editor` 完全不编译 wgpu（见 `docs/roadmap.md` M1 验收第 3 条）。
//!
//! 它同时是「不带渲染也能用 Builder」的样本：只声明窗口，不碰 `render` 特性下的任何东西。
//!
//! 逻辑住在这里、可执行入口住在同包的 `main.rs`（它只调一行 [`run`]）——两个应用同型，
//! Android 才能一视同仁（那边的真实入口是 `android_main`，与 `fn main` 无关）。

use lxlake::core::window::WindowDesc;

/// 编辑器状态。M0 还没有内容，但先按托管状态登记——后续编辑器自己的东西都挂在它上面。
struct Editor;

/// 应用装配：`#[lxlake::entry]` 标在工厂函数上，宏据此产出桌面 `run()` 与 Android
/// `android_main`——两端共用这一份装配。
#[lxlake::entry]
fn app() -> lxlake::Builder {
  lxlake::Builder::new()
    .app_id("com.lxlake.editor")
    .main_window(WindowDesc {
      title: "lxlake editor".to_owned(),
      ..WindowDesc::default()
    })
    .manage(Editor)
}
