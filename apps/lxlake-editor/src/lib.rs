//! lxlake-editor：框架侧应用的**库**。
//!
//! 它是**框架主线**的样本：只开 `ui-render` 档，自绘 UI 需要设备与方片管线，但**不编 3D**
//! （`pipeline.rs` / `shader.wgsl` 不进编译单元，见 `docs/roadmap.md` M1 验收第 3 条）。
//!
//! 编辑器第一刀走**文档驱动保留模式**：唯一真相源是文档，`instantiate` 出的 `UiTree` 只是它
//! 这一刻的投影，选中 / 悬停这类临时状态放在文档之外。本片（片 1）只把骨架与 2D 帧循环立起来——
//! 树里先摆一块写死的左栏矩形，把「建树 → 摆位 → 出方片 → 出画」这条链路跑通；文档层与
//! 结构树 / 属性面板随后各占一片（见 `.trae/documents/slice0-render-tiers-slice1-editor.md`）。
//!
//! 逻辑住在这里、可执行入口住在同包的 `main.rs`（它只调一行 [`run`]）——两个应用同型，
//! Android 才能一视同仁（那边的真实入口是 `android_main`，与 `fn main` 无关）。

use lxlake::core::event::Event;
use lxlake::core::geometry::{LogicalPosition, LogicalSize};
use lxlake::core::widget::{Anchor, UiId, Widget};
use lxlake::core::window::WindowDesc;
use lxlake::render::Renderer;
use lxlake::runtime::{App, AppContext, Capabilities, Frame};
use lxlake::ui::{Quad, UiTree};

/// UI 字体（相对工作区根，发行物按工作目录读资产）。
const UI_FONT: &str = "data/fonts/NotoSansSC-Regular.otf";

/// 左栏：结构树将来住这儿。
const SIDEBAR_ID: UiId = UiId(1);

/// 左栏尺寸（逻辑像素）。片 1 写死；片 3 起随文档与层级算。
const SIDEBAR_SIZE: LogicalSize = LogicalSize::new(240.0, 1000.0);

/// 左栏底色（sRGB）。
const SIDEBAR_COLOR: [u8; 4] = [26, 28, 34, 255];

/// 编辑器状态。
///
/// `tree` 每帧重建——**文档才是唯一真相源**，树不留真相；`cursor` 留在这里是因为命中测试要它，
/// 而它只能从 `CursorMoved` 一路攒（见 [`Event::CursorMoved`]）。
struct Editor {
  tree: UiTree,
  cursor: LogicalPosition,
  /// 方片缓冲：每帧清空重填。留成字段只为**不在帧里分配**。
  quads: Vec<Quad>,
}

impl Default for Editor {
  fn default() -> Self {
    Self::new()
  }
}

impl Editor {
  fn new() -> Self {
    Self {
      tree: UiTree::new(),
      cursor: LogicalPosition::new(0.0, 0.0),
      quads: Vec::new(),
    }
  }

  /// 主窗口的逻辑视口。窗口还没建好时给一个空视口——那时本来也没东西可摆。
  fn viewport(cx: &AppContext) -> LogicalSize {
    cx.main_window()
      .map_or(LogicalSize::new(0.0, 0.0), |window| {
        let handle = window.handle();
        handle.size().to_logical(handle.scale_factor())
      })
  }

  fn handle_event(&mut self, cx: &mut AppContext, event: &Event) {
    match event {
      Event::CloseRequested { .. } => cx.exit(),
      // 位置只在命中测试里用（片 3 起），攒着供点击那一下取。
      Event::CursorMoved { position, .. } => self.cursor = *position,
      // resize 与 DPI 变化不在这里接：它们先落到该窗的渲染器上（表面重配 + 换算比例），
      // 这是每个应用都要写一遍的样板，已经收进装配层的内建转发（见 `Builder`）。
      _ => {}
    }
  }

  /// 一帧：建树 → 摆位 → 出方片 → 出画。
  ///
  /// 渲染器由调用方从能力里递进来（见 [`Capabilities`]），本方法只管它的用法。
  fn frame(&mut self, cx: &mut AppContext, capabilities: Capabilities<'_>) {
    let viewport = Self::viewport(cx);
    // 没有渲染器这一帧就无事可做（装配层 `renderer` 没配，或主窗口还没就绪）。
    let Some(renderer) = capabilities.gpu else {
      return;
    };

    // 每帧重建：先清空再按文档摆——片 1 还没有文档，先摆一块写死的左栏。
    self.tree.clear();
    self
      .tree
      .add(Widget::new(SIDEBAR_ID, Anchor::TopLeft, SIDEBAR_SIZE));
    self.tree.layout(viewport);

    self.quads.clear();
    if let Some(rect) = self.tree.rect_of(SIDEBAR_ID) {
      self.quads.push(Quad::solid(rect, SIDEBAR_COLOR));
    }

    if let Err(error) = renderer.draw_ui(&self.quads) {
      eprintln!("lxlake editor：渲染失败：{error}");
      cx.exit();
    }
  }
}

/// 生命周期：装配层（[`lxlake::Builder`]）把钩子交给下面这几个自由函数，每个钩子从托管状态里
/// 取出 [`Editor`]，再交给它自己的方法。
fn on_event(app: &mut App, event: &Event) {
  app.with_state::<Editor, _>(|editor, cx| editor.handle_event(cx, event));
}

/// 片 1 用不上帧时间（没有动画与模拟），参数先按 `Frame` 接上，将来要 `delta` 直接改名。
fn on_frame(app: &mut App, _frame: Frame) {
  app.with_capabilities::<Editor, _>(|editor, capabilities, cx| editor.frame(cx, capabilities));
}

/// 应用装配：`#[lxlake::entry]` 标在工厂函数上，宏据此产出桌面 `run()` 与 Android
/// `android_main`——两端共用这一份装配。
///
/// `renderer` 这里是**一参数**那一支（`ui-render` 档的 [`Renderer::new`]）：编辑器没有 3D，
/// 也就没有图集要传。
#[lxlake::entry]
fn app() -> lxlake::Builder {
  lxlake::Builder::new()
    .app_id("com.lxlake.editor")
    .main_window(WindowDesc {
      title: "lxlake editor".to_owned(),
      size: LogicalSize::new(1280.0, 720.0),
      ..WindowDesc::default()
    })
    .font_path(UI_FONT)
    .renderer(Renderer::new)
    .manage(Editor::new())
    .on_event(on_event)
    .on_frame(on_frame)
}
