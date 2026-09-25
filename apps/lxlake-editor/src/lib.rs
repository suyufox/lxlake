//! lxlake-editor：框架侧应用的**库**。
//!
//! 它是**框架主线**的样本：只开 `ui-render` 档，自绘 UI 需要设备与方片管线，但**不编 3D**
//! （`pipeline.rs` / `shader.wgsl` 不进编译单元，见 `docs/roadmap.md` M1 验收第 3 条）。
//!
//! 编辑器走**文档驱动保留模式**：唯一真相源是 [`Document`]，[`instantiate`] 出的 [`UiTree`] 只是
//! 它这一刻的投影；选中 / 悬停这类临时状态放在文档之外（见 [`EditorState`]）。当前是第四片——
//! 左栏结构树点选、预览区描边、右栏 Inspector 改属性 ⇒ 下一帧投影出新矩形（实时预览）。
//! 全程见 `.trae/documents/slice0-render-tiers-slice1-editor.md`。
//!
//! **三棵树**是刻意的：每块区域要摆进**不同的矩形**，而 [`UiTree::layout_in`] 一次只认一个，
//! 于是只能按区域分开——`chrome`（左栏结构树）、`inspector`（右栏属性面板）、`preview`
//! （文档投影）。三棵都不留真相：每帧重建，真相在文档里。分开还带来一个好处——三份 id 空间
//! 天然不重叠，命中之后不必拿位段去猜「这条 id 属于谁」（见 [`Hit`]）。
//!
//! 逻辑住在这里、可执行入口住在同包的 `main.rs`（它只调一行 [`run`]）——两个应用同型，
//! Android 才能一视同仁（那边的真实入口是 `android_main`，与 `fn main` 无关）。

use lxlake::core::event::Event;
use lxlake::core::geometry::{LogicalPosition, LogicalRect, LogicalSize};
use lxlake::core::input::MouseButton;
use lxlake::core::widget::{Anchor, UiId, Widget};
use lxlake::core::window::WindowDesc;
use lxlake::render::Renderer;
use lxlake::runtime::{App, AppContext, Capabilities, Frame};
use lxlake::ui::{
  DocNode, Document, NODE_PROPS, NodeId, PropDesc, PropKind, PropValue, Quad, TextShaper,
  TextStyle, UiTree, instantiate,
};

/// UI 字体（相对工作区根，发行物按工作目录读资产）。
const UI_FONT: &str = "data/fonts/NotoSansSC-Regular.otf";

/// 左栏（结构树）宽度（逻辑像素）。
const SIDEBAR_WIDTH: f64 = 240.0;

/// 右栏（Inspector）宽度。
const INSPECTOR_WIDTH: f64 = 280.0;

/// 面板内边距。
const PANEL_PADDING: f64 = 12.0;

/// 结构树一行的高度。
///
/// **行矩形只由它和行号算出来，不量文字**：量文字要排版器，而几何不该依赖字体读没读进来
/// （见 `relayout`）。文字是画在行上的装饰，行本身是纯算术。
const ROW_HEIGHT: f64 = 28.0;

/// 结构树文字的样式：字号 14、行高 18。
const ROW_STYLE: TextStyle = TextStyle::new(14.0, 18.0);

/// 行内文字左内边距；以及垂直居中量（行高 28 − 行盒 18，上下各 5）。
const ROW_TEXT_INSET: f64 = 10.0;
const ROW_TEXT_TOP: f64 = 5.0;

/// Inspector 里标签列的宽度：值从这条线往右写。
const LABEL_WIDTH: f64 = 76.0;

/// 控件尺寸。按钮上下居中于行内（行高 28 − 按钮 20，上下各 4）。
const BUTTON_HEIGHT: f64 = 20.0;
const BUTTON_WIDTH: f64 = 26.0;
/// 枚举按钮要放得下候选名，所以宽一些。
const ENUM_BUTTON_WIDTH: f64 = 96.0;
const BUTTON_GAP: f64 = 4.0;
/// 按钮上的字的内边距（按钮 20 高、字行盒 18 ⇒ 上下各 1）。
const BUTTON_TEXT_INSET: f64 = 7.0;
const BUTTON_TEXT_TOP: f64 = 1.0;

/// 预览区四周留白。
const PREVIEW_MARGIN: f64 = 24.0;

/// 选中描边的线宽。
const STROKE_WIDTH: f64 = 2.0;

/// 行 id 与预览 id 的分野：**翻转最高位**（双射，不会把两个节点映到同一个 id 上）。
///
/// 两棵树里的 id 本来也不冲突，分开是为了让命中到的 id 一眼能看出「这是行还是节点」。
const ROW_ID_BIT: u32 = 0x8000_0000;

