//! Widget 契约：**自绘 UI 与原生覆盖层共用**的一份摆位数据。
//!
//! 共用是刻意的：两种呈现形态按同一套锚定规则摆位——自绘的 HUD 由 [`crate::ui`] 算成矩形后
//! 交给渲染，wry 覆盖层算成同一个矩形后交给原生子窗口。摆位规则因此只有一份，接 wry 时不必
//! 再写一套「覆盖层专用布局」（见 `docs/architecture.md` 的 webview 双线）。
//!
//! `overlay` 不是普通的 z 序标志：覆盖层是**原生子窗口**，永远浮在最上、不参与 z 序与裁剪，
//! 所以它在输入归属上整组排在自绘 UI 之上。

use crate::core::geometry::LogicalSize;

/// Widget 身份。应用自己编号，UI 层只按它记账。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct UiId(pub u32);

/// 锚定位置：视口九宫格。
///
/// 用锚定而不是绝对坐标，是为了让「右上角的信息」在窗口缩放后仍然贴着右上角——HUD 的元素
/// 几乎都是这么定位的。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Anchor {
  TopLeft,
  TopCenter,
  TopRight,
  CenterLeft,
  Center,
  CenterRight,
  BottomLeft,
  BottomCenter,
  BottomRight,
}

impl Anchor {
  /// 锚点在视口里的归一化坐标（`0.0` = 左 / 上，`1.0` = 右 / 下）。
  ///
  /// 布局只需一次乘法：`x = (viewport.width - widget.width) * fx`。把它做成数据而不是在布局里
  /// `match` 九个分支，布局代码因此没有枚举分支，九宫格全由这一张表决定。
  pub const fn normalized(self) -> [f64; 2] {
    let fx = match self {
      Self::TopLeft | Self::CenterLeft | Self::BottomLeft => 0.0,
      Self::TopCenter | Self::Center | Self::BottomCenter => 0.5,
      Self::TopRight | Self::CenterRight | Self::BottomRight => 1.0,
    };
    let fy = match self {
      Self::TopLeft | Self::TopCenter | Self::TopRight => 0.0,
      Self::CenterLeft | Self::Center | Self::CenterRight => 0.5,
      Self::BottomLeft | Self::BottomCenter | Self::BottomRight => 1.0,
    };
    [fx, fy]
  }
}

/// 一个 Widget 的摆位数据：锚定 + 尺寸 + 偏移。
#[derive(Debug, Clone, PartialEq)]
pub struct Widget {
  pub id: UiId,
  pub anchor: Anchor,
  /// 从锚点往视口内侧推的偏移（逻辑像素，向右下为正）。
  pub offset: [f64; 2],
  pub size: LogicalSize,
  /// 是否由**原生覆盖层**（wry 子窗口）承载，而不是自绘。
  ///
  /// 覆盖层永远浮在最上，故命中测试与输入归属都把它整组排在自绘 Widget 之前。
  pub overlay: bool,
}

impl Widget {
  /// 贴住锚点、无偏移的自绘 Widget。
  pub fn new(id: UiId, anchor: Anchor, size: LogicalSize) -> Self {
    Self {
      id,
      anchor,
      offset: [0.0, 0.0],
      size,
      overlay: false,
    }
  }

  /// 从锚点推开一段偏移（逻辑像素）。
  pub fn offset(mut self, offset: [f64; 2]) -> Self {
    self.offset = offset;
    self
  }

  /// 标记为由原生覆盖层承载（wry），而不是自绘。
  pub fn overlay(mut self) -> Self {
    self.overlay = true;
    self
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn anchors_cover_the_full_three_by_three_grid() {
    assert_eq!(Anchor::TopLeft.normalized(), [0.0, 0.0]);
    assert_eq!(Anchor::Center.normalized(), [0.5, 0.5]);
    assert_eq!(Anchor::BottomRight.normalized(), [1.0, 1.0]);
    assert_eq!(Anchor::TopCenter.normalized(), [0.5, 0.0]);
    assert_eq!(Anchor::CenterLeft.normalized(), [0.0, 0.5]);
    assert_eq!(Anchor::BottomCenter.normalized(), [0.5, 1.0]);
  }

  #[test]
  fn a_new_widget_is_drawn_at_its_anchor() {
    let widget = Widget::new(UiId(1), Anchor::TopLeft, LogicalSize::new(120.0, 40.0));

    assert_eq!(widget.offset, [0.0, 0.0]);
    assert!(!widget.overlay, "默认是自绘");
  }

  #[test]
  fn overlay_and_offset_are_chainable() {
    let widget = Widget::new(UiId(2), Anchor::TopRight, LogicalSize::new(320.0, 240.0))
      .offset([16.0, 16.0])
      .overlay();

    assert!(widget.overlay);
    assert_eq!(widget.offset, [16.0, 16.0]);
  }
}
