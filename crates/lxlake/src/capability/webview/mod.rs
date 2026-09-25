//! webview：原生覆盖层（wry 子窗口）与将来的纹理层（CEF）的**接口层**。
//!
//! 本层**常编译**——配置、能力查询、错误、句柄 trait 都在这里，与有没有后端无关；真正的后端关在
//! `webview-wry` 特性之后，没有后端时由一份「垫底」实装接住（能力全 `false`、创建一律拒）。
//!
//! 两种呈现形态：
//!
//! - **覆盖层**（[`Mode::Overlay`]）：原生子窗口，永远浮在画面与自绘 UI 之上，不参与 z 序与裁剪。
//!   摆位数据与自绘**共用同一份** [`Widget`]，矩形由 `ui::place` 算出——摆位实现只有一份。
//! - **纹理层**（[`Mode::Texture`]）：离屏成纹理再交给渲染管线绘制（CEF，未实装）。
//!
//! 沙箱默认 **fail-closed**：[`SandboxPolicy::Required`] 是默认值，后端给不出沙箱就拒建，而不是
//! 悄悄降级（见 [`validate`]）。

use crate::core::geometry::LogicalRect;
use crate::core::widget::Widget;
use crate::core::window::{WindowHandle, WindowLabel};
use std::fmt;
use std::sync::Arc;

// 后端只按 `cfg` 挂载：**不是**「同 target 上的可选能力」（那是 feature 的活），而是「这个构建里
// 有没有 webview 后端」。两份实装只在 [`capabilities`] 与 `build_overlay` 上分岔，公开入口
// [`create_overlay`] 由本层统一给——读的人只需要认一个签名。
#[cfg(not(all(feature = "webview-wry", target_os = "windows")))]
mod unsupported;
#[cfg(all(feature = "webview-wry", target_os = "windows"))]
mod wry;

#[cfg(not(all(feature = "webview-wry", target_os = "windows")))]
pub use unsupported::capabilities;
#[cfg(all(feature = "webview-wry", target_os = "windows"))]
pub use wry::capabilities;

/// 覆盖层身份。应用自己编号，运行时只按它记账（与 `UiId` 同类）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct OverlayId(pub u32);

/// webview 装什么。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WebViewSource {
  /// 远程或本地 URL。
  Url(String),
  /// 内联 HTML（本轮 demo 用它，不引资产目录）。
  Html(String),
}

/// 沙箱策略。
///
/// 默认 [`Required`](Self::Required)——**fail-closed**：后端给不出沙箱就拒建。不允许以「关掉沙箱」
/// 交付（见 `docs/architecture.md` 的 webview 双线），[`Disabled`](Self::Disabled) 只留给调试构建
/// 下的排障。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SandboxPolicy {
  #[default]
  Required,
  Disabled,
}

/// 一个 webview 的创建参数。
#[derive(Debug, Clone, PartialEq)]
pub struct WebViewConfig {
  pub source: WebViewSource,
  /// 背景透出下层：覆盖层叠在 3D 画面上时要它。
  pub transparent: bool,
  /// 开开发者工具。
  pub devtools: bool,
  pub sandbox: SandboxPolicy,
}

impl WebViewConfig {
  /// 内联 HTML。
  pub fn html(html: impl Into<String>) -> Self {
    Self::from_source(WebViewSource::Html(html.into()))
  }

  /// URL。
  pub fn url(url: impl Into<String>) -> Self {
    Self::from_source(WebViewSource::Url(url.into()))
  }

  pub fn transparent(mut self, transparent: bool) -> Self {
    self.transparent = transparent;
    self
  }

  pub fn devtools(mut self, devtools: bool) -> Self {
    self.devtools = devtools;
    self
  }

  pub fn sandbox(mut self, sandbox: SandboxPolicy) -> Self {
    self.sandbox = sandbox;
    self
  }

  /// 沙箱按默认（[`SandboxPolicy::Required`]），其余两项关掉。
  fn from_source(source: WebViewSource) -> Self {
    Self {
      source,
      transparent: false,
      devtools: false,
      sandbox: SandboxPolicy::default(),
    }
  }
}

/// 呈现形态。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
  /// 原生子窗口，永远浮在最上。
  Overlay,
  /// 离屏成纹理，交给渲染管线（未实装）。
  Texture,
}

