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

#[cfg(test)]
mod tests {
  use super::*;
  use std::sync::atomic::{AtomicUsize, Ordering};

  /// 只记唤醒次数。
  #[derive(Default)]
  struct CountingWakeup(AtomicUsize);

  impl Wakeup for CountingWakeup {
    fn wake(&self) {
      self.0.fetch_add(1, Ordering::SeqCst);
    }
  }

  /// 假源的记账。
  ///
  /// 源被 `Box<dyn EventSource>` 吞进去之后从外面读不到了，所以账目放在 `Arc` 上。
  #[derive(Default)]
  struct Log {
    pumps: AtomicUsize,
    wakes: AtomicUsize,
  }

  struct FakeSource {
    deadline: Option<Instant>,
    produces: bool,
    log: Arc<Log>,
  }

  impl EventSource for FakeSource {
    fn next_deadline(&self) -> Option<Instant> {
      self.deadline
    }

    fn pump(&mut self, cx: &mut PumpContext<'_>) -> bool {
      self.log.pumps.fetch_add(1, Ordering::SeqCst);
      if self.produces {
        self.log.wakes.fetch_add(1, Ordering::SeqCst);
        cx.wake();
      }
      self.produces
    }
  }

  fn source_at(deadline: Option<Instant>, produces: bool) -> (FakeSource, Arc<Log>) {
    let log = Arc::new(Log::default());
    (
      FakeSource {
        deadline,
        produces,
        log: Arc::clone(&log),
      },
      log,
    )
  }

  fn context() -> (AppContext, Arc<CountingWakeup>) {
    let wakeup = Arc::new(CountingWakeup::default());
    let cx = AppContext::new(Arc::clone(&wakeup) as Arc<dyn Wakeup>);
    (cx, wakeup)
  }

  #[test]
  fn without_sources_there_is_no_deadline() {
    let (cx, _) = context();
    assert_eq!(cx.next_source_deadline(), None);
    assert!(!cx.source_due(Instant::now()));
  }

  /// 运行时取最早的截止时间：这就是 `ControlFlow::WaitUntil` 有机会按 ~10ms 醒的前提
  /// （wry / CEF 的泵频率要求落在这条上）。只依赖帧边界的源不参与。
  #[test]
  fn only_the_earliest_deadline_matters() {
    let (mut cx, _) = context();
    let now = Instant::now();
    let (fast, _) = source_at(Some(now + Duration::from_millis(10)), false);
    let (slow, _) = source_at(Some(now + Duration::from_millis(30)), false);
    let (passive, _) = source_at(None, false);
    cx.register_event_source(fast);
    cx.register_event_source(slow);
    cx.register_event_source(passive);

    assert_eq!(
      cx.next_source_deadline(),
      Some(now + Duration::from_millis(10))
    );
  }

  #[test]
  fn a_source_is_due_at_or_after_its_deadline() {
    let now = Instant::now();
    let due = now + Duration::from_millis(10);
    let (source, _) = source_at(Some(due), false);
    let (mut cx, _) = context();
    cx.register_event_source(source);

    assert!(!cx.source_due(now));
    assert!(!cx.source_due(due - Duration::from_millis(1)));
    assert!(cx.source_due(due));
    assert!(cx.source_due(due + Duration::from_millis(5)));
  }

  /// 泵是「**至少**这么勤」：任何一个源到期，运行时会泵掉**全部**源。
  ///
  /// 这条是接口契约的一部分，实现 `EventSource` 的人要靠它：`pump` 必须廉价、且能被超频
  /// 调用（同一帧里被多泵几次也不能出错）。
  #[test]
  fn one_due_source_pumps_every_source() {
    let now = Instant::now();
    let (fast, fast_log) = source_at(Some(now), false);
    let (slow, slow_log) = source_at(Some(now + Duration::from_millis(20)), false);
    let (mut cx, _) = context();
    cx.register_event_source(fast);
    cx.register_event_source(slow);

    cx.pump_sources(now);

    assert_eq!(fast_log.pumps.load(Ordering::SeqCst), 1);
    assert_eq!(
      slow_log.pumps.load(Ordering::SeqCst),
      1,
      "没到期也被泵了一次"
    );
  }

  #[test]
  fn pump_reports_whether_anything_was_produced() {
    let now = Instant::now();
    let (quiet, quiet_log) = source_at(Some(now), false);
    let (loud, loud_log) = source_at(Some(now), true);
    let (mut cx, wakeup) = context();
    cx.register_event_source(quiet);
    cx.register_event_source(loud);

    // 只要有源产生了东西，运行时就会补一帧。
    assert!(cx.pump_sources(now));
    assert_eq!(quiet_log.wakes.load(Ordering::SeqCst), 0);
    assert_eq!(loud_log.wakes.load(Ordering::SeqCst), 1);
    // 源在 pump 里调的 `cx.wake()` 转发到注册时给的那个唤醒句柄。
    assert_eq!(wakeup.0.load(Ordering::SeqCst), 1);
  }

  #[test]
  fn a_quiet_batch_does_not_ask_for_a_frame() {
    let now = Instant::now();
    let (quiet, _) = source_at(Some(now), false);
    let (mut cx, _) = context();
    cx.register_event_source(quiet);

    assert!(!cx.pump_sources(now));
  }
}
