//! 运行时事件。平台负责把原生事件映射到这里（映射逻辑在 `platform`，契约层只定义形状）。

use crate::core::geometry::PhysicalSize;
use crate::core::input::{Key, MouseButton};
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

  /// 键盘按下 / 抬起。
  KeyboardInput {
    window: WindowId,
    key: Key,
    pressed: bool,
    /// 操作系统的按键长按重复。
    repeat: bool,
  },

  /// 原始鼠标位移（设备级）。
  ///
  /// **不带窗口**：设备事件本来就不属于任何窗口（平台后端拿不到窗口 id）。
  /// 给的是**位移量**而不是光标位置——光标位置受屏幕边界与系统加速影响，视角控制要的
  /// 正是位移量，这也是 winit 自己提醒的那一点。
  MouseMotion { delta: [f32; 2] },

  /// 鼠标按键按下 / 抬起。
  MouseButton {
    window: WindowId,
    button: MouseButton,
    pressed: bool,
  },

  /// 滚轮：行数或像素，看平台给哪种。
  MouseWheel { window: WindowId, delta: [f32; 2] },
}
