//! 窗口契约：身份、建窗参数、以及**平台无关的窗口操作句柄**。

use crate::core::geometry::{LogicalSize, PhysicalSize};

/// 窗口身份。契约层自有类型，与平台的窗口 id 解耦。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct WindowId(pub u64);

/// 建窗参数。
#[derive(Debug, Clone, PartialEq)]
pub struct WindowDesc {
  pub title: String,
  /// 逻辑尺寸：同样大小的窗口在不同 DPI 下观感一致。
  pub size: LogicalSize,
  pub resizable: bool,
  pub visible: bool,
}

impl Default for WindowDesc {
  fn default() -> Self {
    Self {
      title: "lxlake".to_owned(),
      size: LogicalSize::new(1280.0, 720.0),
      resizable: true,
      visible: true,
    }
  }
}

/// 窗口操作句柄：`runtime` 与应用看窗口的**唯一**接口。
///
/// 实现由 `platform` 提供（winit 的 `Window`）。正因为这一层，`core` 与 `runtime`
/// 都不必知道窗口是哪个库造的。
pub trait WindowHandle: Send + Sync + 'static {
  fn id(&self) -> WindowId;

  /// 当前物理尺寸（给渲染用的就是它）。
  fn size(&self) -> PhysicalSize;

  /// 当前 DPI 缩放因子。
  fn scale_factor(&self) -> f64;

  fn set_title(&self, title: &str);
}
