//! 应用容器与上下文。
//!
//! [`App`] 是**用户面向**的状态容器：[`AppContext`]（窗口、事件源、唤醒、退出）与托管状态
//! （`manage` / `get` / `with_state`）都在它身上。于是生命周期闭包只拿一个 `&mut App` 就能把事
//! 办完——不必再多传一个上下文参数，也就不会出现「状态与上下文各借一半」的别扭签名。
//!
//! 平台侧不认识 [`App`]，它只认 [`Application`](super::Application)。

use super::exec::{AsyncRuntime, Mailbox};
use super::jobs::JobPool;
use super::pump::{EventSource, PumpContext, Wakeup};
use super::window::{Window, WindowRegistry};
use crate::capability::webview::{OverlayId, WebViewHandle};
use crate::core::window::{WindowHandle, WindowId, WindowLabel};
#[cfg(feature = "render")]
use crate::render::Renderer;
use crate::ui::TextShaper;
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
/// 窗口只以 [`Window`]（内部持 [`WindowHandle`]）的形式露出——契约层与运行时都不认识 winit
/// 的 `Window`。
pub struct AppContext {
  windows: WindowRegistry,
  sources: Vec<Box<dyn EventSource>>,
  /// 跨线程唤醒句柄。**要等事件循环建好才有**，故由平台在建循环之后注入
  /// （见 [`Application::attach_wakeup`](super::Application::attach_wakeup)）。
  wakeup: Option<Arc<dyn Wakeup>>,
  exit_requested: bool,
}

impl AppContext {
  pub(crate) fn new() -> Self {
    Self {
      windows: WindowRegistry::new(),
      sources: Vec::new(),
      wakeup: None,
      exit_requested: false,
    }
  }

  /// 注入唤醒句柄；平台在事件循环建好后调用一次。
  pub(crate) fn set_wakeup(&mut self, wakeup: Arc<dyn Wakeup>) {
    self.wakeup = Some(wakeup);
  }

  /// 主窗口。
  pub fn main_window(&self) -> Option<&Window> {
    self.windows.main()
  }

  /// 按标签取窗。
  pub fn window(&self, label: &str) -> Option<&Window> {
    self.windows.by_label(label)
  }

  /// 按 id 取窗。事件里带的是 id，运行时与平台走这一条。
  pub fn window_by_id(&self, id: WindowId) -> Option<&Window> {
    self.windows.by_id(id)
  }

  /// 全部窗口，按建窗顺序。
  pub fn windows(&self) -> impl Iterator<Item = &Window> {
    self.windows.iter()
  }

  /// 窗口数量。
  pub fn window_count(&self) -> usize {
    self.windows.len()
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

  /// 登记一个窗口（id 由运行时分配）。
  pub(crate) fn insert_window(
    &mut self,
    id: WindowId,
    label: &WindowLabel,
    handle: Arc<dyn WindowHandle>,
  ) {
    self.windows.insert(id, label, handle);
  }

  /// 摘掉一个窗口。
  pub(crate) fn remove_window(&mut self, id: WindowId) -> Option<Window> {
    self.windows.remove(id)
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
    tracing::trace!(sources = self.sources.len(), woke, "泵事件源");
    woke
  }

  /// 取走退出请求。
  pub(crate) fn take_exit_requested(&mut self) -> bool {
    std::mem::take(&mut self.exit_requested)
  }
}

/// 能力视图：装配期按配置建好、运行期借给应用的那几样东西。
///
/// 与 [`App::with_capabilities`] 配套——状态、能力、上下文三者要**同时**用到时走它（帧里几乎
/// 总是如此：状态推进要作业池、拼 HUD 要排版器、出画要渲染器）。
pub struct Capabilities<'a> {
  /// 作业池：`Builder::workers` 配过才有。
  pub jobs: Option<&'a JobPool>,
  /// 文本排版器：`Builder::font_path` / `Builder::font_bytes` 配过、且字体读得进来才有。
  pub text: Option<&'a mut TextShaper>,
  /// **主窗口**的渲染器：`Builder::renderer` 配过才有。
  #[cfg(feature = "render")]
  pub gpu: Option<&'a mut Renderer>,
}

