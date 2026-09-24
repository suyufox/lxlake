//! 尺寸量。契约层自有类型——**不引用 winit 的 `PhysicalSize` / `LogicalSize`**。

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
    let scale = if scale_factor > 0.0 {
      scale_factor
    } else {
      1.0
    };
    LogicalSize {
      width: f64::from(self.width) / scale,
      height: f64::from(self.height) / scale,
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