/// 左栏底色（sRGB）。
const SIDEBAR_COLOR: [u8; 4] = [26, 28, 34, 255];
/// 结构树一行的底色（普通 / 悬停 / 选中）。
const ROW_COLOR: [u8; 4] = [32, 35, 42, 255];
const ROW_HOVERED_COLOR: [u8; 4] = [44, 49, 60, 255];
const ROW_SELECTED_COLOR: [u8; 4] = [56, 96, 160, 255];
/// 结构树文字、Inspector 的值与标签、按钮底色。
const ROW_TEXT_COLOR: [u8; 4] = [214, 218, 226, 255];
const VALUE_COLOR: [u8; 4] = [214, 218, 226, 255];
const LABEL_COLOR: [u8; 4] = [148, 156, 170, 255];
const BUTTON_COLOR: [u8; 4] = [56, 62, 76, 255];
/// Inspector 面板底色（与左栏同色：两翼是一套）。
const PANEL_COLOR: [u8; 4] = [26, 28, 34, 255];
/// 预览区底色、节点底色与选中描边。
const PREVIEW_COLOR: [u8; 4] = [18, 20, 24, 255];
const NODE_COLOR: [u8; 4] = [46, 50, 62, 255];
const STROKE_COLOR: [u8; 4] = [120, 190, 255, 255];

/// Inspector 控件上的动作。第一刀只要这三种（见 `PropKind`）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Action {
  /// 减一个步长。
  Decrease,
  /// 加一个步长。
  Increase,
  /// 枚举走到下一个候选。
  Next,
}

impl Action {
  /// 打包进 [`UiId`] 用的编码（见 [`control_id`]）。
  fn code(self) -> u32 {
    match self {
      Self::Decrease => 0,
      Self::Increase => 1,
      Self::Next => 2,
    }
  }

  /// [`Action::code`] 的反函数。
  fn from_code(code: u32) -> Option<Self> {
    match code {
      0 => Some(Self::Decrease),
      1 => Some(Self::Increase),
      2 => Some(Self::Next),
      _ => None,
    }
  }
}

/// Inspector 上被点中的控件。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Control {
  /// `NODE_PROPS` 里的序号。
  prop: usize,
  action: Action,
}

/// 命中的是什么。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Hit {
  /// 一个节点：结构树的行，或预览区里的方块。
  Node(NodeId),
  /// Inspector 上的一个控件。
  Control(Control),
}

/// 文档之外的编辑器**临时状态**。
///
/// 文档只管「有什么」，这里管「这一刻在看哪个」。它不进文档——否则选中态会被存进 `.lxml`，
/// 「改属性」与「改选中」两条完全不同的路径也挤在同一份数据上（见 `ui::doc` 的三条边界）。
#[derive(Debug, Default, Clone, Copy)]
struct EditorState {
  /// 选中的节点：结构树点一下换它，预览区据此描边，右栏 Inspector 据此出行。
  selected: Option<NodeId>,
  /// 光标下的节点：只影响结构树那一行的高亮。
  hovered: Option<NodeId>,
}