/// 应用：托管状态 + 上下文 + 能力。
///
/// 生命周期闭包拿到的就是它。状态一律经 [`App::manage`] 登记、按类型取回——应用自己的类型由
/// 应用定义，框架不规定它的形状。
pub struct App {
  cx: AppContext,
  /// 托管状态：按类型存一份。生命周期闭包跨帧访问同一份状态就走这里。
  managed: BTreeMap<TypeId, Box<dyn Any + Send + Sync>>,
  /// 作业池：装配期按 `Builder::workers` 建，运行期只读借出。
  jobs: Option<JobPool>,
  /// 文本排版器：装配期按字体配置建，运行期可变借出（排版会往字形缓存里塞东西）。
  text: Option<TextShaper>,
  /// 异步运行时：装配期按 `Builder::async_runtime` 起，运行期只读借出。
  exec: Option<AsyncRuntime>,
  /// 渲染器：**按窗一份**，窗口建好时惰建（见 `Builder::renderer`）。
  #[cfg(feature = "render")]
  gpu: BTreeMap<WindowId, Renderer>,
  /// 覆盖层：**按「窗口 + 覆盖层 id」一份**，窗口建好时惰建（见 `Builder::webview`）。
  ///
  /// 与渲染器不同，这里没有 feature 门控：`capability::webview` 的接口层常编译，没有后端时压根
  /// 建不出东西来，这张表自然是空的。键里带窗口是因为同一个 `OverlayId` 可以挂在不同窗口上。
  webviews: BTreeMap<(WindowId, OverlayId), Box<dyn WebViewHandle>>,
}

impl App {
  pub(crate) fn new() -> Self {
    Self {
      cx: AppContext::new(),
      managed: BTreeMap::new(),
      jobs: None,
      text: None,
      exec: None,
      #[cfg(feature = "render")]
      gpu: BTreeMap::new(),
      webviews: BTreeMap::new(),
    }
  }

  pub(crate) fn context_mut(&mut self) -> &mut AppContext {
    &mut self.cx
  }

  pub(crate) fn attach_wakeup(&mut self, wakeup: Arc<dyn Wakeup>) {
    self.cx.set_wakeup(wakeup);
  }

  /// 主窗口。
  pub fn main_window(&self) -> Option<&Window> {
    self.cx.main_window()
  }

  /// 按标签取窗。
  pub fn window(&self, label: &str) -> Option<&Window> {
    self.cx.window(label)
  }

  /// 全部窗口，按建窗顺序。
  pub fn windows(&self) -> impl Iterator<Item = &Window> {
    self.cx.windows()
  }

  /// 窗口数量。
  pub fn window_count(&self) -> usize {
    self.cx.window_count()
  }

  /// 某个窗口上的覆盖层（按窗口标签 + 覆盖层 id）。
  ///
  /// 拿到的句柄可以改矩形、导航、跑脚本——`set_visible` 能临时把它藏起来（藏起来时下层照画）。
  /// 覆盖层是**原生子窗口**，不参与 z 序与裁剪：它永远浮在画面与自绘 UI 之上。
  ///
  /// 句柄的生命周期写死 `'static` 是因为 `Box` 自己持有它（不借别处）；而这个 `+ 'static` 必须
  /// 写出来——`&mut` 在对象生命周期上是不变的，省掉它就得不出这个引用。
  pub fn webview(
    &mut self,
    window_label: &str,
    id: OverlayId,
  ) -> Option<&mut (dyn WebViewHandle + 'static)> {
    let window = self.cx.window(window_label)?.id();
    self.webview_mut(window, id)
  }