/// 当前构建与平台**支持什么**。能力以查询形式暴露，调用方不必去猜 feature 开了没有。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Capabilities {
  pub overlay: bool,
  pub texture: bool,
  /// 沙箱是否可用。`false` 时 [`SandboxPolicy::Required`] 一律拒建（fail-closed）。
  pub sandbox: bool,
  pub devtools: bool,
}

impl Capabilities {
  /// 什么都不支持——没有后端的构建的答案。
  pub const fn none() -> Self {
    Self {
      overlay: false,
      texture: false,
      sandbox: false,
      devtools: false,
    }
  }

  /// 这个形态能不能建。
  pub const fn supports(self, mode: Mode) -> bool {
    match mode {
      Mode::Overlay => self.overlay,
      Mode::Texture => self.texture,
    }
  }
}

/// 建 webview 失败。
#[derive(Debug)]
pub enum WebViewError {
  /// 当前构建 / 平台没有这个形态的后端。
  Unsupported {
    /// 平台名（`std::env::consts::OS`），报错里点名省得去猜。
    platform: &'static str,
    mode: Mode,
  },
  /// 配置要求沙箱，而当前后端给不出。
  SandboxUnsupported,
  /// 后端自己失败：运行时缺失、建子窗口被拒等。
  Backend(String),
}

impl fmt::Display for WebViewError {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    match self {
      Self::Unsupported { platform, mode } => {
        let mode = match mode {
          Mode::Overlay => "覆盖层",
          Mode::Texture => "纹理层",
        };
        write!(f, "{platform} 上没有{mode}形态的 webview 后端")
      }
      Self::SandboxUnsupported => write!(f, "当前 webview 后端给不出沙箱，按 fail-closed 拒建"),
      Self::Backend(message) => write!(f, "webview 后端失败：{message}"),
    }
  }
}

impl std::error::Error for WebViewError {}

/// 一个已建好的 webview。
///
/// 读用 `&self`、改用 `&mut self`；**不给 `Send + Sync`**——后端句柄是独占的，且只在主线程上用
/// （运行时按 [`WindowId`](crate::core::window::WindowId) 记账，见 `App` 的覆盖层表）。
pub trait WebViewHandle {
  fn mode(&self) -> Mode;

  /// 当前矩形（逻辑像素）。
  fn bounds(&self) -> LogicalRect;

  /// 改矩形。视口变化 / DPI 变化后重算摆位时走它。
  fn set_bounds(&mut self, rect: LogicalRect);

  fn set_visible(&mut self, visible: bool);

  fn navigate(&mut self, url: &str);

  fn evaluate_script(&mut self, js: &str);

  /// 释放原生资源。调用后这个句柄不该再用——运行时靠 `Option::take` 真正丢掉它。
  fn close(&mut self);
}

/// 配置校验：这份配置在当前后端的能力下能不能被满足。
///
/// **只看配置**——「有没有后端」由 [`create_overlay`] 答，而且它的答案更靠前（没有后端就直接答
/// `Unsupported`，不走到这里），以免把「这个构建里根本没有 webview」误报成「沙箱不支持」。
///
/// 公开是刻意的：应用能在装配期先问一句，而不是等建窗时才发现配置在这个构建上没戏；公开项也
/// 不会在没有后端的构建里变成死代码（本仓 `-D warnings`）。
pub fn validate(config: &WebViewConfig) -> Result<(), WebViewError> {
  if config.sandbox == SandboxPolicy::Required && !capabilities().sandbox {
    return Err(WebViewError::SandboxUnsupported);
  }
  Ok(())
}

/// 装配期的覆盖层声明：**装在哪、装什么、摆在哪**。
///
/// `widget` 是 [`Widget`] 本体而不是算好的矩形：摆位数据因此与自绘共用一份，矩形在运行期按视口
/// 由 `ui::place` 算出。`overlay` 标记由 [`OverlaySpec::new`] 负责打上——覆盖层那块矩形在命中
/// 测试里整组优先。
#[derive(Debug, Clone, PartialEq)]
pub struct OverlaySpec {
  pub id: OverlayId,
  /// 挂哪个窗口（按标签）。默认主窗口。
  pub window: WindowLabel,
  pub config: WebViewConfig,
  pub widget: Widget,
}

