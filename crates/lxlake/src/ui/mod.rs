//! 自绘 UI：布局、命中测试、文本排版。
//!
//! 与 [`crate::core::widget`] 的分工：契约层定「Widget 长什么样」（锚定 + 尺寸 + 是否覆盖层），
//! 本模块定「怎么算」（摆位成矩形、命中哪一块）。**纯 CPU**——不含任何 GPU 类型，也不加
//! feature 门（见 `docs/architecture.md` 分层）。
//!
//! 「覆盖层永远浮在最上」这条架构约定在这里就变成一条排序规则：`overlay` 的 Widget 整组排在
//! 自绘之上，而不是按加入顺序混在一次 z 序里（见 `docs/architecture.md` 的 webview 双线）。

use crate::core::geometry::{LogicalPosition, LogicalRect, LogicalSize};
use crate::core::widget::{UiId, Widget};

mod draw;
mod text;

pub use draw::{Quad, QuadSource};
pub use text::{GlyphBitmap, GlyphKey, TextError, TextShaper, TextStyle};

/// 布局结果：一个 Widget 在视口里的矩形。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Placed {
  pub id: UiId,
  pub rect: LogicalRect,
  /// 是否由原生覆盖层承载。**渲染要跳过它们**——覆盖层是原生子窗口，不在自绘的 z 序里。
  pub overlay: bool,
}

/// UI 树：一组 Widget，以及按视口算出来的摆位。
#[derive(Debug, Default, Clone)]
pub struct UiTree {
  widgets: Vec<Widget>,
  placed: Vec<Placed>,
}

impl UiTree {
  pub fn new() -> Self {
    Self::default()
  }

  /// 加一个 Widget。**后加的在上层**（覆盖层整组另算，见 [`UiTree::hit_test`]）。
  pub fn add(&mut self, widget: Widget) {
    self.widgets.push(widget);
  }

  /// 清空：重建一棵树比逐个删简单，HUD 每帧重建也无所谓（Widget 是纯数据）。
  pub fn clear(&mut self) {
    self.widgets.clear();
    self.placed.clear();
  }

  /// 按视口尺寸算摆位。视口变化或 Widget 变动后必须重算一次，否则命中测试用的是旧矩形。
  ///
  /// 这是 [`UiTree::layout_in`] 的薄包装：**视口就是一个原点在 `(0, 0)` 的容器**。
  pub fn layout(&mut self, viewport: LogicalSize) {
    self.layout_in(LogicalRect::new(0.0, 0.0, viewport.width, viewport.height));
  }

  /// 按**任意容器矩形**算摆位。
  ///
  /// 编辑器的预览区、将来面板内部都用它：文档节点锚进一个**算出来的**区域，于是文档里永远
  /// 不含窗口尺寸（否则存盘会把某台机器的分辨率存进去）。摆位结果仍是绝对坐标，命中测试
  /// 因此不需要任何坐标转换（见 [`Placed`]）。
  pub fn layout_in(&mut self, area: LogicalRect) {
    self.placed = self
      .widgets
      .iter()
      .map(|widget| Placed {
        id: widget.id,
        rect: place_in(widget, area),
        overlay: widget.overlay,
      })
      .collect();
  }

  /// 算好的摆位（加入顺序）。
  pub fn placed(&self) -> &[Placed] {
    &self.placed
  }

  /// 按 id 取矩形；布局之前是 `None`。
  pub fn rect_of(&self, id: UiId) -> Option<LogicalRect> {
    self
      .placed
      .iter()
      .find(|placed| placed.id == id)
      .map(|placed| placed.rect)
  }

  /// 命中测试：该点上最上层的 Widget。
  ///
  /// 覆盖层**整组优先**于自绘——不是「加得晚就赢」，而是因为覆盖层是原生子窗口，永远浮在
  /// 自绘画面之上（见模块文档）。同组内后加的赢。
  pub fn hit_test(&self, point: LogicalPosition) -> Option<UiId> {
    self
      .placed
      .iter()
      .rev()
      .filter(|placed| placed.overlay)
      .chain(self.placed.iter().rev().filter(|placed| !placed.overlay))
      .find(|placed| placed.rect.contains(point))
      .map(|placed| placed.id)
  }
}