  /// 某个窗口上的全部覆盖层，按覆盖层 id 升序。
  pub fn webviews(
    &self,
    window_label: &str,
  ) -> impl Iterator<Item = (OverlayId, &dyn WebViewHandle)> {
    let window = self.cx.window(window_label).map(|window| window.id());
    self
      .webviews
      .iter()
      .filter_map(move |(&(id, overlay), view)| (window == Some(id)).then_some((overlay, &**view)))
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

  /// 异步运行时：`Builder::async_runtime` 配过才有。
  ///
  /// 提交任务的场合多得很（帧里也能提），所以它是独立取值而不是能力视图的一项；`None` 时
  /// 应用自己降级（同步做掉，或干脆不做）。
  pub fn exec(&self) -> Option<&AsyncRuntime> {
    self.exec.as_ref()
  }

  /// 帧边界邮箱：按类型**懒建**并托管（见 [`Mailbox`]）。
  ///
  /// 帧里的典型用法是先排空、再带着结果去改世界状态：
  ///
  /// ```ignore
  /// let drained: Vec<Msg> = app.mailbox::<Msg>().drain().collect();
  /// app.with_state::<World, _>(|world, _| world.apply(drained));
  /// ```
  pub fn mailbox<T: Send + 'static>(&mut self) -> &mut Mailbox<T> {
    self
      .managed
      .entry(TypeId::of::<Mailbox<T>>())
      .or_insert_with(|| Box::new(Mailbox::<T>::new()))
      .downcast_mut::<Mailbox<T>>()
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

  /// **同时**取托管状态、能力与上下文。
  ///
  /// 状态仍是「临时取出」那一套（见 [`App::with_state`]）；能力是 `App` 的另外几个字段，与状态、
  /// 上下文各不相干，所以能与它们一起借出。
  ///
  /// 借出的渲染器是**主窗口**那一份（多窗口各自的渲染器见 `App::gpu`）。
  pub fn with_capabilities<T: Any + Send + Sync, R>(
    &mut self,
    f: impl FnOnce(&mut T, Capabilities<'_>, &mut AppContext) -> R,
  ) -> Option<R> {
    let mut state = self.managed.remove(&TypeId::of::<T>())?;
    // 渲染器按主窗口取：上面那句文档说的就是这个 `main`。
    #[cfg(feature = "render")]
    let main = self.cx.main_window().map(|window| window.id());
    let capabilities = Capabilities {
      jobs: self.jobs.as_ref(),
      text: self.text.as_mut(),
      #[cfg(feature = "render")]
      gpu: main.and_then(|id| self.gpu.get_mut(&id)),
    };
    let result = f(
      state.downcast_mut::<T>().expect("托管状态的键与值类型一致"),
      capabilities,
      &mut self.cx,
    );
    self.managed.insert(TypeId::of::<T>(), state);
    Some(result)
  }

  /// 某个窗口的平台句柄（运行时内部用：建渲染器、建覆盖层）。
  pub(crate) fn window_handle(&self, id: WindowId) -> Option<Arc<dyn WindowHandle>> {
    self
      .cx
      .window_by_id(id)
      .map(|window| Arc::clone(window.handle()))
  }

  /// 按窗口 id 取覆盖层（运行时内部用：resize / DPI 变化后重算摆位）。生命周期口径同
  /// [`App::webview`]。
  pub(crate) fn webview_mut(
    &mut self,
    window: WindowId,
    id: OverlayId,
  ) -> Option<&mut (dyn WebViewHandle + 'static)> {
    self.webviews.get_mut(&(window, id)).map(|view| &mut **view)
  }

  /// 某个窗口的渲染器。
  #[cfg(feature = "render")]
  pub(crate) fn gpu_mut(&mut self, id: WindowId) -> Option<&mut Renderer> {
    self.gpu.get_mut(&id)
  }

  /// 装作业池（装配期调一次）。
  pub(crate) fn set_jobs(&mut self, pool: JobPool) {
    self.jobs = Some(pool);
  }

  /// 装排版器（装配期调一次）。
  pub(crate) fn set_text(&mut self, shaper: TextShaper) {
    self.text = Some(shaper);
  }

  /// 装异步运行时（装配期调一次）。
  pub(crate) fn set_exec(&mut self, runtime: AsyncRuntime) {
    self.exec = Some(runtime);
  }

  /// 收异步运行时：`Drop` 会关停宿主线程并 join（见 [`AsyncRuntime`]）。
  pub(crate) fn clear_exec(&mut self) {
    self.exec = None;
  }

  /// 某个窗口的渲染器建好了。同一窗口再建即覆盖（旧的先释放，表面不会挂着两个）。
  #[cfg(feature = "render")]
  pub(crate) fn insert_gpu(&mut self, id: WindowId, renderer: Renderer) {
    self.gpu.insert(id, renderer);
  }

  /// 收回某个窗口的渲染器：窗口没了，表面必须跟着放掉。
  #[cfg(feature = "render")]
  pub(crate) fn remove_gpu(&mut self, id: WindowId) {
    self.gpu.remove(&id);
  }

  /// 收回全部渲染器。生命周期末尾调（表面挂窗口句柄，要在窗口之前放）。
  #[cfg(feature = "render")]
  pub(crate) fn clear_gpu(&mut self) {
    self.gpu.clear();
  }

  /// 某个窗口的覆盖层建好了。同一「窗口 + id」再建即覆盖（旧的先释放，不会同时挂着两个原生子窗口）。
  pub(crate) fn insert_webview(
    &mut self,
    window: WindowId,
    id: OverlayId,
    view: Box<dyn WebViewHandle>,
  ) {
    self.webviews.insert((window, id), view);
  }

  /// 收回某个窗口的全部覆盖层：窗口没了，原生子窗口必须跟着放掉。
  pub(crate) fn remove_webviews(&mut self, window: WindowId) {
    self.webviews.retain(|(id, _), _| *id != window);
  }

  /// 收回全部覆盖层。生命周期末尾调（原生子窗口挂在父窗口上，要在窗口之前放）。
  pub(crate) fn clear_webviews(&mut self) {
    self.webviews.clear();
  }
}

#[cfg(test)]
mod tests {
  use super::*;
  use crate::capability::webview::Mode;
  use crate::core::geometry::{LogicalRect, PhysicalSize};
  use crate::core::window::WindowHandle;
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

  /// 能力与状态、上下文能**一起**借出——帧里这三样总是同时要用。
  #[test]
  fn capabilities_come_out_alongside_state_and_context() {
    #[derive(Default)]
    struct Demo {
      frames: u32,
    }

    let mut app = App::new();
    app.manage(Demo::default());
    app.set_jobs(JobPool::with_workers(
      1,
      Arc::new(NoopWakeup) as Arc<dyn Wakeup>,
    ));

    let seen = app.with_capabilities::<Demo, _>(|demo, capabilities, cx| {
      demo.frames += 1;
      assert!(capabilities.jobs.is_some(), "装了作业池就该借得到");
      assert!(capabilities.text.is_none(), "没装排版器就是 None");
      cx.exit();
      demo.frames
    });

    assert_eq!(seen, Some(1));
    assert_eq!(app.get::<Demo>().map(|demo| demo.frames), Some(1));
    assert!(app.exit_requested(), "闭包里的退出请求落在同一个上下文上");
    assert_eq!(
      app.with_capabilities::<u32, _>(|_, _, _| ()),
      None,
      "没登记过就是 None"
    );
  }

  /// 只要一个「能登记进注册表」的句柄：覆盖层表按 id 记账，句柄内容无关。
  struct FakeWindow(WindowId);

  impl raw_window_handle::HasWindowHandle for FakeWindow {
    fn window_handle(
      &self,
    ) -> Result<raw_window_handle::WindowHandle<'_>, raw_window_handle::HandleError> {
      Err(raw_window_handle::HandleError::Unavailable)
    }
  }

  impl raw_window_handle::HasDisplayHandle for FakeWindow {
    fn display_handle(
      &self,
    ) -> Result<raw_window_handle::DisplayHandle<'_>, raw_window_handle::HandleError> {
      Err(raw_window_handle::HandleError::Unavailable)
    }
  }

