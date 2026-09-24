//! 运行时：`App` 契约、帧循环、外部事件源泵、作业调度。
//!
//! **等待权在本层与平台手里**：帧节奏由 [`FrameClock`] 定，外部事件源按自己声明的截止
//! 时间被泵。任何异步机制都不得与之争抢等待（见 `docs/architecture.md` 任务与异步）。

mod clock;
mod jobs;
mod pump;

pub use clock::{Frame, FrameClock};
pub use jobs::{JobContext, JobHandle, JobPool};
pub use pump::{EventSource, PumpContext, Wakeup};

use crate::core::Error;
use crate::core::event::Event;
use crate::core::window::{WindowDesc, WindowHandle};
use crate::platform;
use std::sync::Arc;
use std::time::{Duration, Instant};

/// 默认目标帧率。
pub const DEFAULT_FPS: u32 = 60;

/// 应用。
///
/// 钩子全部在**主线程**上调用，顺序为：`windows` → `on_startup` →（`on_event` /
/// `on_frame`）反复 → `on_shutdown`。
pub trait App: 'static {
  /// 启动时要创建的窗口。默认一个默认窗口；无窗口应用覆写为空。
  fn windows(&self) -> Vec<WindowDesc> {
    vec![WindowDesc::default()]
  }

  /// 目标帧间隔；`None` = 不限速（交给 vsync / present mode）。
  fn frame_interval(&self) -> Option<Duration> {
    Some(Duration::from_nanos(1_000_000_000 / u64::from(DEFAULT_FPS)))
  }

  /// 窗口就绪、进入帧循环前调用一次。
  fn on_startup(&mut self, _cx: &mut AppContext) {}

  /// 运行时事件。
  fn on_event(&mut self, _cx: &mut AppContext, _event: &Event) {}

  /// 帧边界。渲染、模拟、任务收割都挂在这里。
  fn on_frame(&mut self, _cx: &mut AppContext, _frame: Frame) {}

  /// 事件循环退出前调用一次。
  fn on_shutdown(&mut self, _cx: &mut AppContext) {}
}

/// 启动应用：把控制权交给平台后端的事件循环，返回时应用已退出。
pub fn run<A: App>(app: A) -> Result<(), Error> {
  platform::run(app)
}

/// 帧 / 事件上下文：应用与运行时、平台之间的调用面。
///
/// 窗口只以 [`WindowHandle`] 的形式露出——契约层与运行时都不认识 winit 的 `Window`。
pub struct AppContext {
  windows: Vec<Arc<dyn WindowHandle>>,
  sources: Vec<Box<dyn EventSource>>,
  wakeup: Arc<dyn Wakeup>,
  exit_requested: bool,
}

impl AppContext {
  pub(crate) fn new(wakeup: Arc<dyn Wakeup>) -> Self {
    Self {
      windows: Vec::new(),
      sources: Vec::new(),
      wakeup,
      exit_requested: false,
    }
  }

  /// 全部窗口。
  pub fn windows(&self) -> &[Arc<dyn WindowHandle>] {
    &self.windows
  }

  /// 主窗口（第一个创建的窗口）。
  pub fn main_window(&self) -> Option<&Arc<dyn WindowHandle>> {
    self.windows.first()
  }

  /// 按 id 取窗口。
  pub fn window(&self, id: crate::core::window::WindowId) -> Option<&Arc<dyn WindowHandle>> {
    self.windows.iter().find(|window| window.id() == id)
  }

  /// 跨线程唤醒句柄：别的线程拿它请求运行时立刻醒一次。
  pub fn wakeup(&self) -> Arc<dyn Wakeup> {
    Arc::clone(&self.wakeup)
  }

  /// 注册外部事件源；注册后由运行时按源声明的截止时间泵。
  pub fn register_event_source(&mut self, source: impl EventSource) {
    self.sources.push(Box::new(source));
  }

  /// 请求退出事件循环：当前帧 / 事件处理结束后运行时退出。
  pub fn exit(&mut self) {
    self.exit_requested = true;
  }

  /// 是否已请求退出。
  pub fn exit_requested(&self) -> bool {
    self.exit_requested
  }

  pub(crate) fn push_window(&mut self, window: Arc<dyn WindowHandle>) {
    self.windows.push(window);
  }

  pub(crate) fn clear_windows(&mut self) {
    self.windows.clear();
  }

  /// 是否有事件源的截止时间已到。
  pub(crate) fn source_due(&self, now: Instant) -> bool {
    self
      .sources
      .iter()
      .any(|source| source.next_deadline().is_some_and(|due| due <= now))
  }

  /// 最早的源截止时间。
  pub(crate) fn next_source_deadline(&self) -> Option<Instant> {
    self
      .sources
      .iter()
      .filter_map(|source| source.next_deadline())
      .min()
  }

  /// 泵一遍所有事件源；返回是否有源产生了待处理的东西。
  pub(crate) fn pump_sources(&mut self, now: Instant) -> bool {
    // 分成两次字段借用，避开 `self.sources` 的可变借用与 `self.wakeup` 的不可变借用冲突。
    let wakeup: &dyn Wakeup = &*self.wakeup;
    let mut woke = false;
    for source in &mut self.sources {
      woke |= source.pump(&mut PumpContext::new(now, wakeup));
    }
    woke
  }

  /// 取走退出请求。
  pub(crate) fn take_exit_requested(&mut self) -> bool {
    std::mem::take(&mut self.exit_requested)
  }
}
