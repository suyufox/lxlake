//! 契约层：事件、几何、窗口等**跨层共享的数据与 trait**。
//!
//! **硬约束：本层不出现 winit / wgpu 类型**，也不依赖任何平台实现。这条是可机检的——
//! 契约层一旦出现这些类型，框架主线就再也无法在不启用渲染的情况下干净编译
//! （见 `docs/architecture.md` 分层）。

pub mod event;
pub mod geometry;
pub mod window;

use std::fmt;

/// 框架统一错误。
#[derive(Debug)]
pub enum Error {
  /// 平台后端失败：事件循环创建/运行、建窗等。
  Platform(String),
}

impl fmt::Display for Error {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    match self {
      Error::Platform(message) => write!(f, "平台错误：{message}"),
    }
  }
}

impl std::error::Error for Error {}