/// 编辑器状态。
struct Editor {
  /// **唯一真相源**。
  document: Document,
  /// 左栏结构树的树。每帧重建。
  chrome: UiTree,
  /// 右栏 Inspector 的控件。每帧重建。
  inspector: UiTree,
  /// 文档本帧的投影（`instantiate` 进预览区）。每帧重建。
  preview: UiTree,
  /// 选中 / 悬停。
  state: EditorState,
  /// 光标位置：命中测试要它，只能从 [`Event::CursorMoved`] 一路攒。
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
      document: sample_document(),
      chrome: UiTree::new(),
      inspector: UiTree::new(),
      preview: UiTree::new(),
      state: EditorState::default(),
      cursor: LogicalPosition::new(0.0, 0.0),
      quads: Vec::new(),
    }
  }

  /// 主窗口的逻辑视口与换算比例。窗口还没建好时给「空视口 + 1.0」——那时本来也没东西可摆
  /// （与 demo 同口径）。
  fn window_metrics(cx: &AppContext) -> (LogicalSize, f64) {
    cx.main_window()
      .map_or((LogicalSize::new(0.0, 0.0), 1.0), |window| {
        let handle = window.handle();
        let scale_factor = handle.scale_factor();
        (handle.size().to_logical(scale_factor), scale_factor)
      })
  }

  fn handle_event(&mut self, cx: &mut AppContext, event: &Event) {
    match event {
      Event::CloseRequested { .. } => cx.exit(),
      // 位置只在命中测试里用，攒着供 `MouseButton` 那一下取。
      Event::CursorMoved { position, .. } => self.cursor = *position,
      // 命中闸口在 `click` 里：点在 UI 上才算 UI 点击，其余不处理（编辑器现在没有「世界」，
      // 但预览区将来的拖拽要靠这条区分「改文档」与「动视图」，同 demo `ui_owns_click`）。
      // 只认按下：按住左键的自动重复不该反复改选中态或反复步进。
      Event::MouseButton {
        button: MouseButton::Left,
        pressed: true,
        ..
      } => self.click(),
      // resize 与 DPI 变化不在这里接：它们先落到该窗的渲染器上（表面重配 + 换算比例），
      // 这是每个应用都要写一遍的样板，已经收进装配层的内建转发（见 `Builder`）。
      _ => {}
    }
  }

  /// 左键按下：先过命中闸口，再按命中到的对象改选中态或改文档。
  fn click(&mut self) {
    match self.hit(self.cursor) {
      Some(Hit::Node(node)) => self.state.selected = Some(node),
      Some(Hit::Control(control)) => self.apply_control(control),
      None => {}
    }
  }

  /// 命中：三棵树各问一遍。
  fn hit(&self, point: LogicalPosition) -> Option<Hit> {
    if let Some(id) = self.chrome.hit_test(point) {
      return self.node_of(id).map(Hit::Node);
    }
    if let Some(id) = self.inspector.hit_test(point) {
      return control_of(id).map(Hit::Control);
    }
    self
      .preview
      .hit_test(point)
      .and_then(|id| self.node_of(id))
      .map(Hit::Node)
  }

  /// 命中 id → 节点身份。**行与预览区都认**：点左栏第 N 行与点预览区里那个方块，选中的是同一个
  /// 节点。
  ///
  /// 反查而不是存一张表：节点数是百量级，一张表要跟着文档同步，反而多一个会走样的地方。
  fn node_of(&self, id: UiId) -> Option<NodeId> {
    self
      .document
      .nodes()
      .iter()
      .map(DocNode::id)
      .find(|node_id| node_id.ui_id() == id || row_id(*node_id) == id)
  }

  /// 应用一次控件动作：改的是**文档**里的值。
  ///
  /// 下一帧 [`instantiate`] 出新矩形，实时预览由此成立——不走「直接改树上的矩形」那条路，
  /// 否则文档与画面就有两个真相源。
  fn apply_control(&mut self, control: Control) {
    // 控件只在有选中时进树（见 `relayout_inspector`），所以这里取不到就是树与状态走样了。
    let Some(selected) = self.state.selected else {
      return;
    };
    let Some(desc) = NODE_PROPS.get(control.prop) else {
      return;
    };
    let Some(node) = self.document.node_mut(selected) else {
      return;
    };

    // 缺项按表里的默认值起步——与 `instantiate`、面板显示读的是同一个数（见 `ui::doc`）。
    let current = match node.prop(desc.key) {
      Some(value) => value.clone(),
      None => desc.default.clone(),
    };
    if let Some(next) = next_value(&desc.kind, control.action, &current) {
      node.set(desc.key, next);
    }
  }

  /// 布局：按当前视口把三棵树重算出来。
  ///
  /// **不碰文字**（不借排版器）：几何与字形分开，几何因此能在没有字体的测试里跑。
  fn relayout(&mut self, viewport: LogicalSize) {
    // 预览：文档投影进**算出来的**预览区。文档里因此永远不含窗口尺寸（见 `ui::doc`）。
    self.preview = instantiate(&self.document, preview_area(viewport));

    // 左栏结构树：一行一个节点，行 id 由节点身份派生（跨帧稳定，选中态才比得上）。
    self.chrome.clear();
    let rows = visible_rows(viewport.height);
    for (index, node) in self.document.nodes().iter().take(rows).enumerate() {
      self.chrome.add(
        Widget::new(
          row_id(node.id()),
          Anchor::TopLeft,
          LogicalSize::new(SIDEBAR_WIDTH, ROW_HEIGHT),
        )
        .offset([0.0, index as f64 * ROW_HEIGHT]),
      );
    }
    self.chrome.layout(viewport);

    // 悬停放在布局之后算：命中测试读的是刚算出来的矩形。只认 chrome——预览区里划过去不该让
    // 左栏某一行亮起来。
    self.state.hovered = self
      .chrome
      .hit_test(self.cursor)
      .and_then(|id| self.node_of(id));

    self.relayout_inspector(viewport);
  }

  /// 右栏 Inspector：按**选中的**节点把控件摆出来。
  ///
  /// 没选中就没有内容（底色仍在，见 `push_inspector`）：控件不进树，点它也自然没反应。
  fn relayout_inspector(&mut self, viewport: LogicalSize) {
    self.inspector.clear();
    if self.state.selected.is_none() {
      return;
    }

    let area = inspector_area(viewport);
    for (index, desc) in NODE_PROPS.iter().enumerate() {
      let row = inspector_row(area, index);
      // 按钮从右往左排：最后一个动作贴右边缘，前面的依次往左让位。
      let mut right = row.x + row.width;
      for action in prop_actions(&desc.kind) {
        let width = button_width(*action);
        right -= width;
        self.inspector.add(
          Widget::new(
            control_id(index, *action),
            Anchor::TopLeft,
            LogicalSize::new(width, BUTTON_HEIGHT),
          )
          .offset([right, row.y + (ROW_HEIGHT - BUTTON_HEIGHT) / 2.0]),
        );
        right -= BUTTON_GAP;
      }
    }
    self.inspector.layout(viewport);
  }

  /// 左栏：底色 + 逐行底色。
  fn push_sidebar(&mut self, viewport: LogicalSize) {
    self.quads.push(Quad::solid(
      LogicalRect::new(0.0, 0.0, SIDEBAR_WIDTH, viewport.height),
      SIDEBAR_COLOR,
    ));

    for node in self.document.nodes() {
      // 装不下的行不在树里（见 `visible_rows`），也就不该出图。
      let Some(rect) = self.chrome.rect_of(row_id(node.id())) else {
        continue;
      };
      let color = if self.state.selected == Some(node.id()) {
        ROW_SELECTED_COLOR
      } else if self.state.hovered == Some(node.id()) {
        ROW_HOVERED_COLOR
      } else {
        ROW_COLOR
      };
      self.quads.push(Quad::solid(rect, color));
    }
  }

  /// 右栏：底色 + 每个控件的底色。
  fn push_inspector(&mut self, viewport: LogicalSize) {
    self
      .quads
      .push(Quad::solid(inspector_area(viewport), PANEL_COLOR));

    // 控件在不在树里、摆哪儿，只有 `relayout_inspector` 说——这里只问树要矩形。
    for (index, desc) in NODE_PROPS.iter().enumerate() {
      for action in prop_actions(&desc.kind) {
        if let Some(rect) = self.inspector.rect_of(control_id(index, *action)) {
          self.quads.push(Quad::solid(rect, BUTTON_COLOR));
        }
      }
    }
  }

  /// 预览区：底色 + 每个节点一块底色 + 选中节点的描边。
  fn push_preview(&mut self, viewport: LogicalSize) {
    self
      .quads
      .push(Quad::solid(preview_area(viewport), PREVIEW_COLOR));

    // 节点底色：文档里「有什么」要看得见，才有对象可选。
    for node in self.document.nodes() {
      if let Some(rect) = self.preview.rect_of(node.id().ui_id()) {
        self.quads.push(Quad::solid(rect, NODE_COLOR));
      }
    }

    if let Some(stroke) = self.selection_stroke() {
      self.quads.extend(stroke);
    }
  }

  /// 选中节点的描边；没选中、或选中的节点不在预览树里都是 `None`。
  ///
  /// 单独成方法是为了让「描边落在哪一个矩形上」可以直接断言（见 tests）。
  fn selection_stroke(&self) -> Option<[Quad; 4]> {
    let rect = self.preview.rect_of(self.state.selected?.ui_id())?;
    Some(stroke_quads(rect))
  }

  /// 全部文字：左栏每行一行字、右栏表头与逐条标签 / 值 / 按钮上的字。
  fn push_text(&mut self, shaper: &mut TextShaper, scale_factor: f64, viewport: LogicalSize) {
    for node in self.document.nodes() {
      let Some(rect) = self.chrome.rect_of(row_id(node.id())) else {
        continue;
      };
      let label = row_label(node);
      self.quads.extend(shaper.layout(
        &label,
        ROW_STYLE,
        LogicalPosition::new(rect.x + ROW_TEXT_INSET, rect.y + ROW_TEXT_TOP),
        ROW_TEXT_COLOR,
        scale_factor,
      ));
    }

    let area = inspector_area(viewport);
    let selected = self.state.selected;
    let Some(node) = selected.and_then(|id| self.document.node_by_id(id)) else {
      return;
    };

    // 表头：选中节点是谁（与结构树那一行同一份文字，改口径只用改一处）。
    let header = inspector_header(area);
    let header_label = row_label(node);
    self.quads.extend(shaper.layout(
      &header_label,
      ROW_STYLE,
      LogicalPosition::new(header.x, header.y + ROW_TEXT_TOP),
      VALUE_COLOR,
      scale_factor,
    ));

    for (index, desc) in NODE_PROPS.iter().enumerate() {
      let row = inspector_row(area, index);
      let value = displayed_value(node, desc);
      // 标签在左，值在标签列之后。
      self.quads.extend(shaper.layout(
        desc.label,
        ROW_STYLE,
        LogicalPosition::new(row.x, row.y + ROW_TEXT_TOP),
        LABEL_COLOR,
        scale_factor,
      ));
      self.quads.extend(shaper.layout(
        &value,
        ROW_STYLE,
        LogicalPosition::new(row.x + LABEL_WIDTH, row.y + ROW_TEXT_TOP),
        VALUE_COLOR,
        scale_factor,
      ));

      for action in prop_actions(&desc.kind) {
        let Some(rect) = self.inspector.rect_of(control_id(index, *action)) else {
          continue;
        };
        let label = button_label(*action, &value);
        self.quads.extend(shaper.layout(
          &label,
          ROW_STYLE,
          LogicalPosition::new(rect.x + BUTTON_TEXT_INSET, rect.y + BUTTON_TEXT_TOP),
          ROW_TEXT_COLOR,
          scale_factor,
        ));
      }
    }
  }

  /// 一帧：布局 → 建方片 → 出画。
  ///
  /// 渲染器与排版器由调用方从能力里递进来（见 [`Capabilities`]），本方法只管它们的用法。
  fn frame(&mut self, cx: &mut AppContext, capabilities: Capabilities<'_>) {
    let (viewport, scale_factor) = Self::window_metrics(cx);
    let Capabilities { mut text, gpu, .. } = capabilities;
    // 没有渲染器这一帧就无事可做（装配层 `renderer` 没配，或主窗口还没就绪）。
    let Some(renderer) = gpu else {
      return;
    };

    self.relayout(viewport);

    // 出图的顺序就是层序：底色 → 面板 → 描边 → 文字。
    self.quads.clear();
    self.push_sidebar(viewport);
    self.push_inspector(viewport);
    self.push_preview(viewport);
    if let Some(shaper) = text.as_deref_mut() {
      self.push_text(shaper, scale_factor, viewport);
    }

    // 本帧新光栅化的字形**先**补进图集、再出画——否则第一帧的字是空的（见 `ui::text`）。
    let glyphs = text.map(TextShaper::take_new_glyphs).unwrap_or_default();
    renderer.upload_glyphs(&glyphs);
    if let Err(error) = renderer.draw_ui(&self.quads) {
      eprintln!("lxlake editor：渲染失败：{error}");
      cx.exit();
    }
  }
}

