//! lxlake-editor：框架侧应用。
//!
//! M0 与 demo 一样只是一个空窗口——它的作用是**验证依赖图**：不启用 `render` 时，
//! `cargo build -p lxlake-editor` 完全不编译 wgpu（见 `docs/roadmap.md` M1 验收第 3 条）。

use lxlake::core::window::WindowDesc;
use lxlake::runtime::App;

struct Editor;

impl App for Editor {
  fn windows(&self) -> Vec<WindowDesc> {
    vec![WindowDesc {
      title: "lxlake editor".to_owned(),
      ..WindowDesc::default()
    }]
  }
}

#[lxlake::entry]
fn main() -> Editor {
  Editor
}
