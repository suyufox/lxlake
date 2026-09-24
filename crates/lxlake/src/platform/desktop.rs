//! winit 驱动的桌面后端（windows / linux / macos）。
//!
//! 职责边界：本文件是**唯一**碰 winit 的地方（`core` 与 `runtime` 都不认识 winit）。
//! 它做三件事——建窗、把原生事件翻译成 [`Event`]、驱动 `App` 的帧钩子。

use crate::core::Error;
use crate::core::event::Event;
use crate::core::geometry::PhysicalSize;
use crate::core::window::{WindowHandle, WindowId};
use crate::runtime::{App, AppContext, FrameClock, Wakeup};
use std::sync::Arc;
use std::time::Instant;
use winit::application::ApplicationHandler;
use winit::dpi::LogicalSize as WinitLogicalSize;
use winit::event::WindowEvent as WinitWindowEvent;
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop, EventLoopProxy};
use winit::window::{Window, WindowId as WinitWindowId};

/// 事件循环的用户事件：唯一用途是跨线程唤醒——把运行时的等待打断，让它立刻泵一遍。
enum UserEvent {
  Wake,
}

/// 把 winit 的 `EventLoopProxy` 收进契约层的 [`Wakeup`]。
struct ProxyWakeup(EventLoopProxy<UserEvent>);

impl Wakeup for ProxyWakeup {
  fn wake(&self) {
    // 事件循环已退出时发送必然失败，此刻唤醒已无意义，忽略。
    let _ = self.0.send_event(UserEvent::Wake);
  }
}

/// [`WindowHandle`] 的 winit 实现。
struct DesktopWindow {
  id: WindowId,
  window: Window,
}

impl WindowHandle for DesktopWindow {
  fn id(&self) -> WindowId {
    self.id
  }

  fn size(&self) -> PhysicalSize {
    let size = self.window.inner_size();
    PhysicalSize::new(size.width, size.height)
  }

  fn scale_factor(&self) -> f64 {
    self.window.scale_factor()
  }

  fn set_title(&self, title: &str) {
    self.window.set_title(title);
  }
}

/// 事件循环驱动器。
struct Driver<A: App> {
  app: A,
  cx: AppContext,
  clock: FrameClock,
  /// 窗口翻译表：平台 id → 契约 id。窗口本体由 [`AppContext`] 持有。
  window_ids: Vec<(WinitWindowId, WindowId)>,
  next_window_id: u64,
  /// 已进入 `resumed`：桌面端要等第一次 resume 才允许建窗。
  started: bool,
  /// 「立刻补一帧」：resize / DPI 变化 / 事件源报了待处理时置上。
  frame_pending: bool,
}

impl<A: App> Driver<A> {
  fn new(app: A, proxy: EventLoopProxy<UserEvent>) -> Self {
    let clock = FrameClock::new(app.frame_interval(), Instant::now());
    let wakeup: Arc<dyn Wakeup> = Arc::new(ProxyWakeup(proxy));
    Self {
      app,
      cx: AppContext::new(wakeup),
      clock,
      window_ids: Vec::new(),
      next_window_id: 0,
      started: false,
      frame_pending: false,
    }
  }

  fn create_windows(&mut self, event_loop: &ActiveEventLoop) {
    for desc in self.app.windows() {
      let attributes = Window::default_attributes()
        .with_title(&desc.title)
        .with_inner_size(WinitLogicalSize::new(desc.size.width, desc.size.height))
        .with_resizable(desc.resizable)
        .with_visible(desc.visible);

      let window = match event_loop.create_window(attributes) {
        Ok(window) => window,
        Err(err) => {
          eprintln!("lxlake: 建窗失败（{}）：{err}", desc.title);
          continue;
        }
      };

      let id = WindowId(self.next_window_id);
      self.next_window_id += 1;
      self.window_ids.push((window.id(), id));
      self.cx.push_window(Arc::new(DesktopWindow { id, window }));
    }
  }

  /// 出一帧：推进时钟并交给应用。
  fn tick_frame(&mut self, now: Instant) {
    let frame = self.clock.advance(now);
    self.app.on_frame(&mut self.cx, frame);
  }

