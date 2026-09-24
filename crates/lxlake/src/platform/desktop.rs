//! winit 驱动的桌面后端（windows / linux / macos）。
//!
//! 职责边界：本文件是**唯一**碰 winit 的地方（`core` 与 `runtime` 都不认识 winit）。
//! 它做三件事——建窗、把原生事件翻译成 [`Event`]、驱动 `App` 的帧钩子。

use crate::core::Error;
use crate::core::event::Event;
use crate::core::geometry::{PhysicalPosition, PhysicalSize};
use crate::core::input::{Key, MouseButton};
use crate::core::window::{WindowHandle, WindowId};
use crate::runtime::{App, AppContext, FrameClock, Wakeup};
use raw_window_handle::{DisplayHandle, HandleError, HasDisplayHandle, HasWindowHandle};
use std::sync::Arc;
use std::time::Instant;
use winit::application::ApplicationHandler;
use winit::dpi::LogicalSize as WinitLogicalSize;
use winit::event::{
  DeviceEvent, DeviceId, ElementState, MouseButton as WinitMouseButton, MouseScrollDelta,
  WindowEvent as WinitWindowEvent,
};
use winit::event_loop::{ActiveEventLoop, ControlFlow, DeviceEvents, EventLoop, EventLoopProxy};
use winit::keyboard::{KeyCode, PhysicalKey};
use winit::window::{CursorGrabMode, Window, WindowId as WinitWindowId};

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

/// 原生句柄直接转发给内层 winit 窗口。
///
/// 这两个 trait 是本层与渲染侧的唯一交接面（见 `core::window`）：渲染拿它建表面，
/// 全程不必经过 winit 的类型。
impl HasWindowHandle for DesktopWindow {
  fn window_handle(&self) -> Result<raw_window_handle::WindowHandle<'_>, HandleError> {
    self.window.window_handle()
  }
}

impl HasDisplayHandle for DesktopWindow {
  fn display_handle(&self) -> Result<DisplayHandle<'_>, HandleError> {
    self.window.display_handle()
  }
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

  fn set_cursor_grab(&self, grab: bool) {
    let mode = if grab {
      CursorGrabMode::Locked
    } else {
      CursorGrabMode::None
    };
    if let Err(err) = self.window.set_cursor_grab(mode) {
      eprintln!("lxlake: 光标抓取失败：{err}");
    }
  }

  fn set_cursor_visible(&self, visible: bool) {
    self.window.set_cursor_visible(visible);
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
    // 原始鼠标位移是设备级事件，得显式声明要。只在窗口有焦点时要——后台不必收。
    event_loop.listen_device_events(DeviceEvents::WhenFocused);
    self.create_windows(event_loop);
    self.app.on_startup(&mut self.cx);
    self.frame_pending = true;
  }

  fn user_event(&mut self, _event_loop: &ActiveEventLoop, _event: UserEvent) {
    // 被别的线程叫醒（作业完成、事件源从别处拿到结果）：补一帧，再泵一遍事件源。
    // 补帧是必须的——`Wakeup` 的语义就是「立刻醒一次」，只泵源不跑帧等于白醒。
    self.frame_pending = true;
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
      WinitWindowEvent::KeyboardInput { event, .. } => {
        // 没映射的键直接丢——契约层的键集是收窄的，不是 winit 的镜像。
        if let Some(key) = translate_key(event.physical_key) {
          self.emit(Event::KeyboardInput {
            window: id,
            key,
            pressed: event.state == ElementState::Pressed,
            repeat: event.repeat,
          });
        }
      }
      // 光标位置换算成逻辑坐标再派发：UI 的布局与命中测试全在逻辑像素里，应用不必自己乘 DPI。
      WinitWindowEvent::CursorMoved { position, .. } => {
        let scale_factor = self
          .cx
          .window(id)
          .map_or(1.0, |window| window.scale_factor());
        self.emit(Event::CursorMoved {
          window: id,
          position: PhysicalPosition::new(position.x, position.y).to_logical(scale_factor),
        });
      }
      WinitWindowEvent::MouseInput { state, button, .. } => {
        if let Some(button) = translate_button(button) {
          self.emit(Event::MouseButton {
            window: id,
            button,
            pressed: state == ElementState::Pressed,
          });
        }
      }
      WinitWindowEvent::MouseWheel { delta, .. } => {
        let delta = match delta {
          MouseScrollDelta::LineDelta(x, y) => [x, y],
          MouseScrollDelta::PixelDelta(position) => [position.x as f32, position.y as f32],
        };
        self.emit(Event::MouseWheel { window: id, delta });
      }
      WinitWindowEvent::CloseRequested => {
        self.emit(Event::CloseRequested { window: id });
        event_loop.exit();
      }
      _ => {}
    }
  }

  fn device_event(
    &mut self,
    _event_loop: &ActiveEventLoop,
    _device_id: DeviceId,
    event: DeviceEvent,
  ) {
    // 只取鼠标原始位移：光标位置会撞屏幕边界、还会被系统加速改掉，视角控制不能用它。
    if let DeviceEvent::MouseMotion { delta } = event {
      self.emit(Event::MouseMotion {
        delta: [delta.0 as f32, delta.1 as f32],
      });
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

/// 原生键码 → 契约层按键。没收录的返回 `None`（运行时不会派发出去）。
fn translate_key(physical: PhysicalKey) -> Option<Key> {
  let PhysicalKey::Code(code) = physical else {
    // 按布局位置而非物理键位报上来的键，本层不认。
    return None;
  };
  Some(match code {
    KeyCode::KeyW => Key::W,
    KeyCode::KeyA => Key::A,
    KeyCode::KeyS => Key::S,
    KeyCode::KeyD => Key::D,
    KeyCode::KeyQ => Key::Q,
    KeyCode::KeyE => Key::E,
    KeyCode::Space => Key::Space,
    KeyCode::ShiftLeft | KeyCode::ShiftRight => Key::Shift,
    KeyCode::ControlLeft | KeyCode::ControlRight => Key::Control,
    KeyCode::Tab => Key::Tab,
    KeyCode::Escape => Key::Escape,
    _ => return None,
  })
}

/// 原生鼠标键 → 契约层鼠标键。侧键一类不收录。
fn translate_button(button: WinitMouseButton) -> Option<MouseButton> {
  Some(match button {
    WinitMouseButton::Left => MouseButton::Left,
    WinitMouseButton::Right => MouseButton::Right,
    WinitMouseButton::Middle => MouseButton::Middle,
    _ => return None,
  })
}
