//! 运行时：应用装配（[`Builder`]）、平台驱动契约（[`Application`]）、帧循环、外部事件源泵、
//! 作业调度。
//!
//! 两层应用对象，别混：
//!
//! - [`App`] —— **用户面向**的状态容器：托管状态 + [`AppContext`]。生命周期闭包拿到的是它。
//! - [`Application`] —— **平台面向**的内部契约，由 [`Builder`] 满足，应用侧不实现它。
//!
//! **等待权在本层与平台手里**：帧节奏由 [`FrameClock`] 定，外部事件源按自己声明的截止
//! 时间被泵。任何异步机制都不得与之争抢等待（见 `docs/architecture.md` 任务与异步）。

mod app;
mod builder;
mod clock;
mod jobs;
mod pump;

pub use app::{App, AppContext};
pub use builder::Builder;
pub use clock::{Frame, FrameClock};
pub use jobs::{JobContext, JobHandle, JobPool};
pub use pump::{EventSource, PumpContext, Wakeup};

use crate::core::Error;
use crate::core::event::Event;
use crate::core::window::{WindowDesc, WindowId};
use crate::platform;
use std::sync::Arc;
use std::time::Duration;

/// 默认目标帧率。
pub const DEFAULT_FPS: u32 = 60;

/// 平台驱动的**内部**契约：由 [`Builder`] 满足，应用侧不实现它。
///
/// 钩子全部在**主线程**上调用，顺序为：`attach_wakeup` → `windows` → `on_startup` →
///（`on_event` / `on_frame`）反复 → `on_shutdown`。窗口相关的两个钩子在建窗 / 摘窗的当口调用。
#[doc(hidden)]
pub trait Application: 'static {
  /// 注入跨线程唤醒句柄。
  ///
  /// 应用对象在事件循环**之前**就已构造，而唤醒句柄要等循环建好才有，所以必须补这一步。
  fn attach_wakeup(&mut self, wakeup: Arc<dyn Wakeup>);

  /// 运行时 / 平台要用的上下文。
  fn context_mut(&mut self) -> &mut AppContext;

  /// 启动时要创建的窗口。
  fn windows(&self) -> Vec<WindowDesc>;

  /// 目标帧间隔；`None` = 不限速。
  fn frame_interval(&self) -> Option<Duration>;

  /// 某个窗口刚建好。
  fn on_window_ready(&mut self, id: WindowId);

  /// 某个窗口被摘掉。
  fn on_window_destroyed(&mut self, id: WindowId);

  /// 窗口就绪、进入帧循环前调用一次。
  fn on_startup(&mut self);

  /// 运行时事件。
  fn on_event(&mut self, event: &Event);

  /// 帧边界。
  fn on_frame(&mut self, frame: Frame);

  /// 事件循环退出前调用一次。
  fn on_shutdown(&mut self);
}

/// 启动应用：把控制权交给平台后端的事件循环，返回时应用已退出。
pub fn run(app: impl Application) -> Result<(), Error> {
  platform::run(app)
}