/// 右栏区域：贴着视口右边，占满高度。
fn inspector_area(viewport: LogicalSize) -> LogicalRect {
  LogicalRect::new(
    viewport.width - INSPECTOR_WIDTH,
    0.0,
    INSPECTOR_WIDTH,
    viewport.height,
  )
}

/// Inspector 表头那一行（第 0 行）。
fn inspector_header(area: LogicalRect) -> LogicalRect {
  LogicalRect::new(
    area.x + PANEL_PADDING,
    area.y + PANEL_PADDING,
    area.width - PANEL_PADDING * 2.0,
    ROW_HEIGHT,
  )
}

/// Inspector 第 `index` 条属性的行矩形（表头之后）。
fn inspector_row(area: LogicalRect, index: usize) -> LogicalRect {
  let header = inspector_header(area);
  LogicalRect::new(
    header.x,
    header.y + (index as f64 + 1.0) * ROW_HEIGHT,
    header.width,
    ROW_HEIGHT,
  )
}

/// 预览区：视口挖掉左右两栏与四周留白。为负就夹到 0（窗口不够宽时不该出逆向矩形）。
fn preview_area(viewport: LogicalSize) -> LogicalRect {
  LogicalRect::new(
    SIDEBAR_WIDTH + PREVIEW_MARGIN,
    PREVIEW_MARGIN,
    (viewport.width - SIDEBAR_WIDTH - INSPECTOR_WIDTH - PREVIEW_MARGIN * 2.0).max(0.0),
    (viewport.height - PREVIEW_MARGIN * 2.0).max(0.0),
  )
}