impl OverlaySpec {
  /// 落在**主窗口**上的覆盖层。
  pub fn new(id: OverlayId, widget: Widget, config: WebViewConfig) -> Self {
    Self {
      id,
      window: WindowLabel::new(WindowLabel::MAIN),
      config,
      widget: widget.overlay(),
    }
  }

  /// 改挂到别的窗口。
  pub fn window(mut self, label: impl Into<WindowLabel>) -> Self {
    self.window = label.into();
    self
  }
}

/// 建一个覆盖层：父窗口句柄是**唯一的平台缝**，走 `raw-window-handle` 那一套中性 ABI。
///
/// 没有后端 / 非 Windows 一律 `Err(`[`WebViewError::Unsupported`]`)`，而且**先答这一条**——把
/// 「这个构建里根本没有 webview」误报成「沙箱不支持」会把排查引到错的地方。有后端时先 [`validate`]
/// 再建；后端自身失败是 [`WebViewError::Backend`]，由上层决定「只记日志」（与字体、渲染器同口径）。
pub fn create_overlay(
  parent: &Arc<dyn WindowHandle>,
  config: &WebViewConfig,
  rect: LogicalRect,
) -> Result<Box<dyn WebViewHandle>, WebViewError> {
  #[cfg(all(feature = "webview-wry", target_os = "windows"))]
  let built = wry::build_overlay(parent, config, rect);
  #[cfg(not(all(feature = "webview-wry", target_os = "windows")))]
  let built = unsupported::build_overlay(parent, config, rect);
  built
}

#[cfg(test)]
mod tests {
  use super::*;
  use crate::core::geometry::{LogicalPosition, LogicalSize};
  use crate::core::widget::{Anchor, UiId};
  use crate::ui::{UiTree, place};

  #[test]
  fn mode_support_follows_the_capabilities() {
    let caps = Capabilities {
      overlay: true,
      texture: false,
      sandbox: true,
      devtools: false,
    };

    assert!(caps.supports(Mode::Overlay));
    assert!(!caps.supports(Mode::Texture));

    let none = Capabilities::none();
    assert!(!none.supports(Mode::Overlay));
    assert!(!none.supports(Mode::Texture));
  }

  /// fail-closed：默认配置要求沙箱，而后端给不出就拒。这条断言在有无后端的构建里都成立——
  /// 有后端且沙箱可用时它就是 `Ok`。
  #[test]
  fn requiring_sandbox_tracks_the_backend_fail_closed() {
    let required = WebViewConfig::html("<p>hi</p>");
    assert_eq!(
      validate(&required).is_ok(),
      capabilities().sandbox,
      "`Required` 的结论必须与后端的沙箱能力一致"
    );

    assert!(
      validate(&required.sandbox(SandboxPolicy::Disabled)).is_ok(),
      "显式关掉沙箱的配置不因沙箱被拒"
    );
  }

  /// 覆盖层与自绘算出的矩形必须**逐位相同**——这是「契约层共用一份摆位数据」的可机检形式。
  /// 哪天有人给覆盖层另写一套布局，这条会先红。
  #[test]
  fn overlay_and_self_drawn_share_one_placement() {
    let widget = Widget::new(UiId(7), Anchor::BottomRight, LogicalSize::new(320.0, 200.0))
      .offset([-16.0, -16.0]);
    let viewport = LogicalSize::new(1280.0, 720.0);

    let mut tree = UiTree::new();
    tree.add(widget.clone());
    tree.layout(viewport);

    let rect = place(&widget, viewport);
    assert_eq!(tree.rect_of(widget.id), Some(rect));
    assert_eq!(rect, LogicalRect::new(944.0, 504.0, 320.0, 200.0));
    assert!(rect.contains(LogicalPosition::new(960.0, 520.0)));
  }

  #[test]
  fn an_overlay_spec_lands_on_the_main_window_and_is_marked_as_overlay() {
    let spec = OverlaySpec::new(
      OverlayId(1),
      Widget::new(UiId(1), Anchor::BottomRight, LogicalSize::new(320.0, 200.0)),
      WebViewConfig::html("<p>hi</p>"),
    );

    assert!(spec.window.is_main());
    assert!(spec.widget.overlay, "覆盖层标记由 OverlaySpec 打上");
    assert_eq!(
      spec.config.sandbox,
      SandboxPolicy::Required,
      "默认 fail-closed"
    );

    let moved = spec.window("inspector");
    assert_eq!(moved.window.as_str(), "inspector");
  }
}
