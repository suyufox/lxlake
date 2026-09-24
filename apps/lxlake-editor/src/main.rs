//! lxlake-editor：框架侧应用。
//!
//! M0 与 demo 一样只是一个空窗口——它的作用是**验证依赖图**：不启用 `render` 时，
//! `cargo build -p lxlake-editor` 完全不编译 wgpu（见 `docs/roadmap.md` M1 验收第 3 条）。
//!
//! 它同时是「不带渲染也能用 Builder」的样本：只声明窗口，不碰 `render` 特性下的任何东西。

use lxlake::core::window::WindowDesc;

/// 编辑器状态。M0 还没有内容，但先按托管状态登记——后续编辑器自己的东西都挂在它上面。
struct Editor;

/// 应用入口：装配层。
#[lxlake::entry]
fn main() -> lxlake::Builder {
  lxlake::Builder::new()
    .window(WindowDesc {
      title: "lxlake editor".to_owned(),
      ..WindowDesc::default()
    })
    .manage(Editor)
}