/// 左栏装得下的行数。
///
/// **必须承认的缺口**：`Widget` 没有裁剪，多摆的行会画到面板外。第一刀只摆装得下的这些，
/// 裁剪与滚动留给切片 3（层级容器那一刀）。
fn visible_rows(height: f64) -> usize {
  (height / ROW_HEIGHT).floor().max(0.0) as usize
}

/// 结构树一行的 `UiId`（见 [`ROW_ID_BIT`]）。
fn row_id(id: NodeId) -> UiId {
  UiId(id.ui_id().0 ^ ROW_ID_BIT)
}

/// Inspector 控件的 `UiId`：`属性序号 << 4 | 动作编码`。
///
/// 控件住在自己的树里（`Editor::inspector`），所以这个 id 空间与行 id、预览 id 天然分开，
/// 不必再拿位段去和它们挤。序号上限由 `NODE_PROPS` 的长度定，四位动作码够用（见 [`Action`]）。
fn control_id(prop: usize, action: Action) -> UiId {
  UiId(((prop as u32) << 4) | action.code())
}

/// [`control_id`] 的反函数：命中到的 id 是哪条属性的哪个动作。
fn control_of(id: UiId) -> Option<Control> {
  let prop = (id.0 >> 4) as usize;
  let action = Action::from_code(id.0 & 0xF)?;
  // 序号越界说明这条 id 根本不是控件。
  NODE_PROPS.get(prop)?;
  Some(Control { prop, action })
}

/// 一条属性要哪几个按钮。`Text` 第一刀只读，所以一个都没有（不碰文本输入与 IME）。
fn prop_actions(kind: &PropKind) -> &'static [Action] {
  match kind {
    PropKind::Number { .. } => &[Action::Decrease, Action::Increase],
    PropKind::Enum(_) => &[Action::Next],
    PropKind::Text => &[],
  }
}

/// 按钮宽度：枚举那个要放得下候选名。
fn button_width(action: Action) -> f64 {
  match action {
    Action::Decrease | Action::Increase => BUTTON_WIDTH,
    Action::Next => ENUM_BUTTON_WIDTH,
  }
}

