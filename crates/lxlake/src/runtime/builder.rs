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
use super::exec::{AsyncConfig, AsyncRuntime};
use super::log::LogConfig;
use super::{Application, DEFAULT_FPS, Frame, JobPool, Wakeup};
use crate::core::Error;
use crate::core::event::Event;
#[cfg(feature = "render")]
use crate::core::window::WindowHandle;
use crate::core::window::{WindowDesc, WindowId, WindowLabel, WindowSpec};
#[cfg(feature = "render")]
use crate::render::{RenderError, Renderer};
use crate::ui::TextShaper;
use std::any::Any;
use std::sync::Arc;
use std::time::Duration;

/// 生命周期闭包的存储形状（给下面的字段用，省得每一处都写一遍 boxed dyn）。
type StartupHook = Box<dyn FnOnce(&mut App)>;
type EventHook = Box<dyn FnMut(&mut App, &Event)>;
type FrameHook = Box<dyn FnMut(&mut App, Frame)>;
type ShutdownHook = Box<dyn FnOnce(&mut App)>;

/// 渲染器工厂的存储形状：按窗口句柄建一份渲染器。
#[cfg(feature = "render")]
type RendererFactory = Box<dyn Fn(Arc<dyn WindowHandle>) -> Result<Renderer, RenderError>>;

/// 没配 `app_id` 时的应用标识。
const DEFAULT_APP_ID: &str = "lxlake";

/// 装配期插件：拿到当前 [`Builder`]，返回改写后的那个。
///
/// 这里**只留接位**——真正的插件宿主（C ABI / wasmtime）落在 `plugin` 模块，留到后面那一步。
pub trait Plugin: 'static {
  fn configure(self: Box<Self>, builder: Builder) -> Builder;
}

/// 字体来源：应用给路径，或直接给已经读进内存的字节。
enum FontSource {
  Path(String),
  Bytes(Vec<u8>),
}

/// 应用装配器。
pub struct Builder {
  /// 应用本体：托管状态与上下文在装配期就已经是运行期那一份。
  app: App,
  /// 应用标识。应用目录与日志默认落点由它定（见 `crate::path`）。
  app_id: String,
  /// 待建窗口。标签为主窗口的那个排在最前（`main_window` 换的就是它）。
  windows: Vec<WindowSpec>,
  /// 目标帧间隔；`None` = 不限速。
  frame_interval: Option<Duration>,
  /// 作业线程数；`None` = 不建作业池。
  workers: Option<usize>,
  /// 字体来源；`None` = 不建排版器。
  font: Option<FontSource>,
  /// 日志配置；`None` = 不装全局订阅器。
  log: Option<LogConfig>,
  /// 异步运行时配置；`None` = 不起宿主线程（`App::exec()` 为 `None`）。
  async_config: Option<AsyncConfig>,
  /// 待应用的插件：`run` 起点按加入顺序依次改写装配。
  plugins: Vec<Box<dyn Plugin>>,
  /// 渲染器工厂：窗口建好时按它的句柄建一份。`None` = 不建渲染器。
  ///
  /// 是 `Fn` 而不是 `FnOnce`——多窗口要各建一份。
  #[cfg(feature = "render")]
  renderer: Option<RendererFactory>,
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
  /// 默认：没有窗口 + 60 帧的目标帧率。
  ///
  /// 窗口是显式声明的（[`Builder::main_window`] / [`Builder::create_window`]）——不隐式建窗，
  /// 于是「无窗口应用」天然可表达（不声明窗口即可）。
  pub fn new() -> Self {
    Self {
      app: App::new(),
      app_id: DEFAULT_APP_ID.to_owned(),
      windows: Vec::new(),
      frame_interval: Some(Duration::from_nanos(1_000_000_000 / u64::from(DEFAULT_FPS))),
      workers: None,
      font: None,
      log: None,
      async_config: None,
      plugins: Vec::new(),
      #[cfg(feature = "render")]
      renderer: None,
      on_startup: None,
      on_event: None,
      on_frame: None,
      on_shutdown: None,
    }
  }

