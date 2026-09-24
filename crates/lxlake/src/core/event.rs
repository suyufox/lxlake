//! 运行时事件。平台负责把原生事件映射到这里（映射逻辑在 `platform`，契约层只定义形状）。

use crate::core::geometry::PhysicalSize;
use crate::core::window::WindowId;

/// 应用通过 `App::on_event` 收到的事件。
#[derive(Debug, Clone, PartialEq)]
pub enum Event {
  /// 窗口尺寸变化（物理像素）。
  Resized {
    window: WindowId,
    size: PhysicalSize,
  },

  /// DPI 缩放因子变化（跨显示器拖动、系统缩放调整）。
  ScaleFactorChanged {
    window: WindowId,
    scale_factor: f64,
    size: PhysicalSize,
  },

  /// 焦点变化。
  Focused { window: WindowId, focused: bool },

  /// 用户请求关闭（点关闭按钮）。运行时在派发后退出事件循环。
  CloseRequested { window: WindowId },
}