  impl WindowHandle for FakeWindow {
    fn id(&self) -> WindowId {
      self.0
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

  /// 假覆盖层：表只按 id 记账，句柄内容无关，所以它什么都不用记。
  struct FakeView;

  impl WebViewHandle for FakeView {
    fn mode(&self) -> Mode {
      Mode::Overlay
    }

    fn bounds(&self) -> LogicalRect {
      LogicalRect::new(0.0, 0.0, 0.0, 0.0)
    }

    fn set_bounds(&mut self, _rect: LogicalRect) {}

    fn set_visible(&mut self, _visible: bool) {}

    fn navigate(&mut self, _url: &str) {}

    fn evaluate_script(&mut self, _js: &str) {}

    fn close(&mut self) {}
  }

  fn app_with_two_windows() -> App {
    let mut app = App::new();
    for (id, label) in [(0, WindowLabel::MAIN), (1, "inspector")] {
      let id = WindowId(id);
      app
        .context_mut()
        .insert_window(id, &WindowLabel::new(label), Arc::new(FakeWindow(id)));
    }
    app
  }

  /// 覆盖层按「窗口 + id」记账：标签查得到、别的窗口上是**另一份**、摘窗只摘自己那份。
  #[test]
  fn overlays_are_booked_per_window_and_id() {
    let mut app = app_with_two_windows();
    app.insert_webview(WindowId(0), OverlayId(1), Box::new(FakeView));
    app.insert_webview(WindowId(1), OverlayId(1), Box::new(FakeView));

    assert!(app.webview(WindowLabel::MAIN, OverlayId(1)).is_some());
    assert!(
      app.webview("inspector", OverlayId(1)).is_some(),
      "同一个 id 在别的窗口上是另一份"
    );
    assert!(app.webview(WindowLabel::MAIN, OverlayId(2)).is_none());
    assert!(
      app.webview("nowhere", OverlayId(1)).is_none(),
      "窗口不在就查不到"
    );
    assert_eq!(app.webviews(WindowLabel::MAIN).count(), 1);

    app.remove_webviews(WindowId(0));
    assert!(app.webview(WindowLabel::MAIN, OverlayId(1)).is_none());
    assert!(
      app.webview("inspector", OverlayId(1)).is_some(),
      "别的窗口不受影响"
    );

    app.clear_webviews();
    assert_eq!(app.webviews("inspector").count(), 0);
  }
}