/// 按控件算下一个值。
///
/// 类型对不上（手写文档可能出现）就给 `None`——**不 panic**：文档是外部输入，这里不是校验器
/// （见 `ui::doc`）。
fn next_value(kind: &PropKind, action: Action, current: &PropValue) -> Option<PropValue> {
  match (kind, action) {
    (PropKind::Number { min, step, .. }, Action::Decrease) => match current {
      PropValue::Number(value) => Some(PropValue::Number((value - step).max(*min))),
      _ => None,
    },
    (PropKind::Number { max, step, .. }, Action::Increase) => match current {
      PropValue::Number(value) => Some(PropValue::Number((value + step).min(*max))),
      _ => None,
    },
    (PropKind::Enum(names), Action::Next) => match current {
      PropValue::Variant(index) if !names.is_empty() => {
        Some(PropValue::Variant((index + 1) % names.len()))
      }
      _ => None,
    },
    _ => None,
  }
}

/// 一条属性当前的显示值。缺项取表里的默认值——与 `instantiate` 读的是同一个数（见 `ui::doc`）。
fn displayed_value(node: &DocNode, desc: &PropDesc) -> String {
  match node.prop(desc.key) {
    Some(value) => prop_text(&desc.kind, value),
    None => prop_text(&desc.kind, &desc.default),
  }
}

/// 值 → 文字。枚举取候选名，名字表里没有就退回序号（坏下标也可能来自手写文档）。
fn prop_text(kind: &PropKind, value: &PropValue) -> String {
  match value {
    PropValue::Number(number) => number.to_string(),
    PropValue::Variant(index) => match kind {
      PropKind::Enum(names) => names
        .get(*index)
        .map_or_else(|| index.to_string(), |name| (*name).to_owned()),
      _ => index.to_string(),
    },
    PropValue::Text(text) => text.clone(),
  }
}

/// 按钮上的字。枚举按钮写的是**当前值**——点它就走到了下一个。
fn button_label(action: Action, value: &str) -> String {
  match action {
    Action::Decrease => "-".to_owned(),
    Action::Increase => "+".to_owned(),
    Action::Next => value.to_owned(),
  }
}

/// 结构树一行的文字（Inspector 表头也用同一份口径）。
fn row_label(node: &DocNode) -> String {
  match node.prop("name") {
    Some(PropValue::Text(name)) if !name.is_empty() => format!("{} {name}", node.kind()),
    _ => node.kind().to_owned(),
  }
}

/// 一个矩形的 4 条描边：上下横跨整个宽度、左右横跨整个高度，拼起来正是它的外框。
///
/// 自绘的唯一出图形式是方片（见 `ui::draw`），没有描边原语，于是描边就是 4 条细方片。
fn stroke_quads(rect: LogicalRect) -> [Quad; 4] {
  let t = STROKE_WIDTH;
  [
    Quad::solid(
      LogicalRect::new(rect.x, rect.y, rect.width, t),
      STROKE_COLOR,
    ),
    Quad::solid(
      LogicalRect::new(rect.x, rect.y + rect.height - t, rect.width, t),
      STROKE_COLOR,
    ),
    Quad::solid(
      LogicalRect::new(rect.x, rect.y, t, rect.height),
      STROKE_COLOR,
    ),
    Quad::solid(
      LogicalRect::new(rect.x + rect.width - t, rect.y, t, rect.height),
      STROKE_COLOR,
    ),
  ]
}

