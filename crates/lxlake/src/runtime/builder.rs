//! Builder：应用的装配面。
//!
//! 应用只声明**要什么**（窗口、帧率、托管状态）与**生命周期要做什么**（闭包），装配顺序、建窗
//! 时机、事件派发、帧节奏由运行时负责。链式配置的返回值仍是 `Builder`，因此
//! `#[lxlake::entry]` 的工厂函数末尾那个值就是「装配好的应用」。
//!
//! 装配期与运行期是**同一份数据**：`Builder` 自己就实现
//! [`Application`](super::Application)（见文件末尾），所以
//! [`runtime::run`](super::run) 直接吃它，不必再造一个中间类型。

use super::app::{App, AppContext};
use super::{Application, DEFAULT_FPS, Frame, Wakeup};
use crate::core::Error;
use crate::core::event::Event;
use crate::core::window::{WindowDesc, WindowId};
use std::any::Any;
use std::sync::Arc;
use std::time::Duration;

/// 生命周期闭包的存储形状（给下面的字段用，省得每一处都写一遍 boxed dyn）。
type StartupHook = Box<dyn FnOnce(&mut App)>;
type EventHook = Box<dyn FnMut(&mut App, &Event)>;
type FrameHook = Box<dyn FnMut(&mut App, Frame)>;
type ShutdownHook = Box<dyn FnOnce(&mut App)>;

/// 应用装配器。
pub struct Builder {
  /// 应用本体：托管状态与上下文在装配期就已经是运行期那一份。
  app: App,
  windows: Vec<WindowDesc>,
  /// 目标帧间隔；`None` = 不限速。
  frame_interval: Option<Duration>,
  on_startup: Option<StartupHook>,
  on_event: Option<EventHook>,
  on_frame: Option<FrameHook>,
  on_shutdown: Option<ShutdownHook>,
}

impl Default for Builder {
  fn default() -> Self {
    Self::new()
  }
}

impl Builder {
  /// 默认：一个默认窗口 + 60 帧的目标帧率。
  pub fn new() -> Self {
    Self {
      app: App::new(),
      windows: vec![WindowDesc::default()],
      frame_interval: Some(Duration::from_nanos(1_000_000_000 / u64::from(DEFAULT_FPS))),
      on_startup: None,
      on_event: None,
      on_frame: None,
      on_shutdown: None,
    }
  }

  /// 追加一个窗口。第一个即主窗口。
  pub fn window(mut self, desc: WindowDesc) -> Self {
    self.windows.push(desc);
    self
  }

  /// 目标帧间隔；`None` = 不限速（交给 present mode / 系统节流）。
  pub fn frame_interval(mut self, interval: Option<Duration>) -> Self {
    self.frame_interval = interval;
    self
  }

  /// 托管一份状态给生命周期闭包用（见 [`App::manage`]）。
  pub fn manage<T: Any + Send + Sync>(mut self, value: T) -> Self {
    self.app.manage(value);
    self
  }

  /// 窗口就绪、进入帧循环前调用一次。
  pub fn on_startup(mut self, f: impl FnOnce(&mut App) + 'static) -> Self {
    self.on_startup = Some(Box::new(f));
    self
  }

  /// 运行时事件。
  pub fn on_event(mut self, f: impl FnMut(&mut App, &Event) + 'static) -> Self {
    self.on_event = Some(Box::new(f));
    self
  }

  /// 帧边界。
  pub fn on_frame(mut self, f: impl FnMut(&mut App, Frame) + 'static) -> Self {
    self.on_frame = Some(Box::new(f));
    self
  }

  /// 事件循环退出前调用一次。
  pub fn on_shutdown(mut self, f: impl FnOnce(&mut App) + 'static) -> Self {
    self.on_shutdown = Some(Box::new(f));
    self
  }

  /// 跑起来，阻塞至应用退出。
  pub fn run(self) -> Result<(), Error> {
    super::run(self)
  }
}

impl Application for Builder {
  fn attach_wakeup(&mut self, wakeup: Arc<dyn Wakeup>) {
    self.app.attach_wakeup(wakeup);
  }

  fn context_mut(&mut self) -> &mut AppContext {
    self.app.context_mut()
  }

  fn windows(&self) -> Vec<WindowDesc> {
    self.windows.clone()
  }

  fn frame_interval(&self) -> Option<Duration> {
    self.frame_interval
  }

  fn on_window_ready(&mut self, _id: WindowId) {}

  fn on_window_destroyed(&mut self, _id: WindowId) {}

  fn on_startup(&mut self) {
    if let Some(f) = self.on_startup.take() {
      f(&mut self.app);
    }
  }

  fn on_event(&mut self, event: &Event) {
    if let Some(f) = &mut self.on_event {
      f(&mut self.app, event);
    }
  }

  fn on_frame(&mut self, frame: Frame) {
    if let Some(f) = &mut self.on_frame {
      f(&mut self.app, frame);
    }
  }

  fn on_shutdown(&mut self) {
    if let Some(f) = self.on_shutdown.take() {
      f(&mut self.app);
    }
  }
}
