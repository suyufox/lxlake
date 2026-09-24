//! winit 驱动的桌面后端（windows / linux / macos）。
//!
//! 职责边界：本文件是**唯一**碰 winit 的地方（`core` 与 `runtime` 都不认识 winit）。
//! 它做三件事——建窗、把原生事件翻译成 [`Event`]、驱动 [`Application`] 的帧钩子。

use crate::core::Error;
use crate::core::event::Event;
use crate::core::geometry::{PhysicalPosition, PhysicalSize};
use crate::core::input::{Key, MouseButton};
use crate::core::window::{WindowHandle, WindowId};
use crate::runtime::{Application, FrameClock, Wakeup};
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
struct Driver<A: Application> {
  app: A,
  clock: FrameClock,
  /// 窗口翻译表：平台 id → 契约 id。窗口本体由 [`AppContext`](crate::runtime::AppContext) 持有。
  window_ids: Vec<(WinitWindowId, WindowId)>,
  next_window_id: u64,
  /// 已进入 `resumed`：桌面端要等第一次 resume 才允许建窗。
  started: bool,
  /// 「立刻补一帧」：resize / DPI 变化 / 事件源报了待处理时置上。
  frame_pending: bool,
}

impl<A: Application> Driver<A> {
  fn new(mut app: A, proxy: EventLoopProxy<UserEvent>) -> Self {
    // 唤醒句柄要等事件循环建好才有，所以在这里注入（应用对象早于事件循环就已构造）。
    app.attach_wakeup(Arc::new(ProxyWakeup(proxy)));
    let clock = FrameClock::new(app.frame_interval(), Instant::now());
    Self {
      app,
      clock,
      window_ids: Vec::new(),
      next_window_id: 0,
      started: false,
      frame_pending: false,
    }
  }

  fn create_windows(&mut self, event_loop: &ActiveEventLoop) {
    for spec in self.app.windows() {
      let attributes = Window::default_attributes()
        .with_title(&spec.desc.title)
        .with_inner_size(WinitLogicalSize::new(
          spec.desc.size.width,
          spec.desc.size.height,
        ))
        .with_resizable(spec.desc.resizable)
        .with_visible(spec.desc.visible);

      let window = match event_loop.create_window(attributes) {
        Ok(window) => window,
        Err(err) => {
          eprintln!("lxlake: 建窗失败（{}）：{err}", spec.label);
          continue;
        }
      };

      // id 在本层发号——它和上面那张翻译表是同一份「窗口出现次序」的两个视图，分开发号就会错位。
      let id = WindowId(self.next_window_id);
      self.next_window_id += 1;
      self.window_ids.push((window.id(), id));
      self
        .app
        .context_mut()
        .insert_window(id, &spec.label, Arc::new(DesktopWindow { id, window }));
      tracing::debug!(window = id.0, label = %spec.label, "建窗");
      self.app.on_window_ready(id);
    }
  }

  /// 摘掉一个窗口：翻译表、注册表、应用回调三处同步，少一处就留下鬼影。
  fn destroy_window(&mut self, id: WindowId) {
    self.window_ids.retain(|(_, contract)| *contract != id);
    self.app.context_mut().remove_window(id);
    tracing::debug!(window = id.0, "摘窗");
    self.app.on_window_destroyed(id);
  }

  /// 出一帧：推进时钟并交给应用。
  fn tick_frame(&mut self, now: Instant) {
    let frame = self.clock.advance(now);
    tracing::trace!(
      index = frame.index,
      delta_ms = frame.delta.as_secs_f64() * 1000.0,
      "帧"
    );
    self.app.on_frame(frame);
  }

  fn pump_sources(&mut self, now: Instant) {
    if self.app.context_mut().pump_sources(now) {
      self.frame_pending = true;
    }
  }

  fn emit(&mut self, event: Event) {
    self.app.on_event(&event);
  }

  /// 决定下一次醒来的时机：谁先到期听谁的。
  fn schedule(&mut self, event_loop: &ActiveEventLoop) {
    let flow = match self.clock.next_due() {
      // 不限速：交给 present mode / 系统节流。
      None => ControlFlow::Poll,
      Some(frame_due) => match self.app.context_mut().next_source_deadline() {
        Some(source_due) => ControlFlow::WaitUntil(frame_due.min(source_due)),
        None => ControlFlow::WaitUntil(frame_due),
      },
    };
    event_loop.set_control_flow(flow);
  }

  /// 释放窗口。必须发生在事件循环销毁之前。
  fn close_windows(&mut self) {
    self.window_ids.clear();
    self.app.context_mut().clear_windows();
  }
}

impl<A: Application> ApplicationHandler<UserEvent> for Driver<A> {
  fn resumed(&mut self, event_loop: &ActiveEventLoop) {
    if self.started {
      return;
    }
    self.started = true;
    // 原始鼠标位移是设备级事件，得显式声明要。只在窗口有焦点时要——后台不必收。
    event_loop.listen_device_events(DeviceEvents::WhenFocused);
    self.create_windows(event_loop);
    self.app.on_startup();
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
          .app
          .context_mut()
          .window_by_id(id)
          .map_or_else(PhysicalSize::default, |w| w.handle().size());
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
          .app
          .context_mut()
          .window_by_id(id)
          .map_or(1.0, |window| window.handle().scale_factor());
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
        self.destroy_window(id);
        self.emit(Event::CloseRequested { window: id });
        // 多窗口下不能「关一个就走」：注册表空了才轮到事件循环退。
        // 想提前退（比如关掉主窗口就结束）由应用在事件钩子里调 `App::exit()`。
        if self.app.context_mut().window_count() == 0 {
          event_loop.exit();
        }
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
    if self.app.context_mut().source_due(now) {
      self.pump_sources(now);
    }

    if self.clock.is_due(now) || self.frame_pending {
      self.frame_pending = false;
      self.tick_frame(now);
    }

    if self.app.context_mut().take_exit_requested() {
      event_loop.exit();
      return;
    }

    self.schedule(event_loop);
  }

  fn exiting(&mut self, _event_loop: &ActiveEventLoop) {
    self.app.on_shutdown();
    self.close_windows();
  }
}

/// 跑事件循环，阻塞至应用退出。
pub(crate) fn run<A: Application>(app: A) -> Result<(), Error> {
  // 路径先装：应用目录由应用的标识定，之后所有目录判断都走 `crate::path` 这一处
  //（日志的默认落点也靠它）。装在这里是因为入口才知道自己是哪个平台、根在哪。
  crate::path::install(crate::path::Paths::for_app(app.app_id()));
  // 日志紧跟路径、且在建事件循环之前：默认落点在应用目录下，此刻才解析得出来；
  // 而窗口与渲染后端初始化阶段的日志也该被捕获（见 runtime::log 的数据与安装分离）。
  if let Some(config) = app.log_config()
    && let Err(error) = config.install()
  {
    eprintln!("lxlake: {error}");
  }

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