/// 第一刀的样例文档：**程序构造**（`.lxml` 的读写是片 5 之后的一刀，见计划文件）。
///
/// 三个节点：正中一块大面板、左上与右下各一块小面板。前两块给了 `key`——身份与位置无关，
/// 将来在它们前面插兄弟也不会让选中跳到别的节点上（见 `ui::doc` 的 [`NodeId`]）。
fn sample_document() -> Document {
  let mut document = Document::new();
  let center = document.push("panel", Some("center"));
  let corner = document.push("panel", Some("corner"));
  let footer = document.push("label", None);

  // 属性逐条写：`set` 就是 Inspector 走的那条路径（见 `Editor::apply_control`）。
  for (id, key, value) in [
    (center, "name", PropValue::Text("中央面板".to_owned())),
    (center, "anchor", PropValue::Variant(4)), // center
    (center, "width", PropValue::Number(320.0)),
    (center, "height", PropValue::Number(160.0)),
    (corner, "name", PropValue::Text("左上角".to_owned())),
    (corner, "x", PropValue::Number(32.0)),
    (corner, "y", PropValue::Number(32.0)),
    (corner, "width", PropValue::Number(160.0)),
    (corner, "height", PropValue::Number(96.0)),
    (footer, "name", PropValue::Text("页脚".to_owned())),
    (footer, "anchor", PropValue::Variant(8)), // bottom-right
    (footer, "width", PropValue::Number(200.0)),
  ] {
    if let Some(node) = document.node_mut(id) {
      node.set(key, value);
    }
  }
  document
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

#[cfg(test)]
mod tests {
  use super::*;

  /// 视口够大：样例文档的每一行都装得下，两翼与预览区也各有一块地方。
  const VIEWPORT: LogicalSize = LogicalSize::new(1280.0, 720.0);

  impl Editor {
    /// 某个控件中心的坐标。控件在 `relayout` 之后才有矩形。**只在测试里用**。
    fn control_center(&self, prop: usize, action: Action) -> LogicalPosition {
      let rect = self
        .inspector
        .rect_of(control_id(prop, action))
        .expect("控件该在树里");
      LogicalPosition::new(rect.x + rect.width / 2.0, rect.y + rect.height / 2.0)
    }

    /// 选中第 `row` 个节点（0 起）。
    ///
    /// 点完之后**重摆一次**：右栏控件只按选中的节点进树（见 `relayout_inspector`），选中态变了
    /// 就得再来一遍布局，否则拿不到控件矩形。
    fn select_row(&mut self, row: usize) {
      self.relayout(VIEWPORT);
      self.cursor = row_center(row);
      self.click();
      assert_eq!(
        self.state.selected,
        Some(self.document.nodes()[row].id()),
        "第 {row} 行该选中第 {row} 个节点"
      );
      self.relayout(VIEWPORT);
    }
  }

  /// 第 `row` 行（0 起）中心的坐标。
  fn row_center(row: usize) -> LogicalPosition {
    LogicalPosition::new(
      SIDEBAR_WIDTH / 2.0,
      row as f64 * ROW_HEIGHT + ROW_HEIGHT / 2.0,
    )
  }

  /// `NODE_PROPS` 里的属性序号。
  fn prop_index(key: &str) -> usize {
    NODE_PROPS
      .iter()
      .position(|desc| desc.key == key)
      .unwrap_or_else(|| panic!("属性表里该有 {key}"))
  }

  /// 点结构树第 2 行 → 选中第 2 个节点，且**描边矩形正是该节点 `instantiate` 后的矩形**
  /// （不另算一遍坐标，直接拿预览树里那份比——两边要是各算各的，这条就白测了）。
  #[test]
  fn clicking_the_second_row_selects_the_second_node() {
    let mut editor = Editor::new();
    editor.select_row(1);

    let second = editor.document.nodes()[1].id();
    let rect = editor
      .preview
      .rect_of(second.ui_id())
      .expect("节点在预览树里");
    assert_eq!(editor.selection_stroke(), Some(stroke_quads(rect)));
  }

  /// 点预览区里的方块选中的是同一个节点。
  #[test]
  fn clicking_a_node_in_the_preview_selects_it_too() {
    let mut editor = Editor::new();
    editor.relayout(VIEWPORT);

    let corner = editor.document.nodes()[1].id();
    let rect = editor
      .preview
      .rect_of(corner.ui_id())
      .expect("节点在预览树里");
    editor.cursor = LogicalPosition::new(rect.x + rect.width / 2.0, rect.y + rect.height / 2.0);
    editor.click();

    assert_eq!(editor.state.selected, Some(corner));
  }

  /// 4 条描边合起来恰好压住这个矩形的四边。
  #[test]
  fn the_stroke_follows_the_selected_rect() {
    let rect = LogicalRect::new(120.0, 80.0, 300.0, 160.0);
    let [top, bottom, left, right] = stroke_quads(rect);

    assert_eq!(top.rect, LogicalRect::new(120.0, 80.0, 300.0, STROKE_WIDTH));
    assert_eq!(bottom.rect.y + bottom.rect.height, rect.y + rect.height);
    assert_eq!(bottom.rect.width, rect.width);
    assert_eq!(left.rect.height, rect.height);
    assert_eq!(right.rect.x + right.rect.width, rect.x + rect.width);
    assert_eq!(right.rect.height, rect.height);
    assert!(
      [top, bottom, left, right]
        .iter()
        .all(|quad| quad.color == STROKE_COLOR),
      "四条一个颜色"
    );
  }

  /// 点空白处不动选中态。
  #[test]
  fn clicking_empty_space_keeps_the_selection() {
    let mut editor = Editor::new();
    editor.select_row(0);
    let first = editor.document.nodes()[0].id();

    // 左栏里、行下方的空白：三棵树都没这块。
    editor.cursor = LogicalPosition::new(SIDEBAR_WIDTH / 2.0, VIEWPORT.height - 1.0);
    editor.click();
    assert_eq!(editor.state.selected, Some(first), "空白处不该改选中");
  }

  /// 还没点过就没有描边，右栏也没有控件。
  #[test]
  fn nothing_is_stroked_before_a_selection() {
    let mut editor = Editor::new();
    editor.relayout(VIEWPORT);

    assert_eq!(editor.state.selected, None);
    assert_eq!(editor.selection_stroke(), None);
    assert!(
      editor.inspector.placed().is_empty(),
      "没选中就没有可编辑的东西"
    );
  }

  /// 装不下的行**不进树**：`Widget` 没有裁剪，多摆的行会画到面板外。
  #[test]
  fn rows_that_do_not_fit_stay_out_of_the_tree() {
    let mut editor = Editor::new();
    // 只够两行半。
    editor.relayout(LogicalSize::new(1280.0, ROW_HEIGHT * 2.5));

    assert_eq!(editor.document.nodes().len(), 3, "样例文档是 3 个节点");
    for (index, node) in editor.document.nodes().iter().enumerate() {
      let placed = editor.chrome.rect_of(row_id(node.id())).is_some();
      assert_eq!(placed, index < 2, "第 {index} 行该不该在树里");
    }
  }

  /// 悬停只认结构树：预览区的节点不让左栏某一行亮起来。
  #[test]
  fn hovering_marks_the_row_under_the_cursor() {
    let mut editor = Editor::new();
    editor.cursor = row_center(2);
    editor.relayout(VIEWPORT);
    assert_eq!(editor.state.hovered, Some(editor.document.nodes()[2].id()));

    editor.cursor = LogicalPosition::new(SIDEBAR_WIDTH / 2.0, VIEWPORT.height - 1.0);
    editor.relayout(VIEWPORT);
    assert_eq!(editor.state.hovered, None);
  }

  /// 行 id 与节点 id 是两套，都反查得回同一个节点。
  #[test]
  fn both_ids_reverse_lookup_the_same_node() {
    let mut editor = Editor::new();
    editor.relayout(VIEWPORT);

    for node in editor.document.nodes() {
      assert_ne!(row_id(node.id()), node.id().ui_id(), "两套 id 不重合");
      assert_eq!(editor.node_of(row_id(node.id())), Some(node.id()));
      assert_eq!(editor.node_of(node.id().ui_id()), Some(node.id()));
    }
  }

  /// 点 `+step` → **文档里的值**变了一步，且下一帧预览矩形跟着动（实时预览的闭环）。
  #[test]
  fn stepping_a_number_moves_the_node() {
    let mut editor = Editor::new();
    editor.select_row(0);

    let node = editor.document.nodes()[0].id();
    let width = prop_index("width");
    let before_value = editor.document.node_by_id(node).unwrap().number("width");
    let before_rect = editor.preview.rect_of(node.ui_id()).unwrap();

    editor.cursor = editor.control_center(width, Action::Increase);
    editor.click();

    // 值落在文档里（不是只改画面）。
    assert_eq!(
      editor.document.node_by_id(node).unwrap().number("width"),
      before_value + 8.0
    );
    // 下一帧重投影：矩形跟着变。
    editor.relayout(VIEWPORT);
    let after_rect = editor.preview.rect_of(node.ui_id()).unwrap();
    assert_eq!(after_rect.width, before_rect.width + 8.0);
    assert_ne!(after_rect, before_rect, "实时预览：改一个数，画面就该动");
  }

  /// 步进被 `min` / `max` 夹住，不会跑出属性表写的范围。
  #[test]
  fn stepping_stops_at_the_bounds() {
    let mut editor = Editor::new();
    editor.select_row(0);

    let node = editor.document.nodes()[0].id();
    let width = prop_index("width");
    // 一路往左点到夹住为止（200 步 × 8 远超过 320）。
    for _ in 0..200 {
      editor.cursor = editor.control_center(width, Action::Decrease);
      editor.click();
    }

    assert_eq!(
      editor.document.node_by_id(node).unwrap().number("width"),
      0.0,
      "宽的下界是 0，夹住而不是变成负数"
    );
  }

  /// 枚举按钮「走到下一个」，走到表尾绕回表头。
  #[test]
  fn the_enum_button_cycles_through_candidates() {
    let mut editor = Editor::new();
    editor.select_row(0);

    let node = editor.document.nodes()[0].id();
    let anchor = prop_index("anchor");
    // 样例里 anchor = 4（center），九个候选走一圈回到原处。
    for expected in [5, 6, 7, 8, 0, 1, 2, 3, 4] {
      editor.cursor = editor.control_center(anchor, Action::Next);
      editor.click();
      assert_eq!(
        editor.document.node_by_id(node).unwrap().variant("anchor"),
        expected
      );
    }
  }

  /// 文本属性第一刀只读：树上没有它的控件。
  #[test]
  fn a_text_prop_has_no_control() {
    let mut editor = Editor::new();
    editor.select_row(0);

    let name = prop_index("name");
    for action in [Action::Decrease, Action::Increase, Action::Next] {
      assert_eq!(editor.inspector.rect_of(control_id(name, action)), None);
    }
  }

  /// 控件 id 反解得回原样，越界与坏动作码都不认。
  #[test]
  fn control_ids_round_trip() {
    for prop in 0..NODE_PROPS.len() {
      for action in [Action::Decrease, Action::Increase, Action::Next] {
        assert_eq!(
          control_of(control_id(prop, action)),
          Some(Control { prop, action })
        );
      }
    }

    // 序号越界、动作码越界都不是控件。
    assert_eq!(control_of(control_id(NODE_PROPS.len(), Action::Next)), None);
    assert_eq!(control_of(UiId(0xF)), None);
  }

  /// 点 Inspector 上的控件不改选中态。
  #[test]
  fn pressing_a_control_keeps_the_selection() {
    let mut editor = Editor::new();
    editor.select_row(0);
    let selected = editor.state.selected;

    editor.cursor = editor.control_center(prop_index("height"), Action::Increase);
    editor.click();

    assert_eq!(editor.state.selected, selected);
  }
}
