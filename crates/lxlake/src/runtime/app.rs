//! 应用容器与上下文。
//!
//! [`App`] 是**用户面向**的状态容器：[`AppContext`]（窗口、事件源、唤醒、退出）与托管状态
//! （`manage` / `get` / `with_state`）都在它身上。于是生命周期闭包只拿一个 `&mut App` 就能把事
//! 办完——不必再多传一个上下文参数，也就不会出现「状态与上下文各借一半」的别扭签名。
//!
//! 平台侧不认识 [`App`]，它只认 [`Application`](super::Application)。

use super::pump::{EventSource, PumpContext, Wakeup};
use crate::core::window::{WindowHandle, WindowId};
use std::any::{Any, TypeId};
use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Instant;

/// 未注入唤醒句柄时的兜底：唤醒无处可去，丢弃即可。
struct NoopWakeup;

impl Wakeup for NoopWakeup {
  fn wake(&self) {}
}

static NOOP_WAKEUP: NoopWakeup = NoopWakeup;

/// 帧 / 事件上下文：应用与运行时、平台之间的调用面。
///
/// 窗口只以 [`WindowHandle`] 的形式露出——契约层与运行时都不认识 winit 的 `Window`。
pub struct AppContext {
  windows: Vec<Arc<dyn WindowHandle>>,
  sources: Vec<Box<dyn EventSource>>,
  /// 跨线程唤醒句柄。**要等事件循环建好才有**，故由平台在建循环之后注入
  /// （见 [`Application::attach_wakeup`](super::Application::attach_wakeup)）。
  wakeup: Option<Arc<dyn Wakeup>>,
  exit_requested: bool,
}

impl AppContext {
  pub(crate) fn new() -> Self {
    Self {
      windows: Vec::new(),
      sources: Vec::new(),
      wakeup: None,
      exit_requested: false,
    }
  }

