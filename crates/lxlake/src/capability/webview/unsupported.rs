//! 没有后端时的垫底实装。
//!
//! 这份**不是错误路径上的补丁**：接口层常编译，编辑器与 demo 都要能照常声明覆盖层（
//! `Builder::webview`），所以必须有东西接住那些调用，而且要接得诚实——能力全 `false`，创建一律
//! [`WebViewError::Unsupported`]。设备平台之外的平台也走这里：Linux 因此不必引 webkit2gtk 系统依赖，
//! 与现有 C 依赖清单的口径不冲突（见 `docs/architecture.md` 的 webview 双线）。

use super::{
  Capabilities, LogicalRect, Mode, WebViewConfig, WebViewError, WebViewHandle, WindowHandle,
};
use std::sync::Arc;

/// 当前构建没有 webview 后端。
pub fn capabilities() -> Capabilities {
  Capabilities::none()
}

/// 一律 `Unsupported`，且**先答这一条**，不去跑 [`validate`](super::validate)——把「这个构建里根本
/// 没有 webview」报成「沙箱不支持」会把排查引到错的地方。
pub(super) fn build_overlay(
  _parent: &Arc<dyn WindowHandle>,
  _config: &WebViewConfig,
  _rect: LogicalRect,
) -> Result<Box<dyn WebViewHandle>, WebViewError> {
  Err(WebViewError::Unsupported {
    platform: std::env::consts::OS,
    mode: Mode::Overlay,
  })
}

#[cfg(test)]
mod tests {
  use super::*;
  use crate::core::geometry::PhysicalSize;
  use crate::core::window::WindowId;
  use raw_window_handle as rwh;

  /// 只为凑出 `&Arc<dyn WindowHandle>`：无后端路径在读到它之前就返回了。
  struct FakeWindow;

  impl rwh::HasWindowHandle for FakeWindow {
    fn window_handle(&self) -> Result<rwh::WindowHandle<'_>, rwh::HandleError> {
      // SAFETY: 假句柄只用于满足签名，这条路径不会解引用它。
      Ok(unsafe {
        rwh::WindowHandle::borrow_raw(rwh::RawWindowHandle::Web(rwh::WebWindowHandle::new(1)))
      })
    }
  }

  impl rwh::HasDisplayHandle for FakeWindow {
    fn display_handle(&self) -> Result<rwh::DisplayHandle<'_>, rwh::HandleError> {
      Ok(rwh::DisplayHandle::web())
    }
  }

  impl WindowHandle for FakeWindow {
    fn id(&self) -> WindowId {
      WindowId(0)
    }

    fn size(&self) -> PhysicalSize {
      PhysicalSize::new(0, 0)
    }

    fn scale_factor(&self) -> f64 {
      1.0
    }

    fn set_title(&self, _title: &str) {}

    fn set_cursor_grab(&self, _grab: bool) {}

    fn set_cursor_visible(&self, _visible: bool) {}
  }

  #[test]
  fn the_fallback_supports_nothing() {
    let caps = capabilities();

    assert!(!caps.overlay, "没有后端就没有覆盖层");
    assert!(!caps.texture, "纹理层要 CEF，更不可能有");
    assert!(!caps.sandbox);
    assert!(!caps.devtools);
  }

  /// 就算配置本身完全合法，答案也必须是「没有后端」而不是「沙箱不支持」。
  #[test]
  fn building_an_overlay_names_the_missing_backend_not_the_config() {
    let parent: Arc<dyn WindowHandle> = Arc::new(FakeWindow);
    let config = WebViewConfig::html("<p>hi</p>");
    let rect = LogicalRect::new(0.0, 0.0, 320.0, 200.0);

    let outcome = build_overlay(&parent, &config, rect);
    let error = match outcome {
      Err(error) => error,
      Ok(_) => panic!("没有后端就必须拒建"),
    };

    match error {
      WebViewError::Unsupported { platform, mode } => {
        assert_eq!(platform, std::env::consts::OS);
        assert_eq!(mode, Mode::Overlay);
      }
      other => panic!("该报「没有后端」，实际是 {other}"),
    }
  }
}
