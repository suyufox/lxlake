//! 窗口契约：身份、建窗参数、以及**平台无关的窗口操作句柄**。

use crate::core::geometry::{LogicalSize, PhysicalSize};
use raw_window_handle::{HasDisplayHandle, HasWindowHandle};

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
///
/// 两个超 trait（[`HasWindowHandle`] / [`HasDisplayHandle`]）来自 `raw-window-handle`——它是
/// **中性的 ABI 契约**，不是 winit 或 wgpu 的类型，因此放在 `core` 里不破坏「契约层不含平台 /
/// GPU 类型」这条硬约束。渲染侧要的就是它：拿到原生句柄就能建表面，全程不必认识 winit。
pub trait WindowHandle: HasWindowHandle + HasDisplayHandle + Send + Sync + 'static {
  fn id(&self) -> WindowId;

  /// 当前物理尺寸（给渲染用的就是它）。
  fn size(&self) -> PhysicalSize;

  /// 当前 DPI 缩放因子。
  fn scale_factor(&self) -> f64;

  fn set_title(&self, title: &str);

  /// 抓取 / 释放光标：抓住后光标不再跑出窗口，视角控制要它。
  ///
  /// 平台可能拒绝（不支持，或系统不允许）——此时**只记日志并继续**：抓不住光标不该让应用
  /// 起不来，最多是视角控制手感差些。
  fn set_cursor_grab(&self, grab: bool);

  /// 光标可见性：抓着光标转视角时通常要把它藏起来。
  fn set_cursor_visible(&self, visible: bool);
}