  /// 应用标识：应用目录与日志默认落点由它定（见 `crate::path::Paths::for_app`）。
  ///
  /// 不配就是 `lxlake`；应用该给一个自己专属的（如 `com.lxlake.demo`）。
  pub fn app_id(mut self, id: impl Into<String>) -> Self {
    self.app_id = id.into();
    self
  }

  /// 注入**主窗口**（标签固定为 `main`）。重复调用则后一次覆盖前一次。
  pub fn main_window(mut self, desc: WindowDesc) -> Self {
    let spec = WindowSpec::main(desc);
    match self.windows.first_mut() {
      Some(first) if first.label.is_main() => *first = spec,
      _ => self.windows.insert(0, spec),
    }
    self
  }

  /// 再建一个窗口，用 `label` 标识它（`main` 是保留标签，用了会在 [`Builder::run`] 报错）。
  pub fn create_window(mut self, label: impl Into<WindowLabel>, desc: WindowDesc) -> Self {
    self.windows.push(WindowSpec {
      label: label.into(),
      desc,
    });
    self
  }

  /// 目标帧间隔；`None` = 不限速（交给 present mode / 系统节流）。
  pub fn frame_interval(mut self, interval: Option<Duration>) -> Self {
    self.frame_interval = interval;
    self
  }

  /// 作业线程数。不配就不建作业池（能力里的 `jobs` 为 `None`）。
  pub fn workers(mut self, threads: usize) -> Self {
    self.workers = Some(threads);
    self
  }

  /// UI 字体从文件读。读不到或解析不了只报一声，应用照常起（能力里的 `text` 为 `None`）。
  pub fn font_path(mut self, path: impl Into<String>) -> Self {
    self.font = Some(FontSource::Path(path.into()));
    self
  }

  /// 字体直接用字节（已经由应用读进内存的那份）。
  pub fn font_bytes(mut self, bytes: Vec<u8>) -> Self {
    self.font = Some(FontSource::Bytes(bytes));
    self
  }

  /// 日志配置：平台入口在装完路径之后安装（见 [`LogConfig`]）。不配就不装订阅器。
  pub fn log(mut self, config: LogConfig) -> Self {
    self.log = Some(config);
    self
  }

  /// 日志配置由闭包给——配置本身要读应用自己的东西（配置文件、命令行）时走它。
  pub fn log_with(mut self, configure: impl FnOnce() -> LogConfig) -> Self {
    self.log = Some(configure());
    self
  }

  /// 异步运行时。不配就不起宿主线程，`App::exec()` 为 `None`。
  pub fn async_runtime(mut self, config: AsyncConfig) -> Self {
    self.async_config = Some(config);
    self
  }

  /// 插件：`run` 起点按加入顺序依次改写装配。
  pub fn plugin(mut self, plugin: impl Plugin + 'static) -> Self {
    self.plugins.push(Box::new(plugin));
    self
  }