  /// 注入唤醒句柄；平台在事件循环建好后调用一次。
  pub(crate) fn set_wakeup(&mut self, wakeup: Arc<dyn Wakeup>) {
    self.wakeup = Some(wakeup);
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
  pub fn window(&self, id: WindowId) -> Option<&Arc<dyn WindowHandle>> {
    self.windows.iter().find(|window| window.id() == id)
  }

  /// 跨线程唤醒句柄：别的线程拿它请求运行时立刻醒一次。
  ///
  /// # Panics
  /// 唤醒句柄由平台在事件循环建好后注入，应用在 `on_startup` 之前调它属于框架时序被破坏。
  pub fn wakeup(&self) -> Arc<dyn Wakeup> {
    self
      .wakeup
      .clone()
      .expect("唤醒句柄尚未注入：它由平台在事件循环建好后提供")
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
    // 只借 `wakeup` 字段而不是整个 `self`，避开与 `self.sources` 可变借用的冲突。
    let wakeup: &dyn Wakeup = self.wakeup.as_deref().unwrap_or(&NOOP_WAKEUP);
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

/// 应用：托管状态 + 上下文。
///
/// 生命周期闭包拿到的就是它。状态一律经 [`App::manage`] 登记、按类型取回——应用自己的类型由
/// 应用定义，框架不规定它的形状。
pub struct App {
  cx: AppContext,
  /// 托管状态：按类型存一份。生命周期闭包跨帧访问同一份状态就走这里。
  managed: BTreeMap<TypeId, Box<dyn Any + Send + Sync>>,
}

impl App {
  pub(crate) fn new() -> Self {
    Self {
      cx: AppContext::new(),
      managed: BTreeMap::new(),
    }
  }

  pub(crate) fn context_mut(&mut self) -> &mut AppContext {
    &mut self.cx
  }

  pub(crate) fn attach_wakeup(&mut self, wakeup: Arc<dyn Wakeup>) {
    self.cx.set_wakeup(wakeup);
  }

  /// 主窗口（第一个创建的窗口）。
  pub fn main_window(&self) -> Option<&Arc<dyn WindowHandle>> {
    self.cx.main_window()
  }

  /// 按 id 取窗口。
  pub fn window(&self, id: WindowId) -> Option<&Arc<dyn WindowHandle>> {
    self.cx.window(id)
  }

  /// 全部窗口。
  pub fn windows(&self) -> &[Arc<dyn WindowHandle>] {
    self.cx.windows()
  }

  /// 请求退出事件循环。
  pub fn exit(&mut self) {
    self.cx.exit();
  }

  /// 是否已请求退出。
  pub fn exit_requested(&self) -> bool {
    self.cx.exit_requested()
  }

  /// 跨线程唤醒句柄（见 [`AppContext::wakeup`]）。
  pub fn wakeup(&self) -> Arc<dyn Wakeup> {
    self.cx.wakeup()
  }

  /// 注册外部事件源。
  pub fn register_event_source(&mut self, source: impl EventSource) {
    self.cx.register_event_source(source);
  }

  /// 托管一份状态；同类型再登记会覆盖前一份。
  pub fn manage<T: Any + Send + Sync>(&mut self, value: T) {
    self.managed.insert(TypeId::of::<T>(), Box::new(value));
  }

  /// 按类型取托管状态。
  pub fn get<T: Any + Send + Sync>(&self) -> Option<&T> {
    self.managed.get(&TypeId::of::<T>())?.downcast_ref()
  }

  /// 按类型取可变托管状态。
  pub fn get_mut<T: Any + Send + Sync>(&mut self) -> Option<&mut T> {
    self
      .managed
      .get_mut(&TypeId::of::<T>())?
      .downcast_mut::<T>()
  }

  /// 取托管状态；没登记过就按 `Default` 建一份。
  pub fn state<T: Default + Any + Send + Sync>(&mut self) -> &mut T {
    self
      .managed
      .entry(TypeId::of::<T>())
      .or_insert_with(|| Box::<T>::default())
      .downcast_mut::<T>()
      .expect("托管状态的键与值类型一致")
  }

  /// **同时**取托管状态与上下文。
  ///
  /// 状态与上下文都在 `App` 里，直接给两份可变借用是给不出来的；所以这里把状态**临时取出来**：
  /// 闭包期间它与 `&mut AppContext` 是两个互不相干的借用，闭包返回后再放回去。
  pub fn with_state<T: Any + Send + Sync, R>(
    &mut self,
    f: impl FnOnce(&mut T, &mut AppContext) -> R,
  ) -> Option<R> {
    let mut state = self.managed.remove(&TypeId::of::<T>())?;
    let result = f(
      state.downcast_mut::<T>().expect("托管状态的键与值类型一致"),
      &mut self.cx,
    );
    self.managed.insert(TypeId::of::<T>(), state);
    Some(result)
  }
}

#[cfg(test)]
mod tests {
  use super::*;
  use std::sync::atomic::{AtomicUsize, Ordering};
  use std::time::Duration;

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
    let mut cx = AppContext::new();
    cx.set_wakeup(Arc::clone(&wakeup) as Arc<dyn Wakeup>);
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

  /// 唤醒句柄还没注入时泵源不该炸：此时唤醒无处可去，丢弃即可。
  #[test]
  fn pumping_before_the_wakeup_is_injected_is_harmless() {
    let now = Instant::now();
    let (loud, _) = source_at(Some(now), true);
    let mut cx = AppContext::new();
    cx.register_event_source(loud);

    assert!(cx.pump_sources(now));
  }

  /// 托管状态：登记、按类型取回、同类型覆盖。
  #[test]
  fn managed_state_is_keyed_by_type() {
    #[derive(Debug, PartialEq)]
    struct Counter(u32);
    struct Label(&'static str);

    let mut app = App::new();
    app.manage(Counter(1));
    app.manage(Label("demo"));

    assert_eq!(app.get::<Counter>(), Some(&Counter(1)));
    assert_eq!(app.get::<Label>().map(|label| label.0), Some("demo"));
    assert_eq!(app.get::<u32>(), None, "没登记过的类型取不到");

    app.get_mut::<Counter>().expect("刚登记过").0 = 2;
    assert_eq!(app.get::<Counter>(), Some(&Counter(2)));

    app.manage(Counter(9));
    assert_eq!(
      app.get::<Counter>(),
      Some(&Counter(9)),
      "同类型再登记即覆盖"
    );
  }

  #[test]
  fn state_is_lazily_created_from_default() {
    let mut app = App::new();

    assert_eq!(app.get::<u32>(), None);
    assert_eq!(*app.state::<u32>(), 0, "没登记过就按 Default 建一份");
    *app.state::<u32>() = 7;
    assert_eq!(app.get::<u32>(), Some(&7), "建出来的那份留下都留着");
  }

  /// `with_state` 是「状态 + 上下文」同时可变借用的唯一入口。
  #[test]
  fn with_state_hands_out_state_and_context_at_once() {
    #[derive(Default)]
    struct Demo {
      frames: u32,
    }

    let mut app = App::new();
    app.manage(Demo::default());

    let seen = app.with_state::<Demo, _>(|demo, cx| {
      demo.frames += 1;
      cx.exit();
      demo.frames
    });

    assert_eq!(seen, Some(1));
    assert_eq!(app.get::<Demo>().map(|demo| demo.frames), Some(1));
    assert!(app.exit_requested(), "闭包里的退出请求落在同一个上下文上");
    assert_eq!(
      app.with_state::<u32, _>(|_, _| ()),
      None,
      "没登记过就是 None"
    );
  }
}