/// 锚定摆位（容器内）：先按九宫格归一化坐标贴到**容器**，再沿屏幕方向加偏移。
///
/// **唯一的摆位实现**——自绘与原生覆盖层共用它：自绘把结果交给渲染，覆盖层把同一个矩形交给原生
/// 子窗口（见 `capability::webview`）。接覆盖层时不必另写一套「覆盖层专用布局」。
pub fn place_in(widget: &Widget, area: LogicalRect) -> LogicalRect {
  let [fx, fy] = widget.anchor.normalized();
  LogicalRect::new(
    (area.width - widget.size.width) * fx + area.x + widget.offset[0],
    (area.height - widget.size.height) * fy + area.y + widget.offset[1],
    widget.size.width,
    widget.size.height,
  )
}

/// 摆进视口：原点在 `(0, 0)` 的容器交给 [`place_in`]。
pub fn place(widget: &Widget, viewport: LogicalSize) -> LogicalRect {
  place_in(
    widget,
    LogicalRect::new(0.0, 0.0, viewport.width, viewport.height),
  )
}

#[cfg(test)]
mod tests {
  use super::*;
  use crate::core::geometry::LogicalSize;
  use crate::core::widget::Anchor;

  const VIEWPORT: LogicalSize = LogicalSize::new(1000.0, 600.0);

  fn widget(id: u32, anchor: Anchor, width: f64, height: f64) -> Widget {
    Widget::new(UiId(id), anchor, LogicalSize::new(width, height))
  }

  #[test]
  fn anchors_place_widgets_at_their_corners() {
    let mut tree = UiTree::new();
    tree.add(widget(1, Anchor::TopLeft, 100.0, 40.0));
    tree.add(widget(2, Anchor::Center, 100.0, 40.0));
    tree.add(widget(3, Anchor::BottomRight, 100.0, 40.0));

    tree.layout(VIEWPORT);

    assert_eq!(tree.rect_of(UiId(1)).unwrap().x, 0.0);
    assert_eq!(tree.rect_of(UiId(1)).unwrap().y, 0.0);
    // 居中 = (视口 − 尺寸) × 0.5，不是视口 × 0.5。
    assert_eq!(tree.rect_of(UiId(2)).unwrap().x, 450.0);
    assert_eq!(tree.rect_of(UiId(2)).unwrap().y, 280.0);
    assert_eq!(tree.rect_of(UiId(3)).unwrap().x, 900.0);
    assert_eq!(tree.rect_of(UiId(3)).unwrap().y, 560.0);
  }

  #[test]
  fn offset_pushes_a_widget_inward_from_its_anchor() {
    let mut tree = UiTree::new();
    // 右上角的 HUD：往内推 16 像素，所以 x 是负的。
    tree.add(widget(1, Anchor::TopRight, 200.0, 100.0).offset([-16.0, 16.0]));

    tree.layout(VIEWPORT);

    let rect = tree.rect_of(UiId(1)).unwrap();
    assert_eq!(rect.x, 784.0);
    assert_eq!(rect.y, 16.0);
  }

  #[test]
  fn relayout_follows_the_viewport() {
    let mut tree = UiTree::new();
    tree.add(widget(1, Anchor::BottomRight, 100.0, 40.0));

    tree.layout(VIEWPORT);
    assert_eq!(tree.rect_of(UiId(1)).unwrap().x, 900.0);

    // 窗口缩小：贴右下的元素跟着走。
    tree.layout(LogicalSize::new(500.0, 300.0));
    assert_eq!(tree.rect_of(UiId(1)).unwrap().x, 400.0);
    assert_eq!(tree.rect_of(UiId(1)).unwrap().y, 260.0);
  }

  #[test]
  fn without_layout_there_is_no_rect() {
    let mut tree = UiTree::new();
    tree.add(widget(1, Anchor::TopLeft, 10.0, 10.0));

    // 命中测试与取矩形都只看摆位，没算过就是空的——避免「用的是上一帧的矩形」这种静默错误。
    assert_eq!(tree.rect_of(UiId(1)), None);
    assert_eq!(tree.hit_test(LogicalPosition::new(5.0, 5.0)), None);
  }