  /// 渲染器工厂：**每建一个窗口**按它的句柄建一份（闭包是 `Fn`，多窗各建一份）。
  ///
  /// 建失败只报一声，该窗口不出画，应用照常跑。
  #[cfg(feature = "render")]
  pub fn renderer(
    mut self,
    make: impl Fn(Arc<dyn WindowHandle>) -> Result<Renderer, RenderError> + 'static,
  ) -> Self {
    self.renderer = Some(Box::new(make));
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
  ///
  /// 桌面专有：android 的事件循环要靠系统递进来的 activity 才建得起来，那边走
  /// [`Builder::run_android`]。
  #[cfg(any(target_os = "windows", target_os = "linux", target_os = "macos"))]
  pub fn run(self) -> Result<(), Error> {
    super::run(self.apply_plugins())
  }

  /// android 版 [`Builder::run`]：多一件系统递进来的 activity。
  ///
  /// 插件照桌面那条路在**进入运行时之前**应用——两条入口走同一份装配，只是事件循环的来路不同。
  /// 由 `#[lxlake::entry]` 生成的 `android_main` 调用。
  #[cfg(target_os = "android")]
  pub fn run_android(self, android_app: crate::platform::AndroidApp) -> Result<(), Error> {
    super::run_android(android_app, self.apply_plugins())
  }

  /// 应用插件：按加入顺序依次改写装配。
  ///
  /// 放在进入运行时**之前**——此刻窗口、能力、钩子都还只是数据，改得动。
  fn apply_plugins(mut self) -> Self {
    let plugins = std::mem::take(&mut self.plugins);
    for plugin in plugins {
      self = plugin.configure(self);
    }
    self
  }

  /// 装配期把能力建起来。作业池要唤醒句柄（此刻已由平台注入），字体在这里读一次；
  /// 异步运行时也在这里起（同样要等唤醒句柄就位，且起不来不该挡住启动）。
  fn install_capabilities(&mut self) {
    if let Some(workers) = self.workers {
      self
        .app
        .set_jobs(JobPool::with_workers(workers, self.app.wakeup()));
    }
    if let Some(source) = self.font.take() {
      match load_font(source) {
        Ok(shaper) => self.app.set_text(shaper),
        Err(reason) => eprintln!("lxlake: {reason}"),
      }
    }
    if let Some(config) = self.async_config.take() {
      match AsyncRuntime::new(config) {
        Ok(runtime) => self.app.set_exec(runtime),
        Err(error) => eprintln!("lxlake: {error}"),
      }
    }
  }

  /// 收干净能力：渲染器要在**窗口之前**放——表面挂着窗口句柄，等窗口没了再放就要处理
  /// 「表面活过了窗口」。异步运行时也在这里收：宿主线程关停并 join，别让它活到进程退出之后。
  fn release_capabilities(&mut self) {
    #[cfg(feature = "render")]
    self.app.clear_gpu();
    self.app.clear_exec();
  }

  /// 引擎内建的窗口事件转发：resize / DPI 变化先落到**该窗**的渲染器上，再进应用闭包。
  ///
  /// 这段是每个应用都要写一遍的纯样板（表面重配 + 深度附件重建 + 换算比例），所以收进引擎。
  fn forward_window_event(&mut self, event: &Event) {
    #[cfg(feature = "render")]
    match event {
      Event::Resized { window, size } => {
        if let Some(renderer) = self.app.gpu_mut(*window) {
          renderer.resize(*size);
        }
      }
      Event::ScaleFactorChanged {
        window,
        scale_factor,
        size,
      } => {
        if let Some(renderer) = self.app.gpu_mut(*window) {
          renderer.set_scale_factor(*scale_factor);
          renderer.resize(*size);
        }
      }
      _ => {}
    }
    #[cfg(not(feature = "render"))]
    let _ = event;
  }

  /// 窗口刚建好：按窗惰建渲染器（没配 `renderer` 就什么都不做）。
  fn build_renderer(&mut self, id: WindowId) {
    #[cfg(feature = "render")]
    {
      let Some(make) = self.renderer.as_ref() else {
        return;
      };
      let Some(handle) = self.app.window_handle(id) else {
        return;
      };
      match make(handle) {
        Ok(renderer) => self.app.insert_gpu(id, renderer),
        Err(error) => eprintln!("lxlake: 建渲染器失败（窗口 {id:?}）：{error}"),
      }
    }
    #[cfg(not(feature = "render"))]
    let _ = id;
  }
}

/// 按来源读一份字体并建成排版器。
fn load_font(source: FontSource) -> Result<TextShaper, String> {
  let (label, bytes) = match source {
    FontSource::Path(path) => {
      let bytes = std::fs::read(&path).map_err(|error| format!("读不到字体 {path}：{error}"))?;
      (path, bytes)
    }
    FontSource::Bytes(bytes) => ("<内存>".to_owned(), bytes),
  };
  TextShaper::from_bytes(bytes).map_err(|error| format!("字体 {label} 解析失败：{error}"))
}

impl Application for Builder {
  fn attach_wakeup(&mut self, wakeup: Arc<dyn Wakeup>) {
    self.app.attach_wakeup(wakeup);
  }

