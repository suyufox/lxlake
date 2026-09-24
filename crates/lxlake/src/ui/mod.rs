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
  pub fn layout(&mut self, viewport: LogicalSize) {
    self.placed = self
      .widgets
      .iter()
      .map(|widget| Placed {
        id: widget.id,
        rect: place(widget, viewport),
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

/// 锚定摆位：先按九宫格归一化坐标贴到锚点，再沿屏幕方向加偏移。
fn place(widget: &Widget, viewport: LogicalSize) -> LogicalRect {
  let [fx, fy] = widget.anchor.normalized();
  LogicalRect::new(
    (viewport.width - widget.size.width) * fx + widget.offset[0],
    (viewport.height - widget.size.height) * fy + widget.offset[1],
    widget.size.width,
    widget.size.height,
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
}
