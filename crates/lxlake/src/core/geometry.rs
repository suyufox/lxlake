//! 几何量。契约层自有类型——**不引用 winit 的 `PhysicalSize` / `LogicalSize` / `PhysicalPosition`**。

/// 物理像素尺寸（设备像素，随 DPI 变化）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct PhysicalSize {
  pub width: u32,
  pub height: u32,
}

impl PhysicalSize {
  pub const fn new(width: u32, height: u32) -> Self {
    Self { width, height }
  }

  /// 按缩放因子换算为逻辑尺寸。
  pub fn to_logical(self, scale_factor: f64) -> LogicalSize {
    let scale = sanitize_scale(scale_factor);
    LogicalSize {
      width: f64::from(self.width) / scale,
      height: f64::from(self.height) / scale,
    }
  }
}

/// 物理像素坐标（设备像素，窗口左上角为原点）。
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct PhysicalPosition {
  pub x: f64,
  pub y: f64,
}

impl PhysicalPosition {
  pub const fn new(x: f64, y: f64) -> Self {
    Self { x, y }
  }

  /// 按缩放因子换算为逻辑坐标。
  pub fn to_logical(self, scale_factor: f64) -> LogicalPosition {
    let scale = sanitize_scale(scale_factor);
    LogicalPosition {
      x: self.x / scale,
      y: self.y / scale,
    }
  }
}

/// 逻辑像素尺寸（与 DPI 无关，即用户感知的「大小」）。
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct LogicalSize {
  pub width: f64,
  pub height: f64,
}

impl LogicalSize {
  pub const fn new(width: f64, height: f64) -> Self {
    Self { width, height }
  }
}

/// 逻辑像素坐标（窗口左上角为原点，与 DPI 无关）。
///
/// 与设备级位移（[`crate::core::event::Event::MouseMotion`]）的分工：**位置**喂 UI 命中测试，
/// **位移量**喂视角控制。前者必须知道自己在窗口里的哪儿，后者只关心动了多少。
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct LogicalPosition {
  pub x: f64,
  pub y: f64,
}

impl LogicalPosition {
  pub const fn new(x: f64, y: f64) -> Self {
    Self { x, y }
  }
}

/// 逻辑像素矩形（左上角 + 尺寸）。UI 布局与命中测试的通用形状。
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct LogicalRect {
  pub x: f64,
  pub y: f64,
  pub width: f64,
  pub height: f64,
}

impl LogicalRect {
  pub const fn new(x: f64, y: f64, width: f64, height: f64) -> Self {
    Self {
      x,
      y,
      width,
      height,
    }
  }

  /// 点是否落在矩形内。
  ///
  /// 左 / 上边算**在**内，右 / 下边算**不在**内：相邻两块 UI 因此不会同时命中同一点，
  /// 命中测试的结果与遍历顺序无关。
  pub fn contains(self, point: LogicalPosition) -> bool {
    point.x >= self.x
      && point.x < self.x + self.width
      && point.y >= self.y
      && point.y < self.y + self.height
  }
}

/// 缩放因子守卫：非法值（0 / 负数 / NaN）一律按 1.0 处理。
///
/// 除零会把坐标变成 `inf`，之后一路传进布局与渲染，症状是「画面整个没了」，很难倒查到这里。
pub(crate) fn sanitize_scale(scale_factor: f64) -> f64 {
  if scale_factor > 0.0 {
    scale_factor
  } else {
    1.0
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn physical_size_converts_to_logical() {
    let size = PhysicalSize::new(2560, 1440).to_logical(2.0);
    assert_eq!(size, LogicalSize::new(1280.0, 720.0));
  }

  #[test]
  fn physical_position_converts_to_logical() {
    let position = PhysicalPosition::new(200.0, 100.0).to_logical(2.0);
    assert_eq!(position, LogicalPosition::new(100.0, 50.0));
  }

  /// 非法缩放因子不该让坐标变成 `inf`：按 1.0 兜底。
  #[test]
  fn illegal_scale_factor_falls_back_to_one() {
    for scale in [0.0, -2.0, f64::NAN] {
      assert_eq!(
        PhysicalPosition::new(30.0, 40.0).to_logical(scale),
        LogicalPosition::new(30.0, 40.0)
      );
      assert_eq!(
        PhysicalSize::new(30, 40).to_logical(scale),
        LogicalSize::new(30.0, 40.0)
      );
    }
  }

  #[test]
  fn rect_contains_its_own_half_open_bounds() {
    let rect = LogicalRect::new(10.0, 20.0, 30.0, 40.0);

    assert!(rect.contains(LogicalPosition::new(10.0, 20.0)), "左上是内");
    assert!(rect.contains(LogicalPosition::new(39.9, 59.9)));
    assert!(!rect.contains(LogicalPosition::new(40.0, 30.0)), "右边是外");
    assert!(!rect.contains(LogicalPosition::new(20.0, 60.0)), "下边是外");
    assert!(!rect.contains(LogicalPosition::new(9.9, 30.0)), "左边之外");
    assert!(!rect.contains(LogicalPosition::new(20.0, 19.9)), "上边之外");
  }
}