  fn context_mut(&mut self) -> &mut AppContext {
    self.app.context_mut()
  }

  fn app_id(&self) -> &str {
    &self.app_id
  }

  fn log_config(&self) -> Option<&LogConfig> {
    self.log.as_ref()
  }

  fn windows(&self) -> Vec<WindowSpec> {
    self.windows.clone()
  }

  fn frame_interval(&self) -> Option<Duration> {
    self.frame_interval
  }

  fn on_window_ready(&mut self, id: WindowId) {
    self.build_renderer(id);
  }

  fn on_window_destroyed(&mut self, id: WindowId) {
    #[cfg(feature = "render")]
    self.app.remove_gpu(id);
    #[cfg(not(feature = "render"))]
    let _ = id;
  }

  fn on_startup(&mut self) {
    // 能力先建：应用的 `on_startup` 里往往立刻就要用（读字体、拿作业池提交首批作业）。
    self.install_capabilities();
    if let Some(f) = self.on_startup.take() {
      f(&mut self.app);
    }
  }

  fn on_event(&mut self, event: &Event) {
    // 内建转发在用户闭包**之前**：应用看到事件时，该窗的表面已经是新的了。
    self.forward_window_event(event);
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
    self.release_capabilities();
  }
}

#[cfg(test)]
mod tests {
  use super::*;
  use crate::core::window::WindowHandle;
  use std::sync::atomic::{AtomicUsize, Ordering};

  /// 装配期测试只用到唤醒句柄的**存在**，不关心它做什么。
  struct NullWakeup;

  impl Wakeup for NullWakeup {
    fn wake(&self) {}
  }

  /// 记账用：托管状态，只为让 `with_capabilities` 有个类型可取。
  struct Demo;

  /// 数自己被应用过几次的插件。窗口按**应用次序**起名——这样断言窗口的顺序就等于断言插件的顺序。
  struct CountingPlugin(Arc<AtomicUsize>);

  impl Plugin for CountingPlugin {
    fn configure(self: Box<Self>, builder: Builder) -> Builder {
      let nth = self.0.fetch_add(1, Ordering::SeqCst);
      builder.create_window(format!("from-plugin-{nth}"), WindowDesc::default())
    }
  }

  fn install(builder: &mut Builder) {
    builder.attach_wakeup(Arc::new(NullWakeup));
    builder.install_capabilities();
  }

  /// 借出的能力里，作业池与排版器各在不在。
  ///
  /// 渲染器不在这里看：它要真设备建得出来，单测里给不出（它的存在性由窗口就绪那条路径保证）。
  fn capabilities_of(builder: &mut Builder) -> (bool, bool) {
    builder
      .app
      .with_capabilities::<Demo, _>(|_, capabilities, _| {
        (capabilities.jobs.is_some(), capabilities.text.is_some())
      })
      .expect("登记过 Demo")
  }

  #[test]
  fn plugins_rewrite_the_assembly_in_order() {
    let hits = Arc::new(AtomicUsize::new(0));
    let builder = Builder::new()
      .main_window(WindowDesc::default())
      .plugin(CountingPlugin(Arc::clone(&hits)))
      .plugin(CountingPlugin(Arc::clone(&hits)));

    let builder = builder.apply_plugins();

    assert_eq!(hits.load(Ordering::SeqCst), 2, "每个插件都应用一次");
    let labels: Vec<String> = builder
      .windows()
      .iter()
      .map(|spec| spec.label.0.clone())
      .collect();
    assert_eq!(
      labels,
      vec![
        WindowLabel::MAIN.to_owned(),
        "from-plugin-0".to_owned(),
        "from-plugin-1".to_owned()
      ]
    );
  }

