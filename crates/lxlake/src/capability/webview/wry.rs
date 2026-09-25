//! Windows 上的 wry 覆盖层后端（WebView2）。
//!
//! 只用 wry 的**子窗口**形态（`build_as_child`）：覆盖层是原生子窗口，永远浮在画面与自绘 UI 之上，
//! 不参与 z 序与裁剪——这正是 `Widget::overlay` 那条语义的实装侧。
//!
//! 父窗口的缝走 `raw-window-handle`：wry 要的是 `&impl HasWindowHandle`，而运行时手里是
//! `Arc<dyn WindowHandle>`；这一层「容器实现」由 raw-window-handle 自己提供（`Arc<H: ?Sized>` 那
//! 条 blanket impl），所以这里**不需要**任何转发包装，也不必给 `core` 加 downcast。
//!
//! 没有 `pump`：wry 0.57 不提供泵接口，Windows 上 WebView2 的活儿跑在宿主消息循环里（winit 的
//! 那个循环就是它的泵）。`runtime::EventSource` 因此留给将来的 CEF 纹理层。

use super::{
  Capabilities, LogicalRect, Mode, WebViewConfig, WebViewError, WebViewHandle, WebViewSource,
  validate,
};
use crate::core::window::WindowHandle;
use std::sync::Arc;

/// 当前后端支持什么。
///
/// - 覆盖层有（Windows 原生子窗口）；纹理层没有——那是 CEF 的活。
/// - 沙箱恒 `true`：WebView2 的内容跑在独立进程里（配合受限令牌），wry 也不给「关掉沙箱」的开关，
///   于是默认的 `SandboxPolicy::Required` 不会在 Windows 上误伤。
/// - 开发者工具按构建分档：调试构建开，发行构建关（`config.devtools` 只在这个前提下生效）。
pub fn capabilities() -> Capabilities {
  Capabilities {
    overlay: true,
    texture: false,
    sandbox: true,
    devtools: cfg!(debug_assertions),
  }
}

/// 建一个覆盖层。
///
/// 校验在前（[`validate`]），建子窗口在后；失败一律 [`WebViewError::Backend`]，由上层决定「只记
/// 日志」——WebView2 运行时缺失不该挡住应用启动（与字体、渲染器同口径）。
pub(super) fn build_overlay(
  parent: &Arc<dyn WindowHandle>,
  config: &WebViewConfig,
  rect: LogicalRect,
) -> Result<Box<dyn WebViewHandle>, WebViewError> {
  validate(config)?;

  let builder = ::wry::WebViewBuilder::new()
    .with_bounds(wry_bounds(rect))
    .with_transparent(config.transparent)
    .with_visible(true)
    .with_devtools(config.devtools);
  let builder = match &config.source {
    WebViewSource::Url(url) => builder.with_url(url.as_str()),
    WebViewSource::Html(html) => builder.with_html(html.as_str()),
  };

  let view = builder
    .build_as_child(parent)
    .map_err(|error| WebViewError::Backend(error.to_string()))?;

  tracing::debug!(
    x = rect.x,
    y = rect.y,
    width = rect.width,
    height = rect.height,
    "建覆盖层"
  );

  Ok(Box::new(Overlay {
    view: Some(view),
    rect,
  }))
}

/// 逻辑矩形 → wry 的 `Rect`。给**逻辑**值：DPI 换算是 wry 自己的事。
fn wry_bounds(rect: LogicalRect) -> ::wry::Rect {
  ::wry::Rect {
    position: ::wry::dpi::LogicalPosition::new(rect.x, rect.y).into(),
    size: ::wry::dpi::LogicalSize::new(rect.width, rect.height).into(),
  }
}

/// wry 的覆盖层：把 `wry::WebView` 收进 [`WebViewHandle`] 的形状。
///
/// `view` 是 `Option`：`close` 靠 `take` 真把原生资源放掉（wry 没有 `close`，释放就是 Drop）。
/// 矩形自己记一份——它是我们设进去的意图，比回头问 wry 更直接。
struct Overlay {
  view: Option<::wry::WebView>,
  rect: LogicalRect,
}

impl Overlay {
  /// 拿到活的视图；已经 `close` 过就返回 `None`。
  fn view(&self) -> Option<&::wry::WebView> {
    self.view.as_ref()
  }
}

impl WebViewHandle for Overlay {
  fn mode(&self) -> Mode {
    Mode::Overlay
  }

  fn bounds(&self) -> LogicalRect {
    self.rect
  }

  /// 视口 / DPI 变化后重算摆位时走它。失败只记日志：覆盖层错位比让启动失败轻。
  fn set_bounds(&mut self, rect: LogicalRect) {
    let Some(view) = self.view() else {
      return;
    };
    match view.set_bounds(wry_bounds(rect)) {
      Ok(()) => {
        self.rect = rect;
        tracing::trace!(x = rect.x, y = rect.y, "移动覆盖层");
      }
      Err(error) => tracing::warn!(%error, "移动覆盖层失败"),
    }
  }

  fn set_visible(&mut self, visible: bool) {
    let Some(view) = self.view() else {
      return;
    };
    match view.set_visible(visible) {
      Ok(()) => tracing::debug!(visible, "覆盖层可见性"),
      Err(error) => tracing::warn!(%error, "切换覆盖层可见性失败"),
    }
  }

  fn navigate(&mut self, url: &str) {
    let Some(view) = self.view() else {
      return;
    };
    if let Err(error) = view.load_url(url) {
      tracing::warn!(%error, url, "覆盖层导航失败");
    }
  }

  fn evaluate_script(&mut self, js: &str) {
    let Some(view) = self.view() else {
      return;
    };
    if let Err(error) = view.evaluate_script(js) {
      tracing::warn!(%error, "覆盖层执行脚本失败");
    }
  }

  fn close(&mut self) {
    if self.view.take().is_some() {
      tracing::debug!("关覆盖层");
    }
  }
}

#[cfg(test)]
mod tests {
  use super::*;
  use crate::capability::webview::SandboxPolicy;

  /// 能力与「Windows 上沙箱恒可用」这条口径一致：默认配置不会被 fail-closed 挡住。
  #[test]
  fn the_windows_backend_has_an_overlay_and_a_sandbox() {
    let caps = capabilities();

    assert!(caps.overlay, "Windows 走 WebView2");
    assert!(!caps.texture, "纹理层是 CEF 的活，尚未实装");
    assert!(caps.sandbox, "WebView2 的内容在独立进程里");
    assert_eq!(caps.devtools, cfg!(debug_assertions));

    let config = WebViewConfig::html("<p>hi</p>");
    assert_eq!(config.sandbox, SandboxPolicy::Required);
    assert!(validate(&config).is_ok(), "默认配置不该被 fail-closed 挡住");
  }

  /// 摆位是**逻辑**值交给 wry 的，DPI 换算不在我们这一侧。
  #[test]
  fn bounds_are_handed_to_wry_as_logical_values() {
    let rect = LogicalRect::new(944.0, 504.0, 320.0, 200.0);
    let bounds = wry_bounds(rect);

    match bounds.position {
      ::wry::dpi::Position::Logical(position) => {
        assert_eq!([position.x, position.y], [944.0, 504.0]);
      }
      other => panic!("该按逻辑位置交出去，实际是 {other:?}"),
    }
    match bounds.size {
      ::wry::dpi::Size::Logical(size) => {
        assert_eq!([size.width, size.height], [320.0, 200.0]);
      }
      other => panic!("该按逻辑尺寸交出去，实际是 {other:?}"),
    }
  }
}