  fn pump_sources(&mut self, now: Instant) {
    if self.cx.pump_sources(now) {
      self.frame_pending = true;
    }
  }

  fn emit(&mut self, event: Event) {
    self.app.on_event(&mut self.cx, &event);
  }

  /// 决定下一次醒来的时机：谁先到期听谁的。
  fn schedule(&self, event_loop: &ActiveEventLoop) {
    let flow = match self.clock.next_due() {
      // 不限速：交给 present mode / 系统节流。
      None => ControlFlow::Poll,
      Some(frame_due) => match self.cx.next_source_deadline() {
        Some(source_due) => ControlFlow::WaitUntil(frame_due.min(source_due)),
        None => ControlFlow::WaitUntil(frame_due),
      },
    };
    event_loop.set_control_flow(flow);
  }

  /// 释放窗口。必须发生在事件循环销毁之前。
  fn close_windows(&mut self) {
    self.window_ids.clear();
    self.cx.clear_windows();
  }
}

impl<A: App> ApplicationHandler<UserEvent> for Driver<A> {
  fn resumed(&mut self, event_loop: &ActiveEventLoop) {
    if self.started {
      return;
    }
    self.started = true;
    self.create_windows(event_loop);
    self.app.on_startup(&mut self.cx);
    self.frame_pending = true;
  }

  fn user_event(&mut self, _event_loop: &ActiveEventLoop, _event: UserEvent) {
    // 唤醒只做一件事：立刻泵一遍事件源——它们可能刚从别的线程拿到结果。
    self.pump_sources(Instant::now());
  }

  fn window_event(
    &mut self,
    event_loop: &ActiveEventLoop,
    raw_id: WinitWindowId,
    event: WinitWindowEvent,
  ) {
    let Some((_, id)) = self
      .window_ids
      .iter()
      .find(|(raw, _)| *raw == raw_id)
      .copied()
    else {
      return;
    };

    // resize / DPI 变化本身就要一帧新画面，不等下一个帧边界。
    if matches!(
      event,
      WinitWindowEvent::Resized(_) | WinitWindowEvent::ScaleFactorChanged { .. }
    ) {
      self.frame_pending = true;
    }

    match event {
      WinitWindowEvent::Resized(size) => {
        let size = PhysicalSize::new(size.width, size.height);
        self.emit(Event::Resized { window: id, size });
      }
      WinitWindowEvent::ScaleFactorChanged { scale_factor, .. } => {
        let size = self
          .cx
          .window(id)
          .map_or_else(PhysicalSize::default, |w| w.size());
        self.emit(Event::ScaleFactorChanged {
          window: id,
          scale_factor,
          size,
        });
      }
      WinitWindowEvent::Focused(focused) => self.emit(Event::Focused {
        window: id,
        focused,
      }),
      WinitWindowEvent::CloseRequested => {
        self.emit(Event::CloseRequested { window: id });
        event_loop.exit();
      }
      _ => {}
    }
  }

  fn about_to_wait(&mut self, event_loop: &ActiveEventLoop) {
    let now = Instant::now();

    // 事件源比帧更细，先按它们的截止时间泵。
    if self.cx.source_due(now) {
      self.pump_sources(now);
    }

    if self.clock.is_due(now) || self.frame_pending {
      self.frame_pending = false;
      self.tick_frame(now);
    }

    if self.cx.take_exit_requested() {
      event_loop.exit();
      return;
    }

    self.schedule(event_loop);
  }

  fn exiting(&mut self, _event_loop: &ActiveEventLoop) {
    self.app.on_shutdown(&mut self.cx);
    self.close_windows();
  }
}

/// 跑事件循环，阻塞至应用退出。
pub(crate) fn run<A: App>(app: A) -> Result<(), Error> {
  let event_loop = EventLoop::<UserEvent>::with_user_event()
    .build()
    .map_err(|err| Error::Platform(format!("创建事件循环失败：{err}")))?;

  let proxy = event_loop.create_proxy();
  let mut driver = Driver::new(app, proxy);

  let result = event_loop.run_app(&mut driver);
  // 正常路径已在 `exiting` 里清理过；这里只兜住异常退出。
  driver.close_windows();

  result.map_err(|err| Error::Platform(format!("事件循环异常退出：{err}")))
}