  #[test]
  fn a_configured_pool_becomes_a_job_capability() {
    let mut builder = Builder::new().workers(2).manage(Demo);

    install(&mut builder);

    let (jobs, _) = capabilities_of(&mut builder);
    assert!(jobs, "配过 workers 就该有作业池");
  }

  /// 字体读不进来**不该挡住启动**：能力留空，应用照跑（HUD 整块不画）。
  #[test]
  fn an_unreadable_font_leaves_the_capability_empty() {
    let mut builder = Builder::new()
      .font_bytes(b"not a font".to_vec())
      .manage(Demo);

    install(&mut builder);

    let (_, text) = capabilities_of(&mut builder);
    assert!(!text, "字体解析失败就不给排版器");
  }

  /// 真实字体资产在场时，`font_path` 该把它读成排版器。
  #[test]
  fn a_font_path_becomes_a_text_capability() {
    const FONT_PATH: &str = concat!(
      env!("CARGO_MANIFEST_DIR"),
      "/../../data/fonts/NotoSansSC-Regular.otf"
    );
    if !std::path::Path::new(FONT_PATH).exists() {
      return; // 没有字体资产就不测：HUD 本身也画不出来。
    }

    let mut builder = Builder::new().font_path(FONT_PATH).manage(Demo);

    install(&mut builder);

    let (_, text) = capabilities_of(&mut builder);
    assert!(text, "字体读得进来就该有排版器");
  }

  /// 应用标识是平台入口算应用目录（与日志默认落点）的依据：不配有一份默认值，配了要生效。
  ///
  /// 取值走 [`Application::app_id`] 而不是 `.app_id()`——同名，但那个是**消费式设置器**，
  /// 链式装配要的是它的名字。
  #[test]
  fn the_app_id_is_what_the_assembly_says() {
    assert_eq!(Application::app_id(&Builder::new()), DEFAULT_APP_ID);

    let builder = Builder::new().app_id("com.lxlake.demo");
    assert_eq!(Application::app_id(&builder), "com.lxlake.demo");
  }

  /// `Builder` 实现 `Application` 是装配期与运行期同一份数据的凭据。
  #[test]
  fn windows_are_what_the_application_declares() {
    let builder = Builder::new()
      .main_window(WindowDesc::default())
      .create_window("inspector", WindowDesc::default());

    let specs = builder.windows();

    assert_eq!(specs.len(), 2);
    assert!(specs[0].label.is_main());
    assert_eq!(specs[1].label.as_str(), "inspector");
    let _: &dyn Application = &builder;
  }

  /// 假句柄：只为让 `window_handle` 找得到东西。
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

    fn size(&self) -> crate::core::geometry::PhysicalSize {
      crate::core::geometry::PhysicalSize::new(4, 4)
    }

    fn scale_factor(&self) -> f64 {
      1.0
    }

    fn set_title(&self, _title: &str) {}

    fn set_cursor_grab(&self, _grab: bool) {}

    fn set_cursor_visible(&self, _visible: bool) {}
  }

  /// 没配 `renderer` 时建窗是安静的：不建渲染器、不 panic（无渲染特性的应用就靠这条）。
  #[test]
  fn a_window_without_a_renderer_factory_is_fine() {
    let mut builder = Builder::new().main_window(WindowDesc::default());
    builder.app.context_mut().insert_window(
      WindowId(0),
      &WindowLabel::new(WindowLabel::MAIN),
      Arc::new(FakeWindow(WindowId(0))),
    );

    builder.on_window_ready(WindowId(0));
    builder.on_window_destroyed(WindowId(0));
    builder.forward_window_event(&Event::Resized {
      window: WindowId(0),
      size: crate::core::geometry::PhysicalSize::new(800, 600),
    });
  }
}