  #[test]
  fn the_later_widget_wins_inside_the_same_group() {
    let mut tree = UiTree::new();
    tree.add(widget(1, Anchor::TopLeft, 100.0, 100.0));
    tree.add(widget(2, Anchor::TopLeft, 100.0, 100.0));

    tree.layout(VIEWPORT);

    assert_eq!(
      tree.hit_test(LogicalPosition::new(50.0, 50.0)),
      Some(UiId(2))
    );
  }

  /// 覆盖层不是「加得晚就赢」：即使自绘的 Widget 后加，覆盖层那块矩形仍然归它。
  #[test]
  fn overlays_win_over_later_drawn_widgets() {
    let mut tree = UiTree::new();
    tree.add(widget(1, Anchor::TopLeft, 100.0, 100.0).overlay());
    tree.add(widget(2, Anchor::TopLeft, 100.0, 100.0));

    tree.layout(VIEWPORT);

    assert_eq!(
      tree.hit_test(LogicalPosition::new(50.0, 50.0)),
      Some(UiId(1))
    );
    // 覆盖层之外照样归自绘的那块。
    tree.add(widget(3, Anchor::Center, 20.0, 20.0));
    tree.layout(VIEWPORT);
    assert_eq!(
      tree.hit_test(LogicalPosition::new(500.0, 300.0)),
      Some(UiId(3))
    );
  }

  #[test]
  fn clicks_outside_every_widget_miss() {
    let mut tree = UiTree::new();
    tree.add(widget(1, Anchor::TopLeft, 100.0, 40.0));
    tree.layout(VIEWPORT);

    assert_eq!(
      tree.hit_test(LogicalPosition::new(99.0, 39.0)),
      Some(UiId(1))
    );
    assert_eq!(tree.hit_test(LogicalPosition::new(100.0, 39.0)), None);
    assert_eq!(tree.hit_test(LogicalPosition::new(500.0, 300.0)), None);
  }

  #[test]
  fn clear_drops_widgets_and_placement() {
    let mut tree = UiTree::new();
    tree.add(widget(1, Anchor::TopLeft, 100.0, 40.0));
    tree.layout(VIEWPORT);

    tree.clear();

    assert!(tree.placed().is_empty());
    assert_eq!(tree.hit_test(LogicalPosition::new(10.0, 10.0)), None);
  }

  /// 薄包装没走样：**视口布局 == 摆进「原点在 (0,0) 的同尺寸容器」**，逐项相等。
  #[test]
  fn layout_in_a_full_viewport_area_matches_layout() {
    let mut via_layout = UiTree::new();
    let mut via_area = UiTree::new();
    let widgets = [
      widget(1, Anchor::TopLeft, 100.0, 40.0),
      widget(2, Anchor::Center, 100.0, 40.0).offset([8.0, -8.0]),
      // 覆盖层也要一起对：这条比较的是整张 `placed`，别漏了那个标志位。
      widget(3, Anchor::BottomRight, 100.0, 40.0).overlay(),
    ];
    for widget in &widgets {
      via_layout.add(widget.clone());
      via_area.add(widget.clone());
    }

    via_layout.layout(VIEWPORT);
    via_area.layout_in(LogicalRect::new(0.0, 0.0, VIEWPORT.width, VIEWPORT.height));

    assert_eq!(via_layout.placed(), via_area.placed());
  }

  /// 非零原点：容器里算出来的坐标整体平移，归一化比例仍按**容器**尺寸算。
  #[test]
  fn place_in_shifts_the_whole_container() {
    let widget = widget(1, Anchor::Center, 100.0, 40.0);
    let at_origin = place(&widget, VIEWPORT);
    let inside = place_in(
      &widget,
      LogicalRect::new(300.0, 200.0, VIEWPORT.width, VIEWPORT.height),
    );

    assert_eq!(inside.x, at_origin.x + 300.0);
    assert_eq!(inside.y, at_origin.y + 200.0);
  }
}
