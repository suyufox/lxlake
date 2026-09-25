//! 渲染的**地板档**：设备、队列、表面与它的配置。
//!
//! 本模块是 `gpu` 特性那一档的全部内容——不含方片管线（那是 `ui-render` 的 `super::ui`），
//! 也不含 3D（那是 `render` 的 `super::pipeline` / `super::atlas`）。分出来的理由是消费者不同：
//! **CEF 纹理模式**要「建设备 → 把自己的纹理交给 wgpu → 取帧 → present」，但一点都不需要方片
//! 管线。今天还没有这个调用方，所以只开 `gpu` 的构建**只有编译能当守卫**。
//!
//! [`Renderer`](super::Renderer) 在三种档位下都存在，内部就是本模块的一个 [`GpuContext`]——
//! 于是 `runtime` 侧「窗口就绪时建一份、resize 时喂给它、销毁时收掉」这三点不必按档分叉。

use crate::core::geometry::{LogicalSize, PhysicalSize, sanitize_scale};
use crate::core::window::WindowHandle;
use raw_window_handle::{DisplayHandle, HandleError, HasDisplayHandle, HasWindowHandle};
use std::future::Future;
use std::sync::Arc;

/// 渲染初始化 / 运行期的错误。
#[derive(Debug)]
pub enum RenderError {
  /// 没有可用的适配器（缺驱动，或这块表面不被任何后端接受）。
  NoAdapter,
  /// 建表面失败。
  Surface(wgpu::CreateSurfaceError),
  /// 请求设备失败。
  Device(wgpu::RequestDeviceError),
  /// 适配器给不出这块表面的默认配置——等于不支持这个窗口。
  UnsupportedSurface,
  /// 表面已丢失。M1 只上报，不自动重建（重建时机留给平台层统一处理）。
  SurfaceLost,
}

impl std::fmt::Display for RenderError {
  fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    match self {
      Self::NoAdapter => write!(f, "没有可用的图形适配器"),
      Self::Surface(error) => write!(f, "建表面失败：{error}"),
      Self::Device(error) => write!(f, "请求设备失败：{error}"),
      Self::UnsupportedSurface => write!(f, "适配器不支持该窗口的表面"),
      Self::SurfaceLost => write!(f, "表面已丢失"),
    }
  }
}

impl std::error::Error for RenderError {}

/// 用哪些图形后端建实例。
///
/// **Windows 上只开 D3D12**：这台设备的 Intel Gen9 Vulkan 驱动在建设备时会直接 AV 崩掉进程
/// （`adapter.request_device` 一路进到驱动里就没了），D3D12 一切正常。多开一个后端换不来任何
/// 东西，只会换来「在谁的机器上崩、为什么崩」这种排查成本。其他平台照旧全开。
fn instance_backends() -> wgpu::Backends {
  if cfg!(target_os = "windows") {
    wgpu::Backends::DX12
  } else {
    wgpu::Backends::all()
  }
}

/// 设备、队列、表面与它的配置——渲染三档共用的那部分。
pub struct GpuContext {
  device: wgpu::Device,
  queue: wgpu::Queue,
  /// 表面借窗口的句柄建出来，但把窗口的所有权收进了自己的 `Arc`（见 [`SurfaceWindow`]），
  /// 因此这里的生命周期是 `'static`——窗口不会先于表面消失。
  surface: wgpu::Surface<'static>,
  config: wgpu::SurfaceConfiguration,
  /// DPI 缩放因子（来自窗口）。UI 的方片是逻辑像素，要靠它换算成逻辑视口。
  scale_factor: f64,
}

impl GpuContext {
  /// 建上下文：表面 → 适配器 → 设备 → 表面配置。
  ///
  /// 窗口以 `Arc` 交进来是**必须的**：表面要活得比这次调用久（见 [`SurfaceWindow`]）。
  pub fn new(window: Arc<dyn WindowHandle>) -> Result<Self, RenderError> {
    let instance = wgpu::Instance::new(wgpu::InstanceDescriptor {
      backends: instance_backends(),
      ..wgpu::InstanceDescriptor::new_without_display_handle()
    });
    // 显式写 `Surface<'static>`：窗口所有权被 `SurfaceWindow` 收进 `Arc`，表面因此可以一直
    // 持有，不必跟着某个调用栈的借用走。
    let surface: wgpu::Surface<'static> = instance
      .create_surface(wgpu::SurfaceTarget::DisplayAndWindow(Box::new(
        SurfaceWindow(window.clone()),
      )))
      .map_err(RenderError::Surface)?;

    let adapter = block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
      power_preference: wgpu::PowerPreference::HighPerformance,
      compatible_surface: Some(&surface),
      force_fallback_adapter: false,
      apply_limit_buckets: false,
    }))
    .map_err(|_| RenderError::NoAdapter)?;

    let (device, queue) = block_on(adapter.request_device(&wgpu::DeviceDescriptor::default()))
      .map_err(RenderError::Device)?;

    let size = window.size();
    let width = size.width.max(1);
    let height = size.height.max(1);
    let config = surface
      .get_default_config(&adapter, width, height)
      .ok_or(RenderError::UnsupportedSurface)?;
    surface.configure(&device, &config);

    Ok(Self {
      device,
      queue,
      surface,
      config,
      // 窗口就在手边，起始的 DPI 不用等一个事件（后续变化见 `set_scale_factor`）。
      scale_factor: sanitize_scale(window.scale_factor()),
    })
  }

  /// 窗口物理尺寸变了：重配表面。返回**是否真的重配了**。
  ///
  /// 尺寸为 0（最小化）时直接忽略——那时建纹理是非法的，而且也没东西可画；尺寸没变也不重配。
  /// 调用方据此决定要不要跟着重建自己的尺寸相关资源（3D 的深度附件就是这样）。
  pub fn resize(&mut self, size: PhysicalSize) -> bool {
    if size.width == 0 || size.height == 0 {
      return false;
    }
    if size.width == self.config.width && size.height == self.config.height {
      return false;
    }
    self.config.width = size.width;
    self.config.height = size.height;
    self.surface.configure(&self.device, &self.config);
    true
  }

  /// DPI 缩放因子变了（跨显示器拖动、系统缩放调整）。只影响自绘 UI 的换算，表面不用重配。
  pub fn set_scale_factor(&mut self, scale_factor: f64) {
    self.scale_factor = sanitize_scale(scale_factor);
  }

  /// 表面的物理尺寸（像素）。
  pub fn physical_size(&self) -> (u32, u32) {
    (self.config.width, self.config.height)
  }

  /// 表面的宽高比（`宽 / 高`）。
  pub fn aspect(&self) -> f32 {
    self.config.width.max(1) as f32 / self.config.height.max(1) as f32
  }

  /// 表面的像素格式。
  pub fn format(&self) -> wgpu::TextureFormat {
    self.config.format
  }

  /// 逻辑视口尺寸：物理 ÷ DPI。自绘方片的矩形是逻辑像素，NDC 换算要的是它。
  pub fn logical_viewport(&self) -> LogicalSize {
    PhysicalSize::new(self.config.width, self.config.height).to_logical(self.scale_factor)
  }

  /// 取本帧要画的表面纹理。
  ///
  /// `Ok(None)` 是**正常的「这帧没得画」**：超时 / 被遮挡（最小化、被压住）跳过即可，下一帧
  /// 自然恢复；配置过期则按当前配置重配一次，同样等下一帧。只有表面真丢了才报错。
  pub fn acquire(&mut self) -> Result<Option<wgpu::SurfaceTexture>, RenderError> {
    match self.surface.get_current_texture() {
      wgpu::CurrentSurfaceTexture::Success(texture)
      | wgpu::CurrentSurfaceTexture::Suboptimal(texture) => Ok(Some(texture)),
      wgpu::CurrentSurfaceTexture::Timeout | wgpu::CurrentSurfaceTexture::Occluded => Ok(None),
      wgpu::CurrentSurfaceTexture::Outdated => {
        self.surface.configure(&self.device, &self.config);
        Ok(None)
      }
      wgpu::CurrentSurfaceTexture::Lost | wgpu::CurrentSurfaceTexture::Validation => {
        Err(RenderError::SurfaceLost)
      }
    }
  }

  /// 交回取到的纹理，上屏。
  pub fn present(&self, frame: wgpu::SurfaceTexture) {
    self.queue.present(frame);
  }

  /// 设备。将来 CEF 纹理模式要从别的图形 API 导入纹理，得拿它。
  pub fn device(&self) -> &wgpu::Device {
    &self.device
  }

  /// 队列。同上：导入纹理与提交都走它。
  pub fn queue(&self) -> &wgpu::Queue {
    &self.queue
  }
}

/// 把 wgpu 的初始化 future 在主线程上跑完。
///
/// `request_adapter` / `request_device` 是 async 的，但初始化发生在主线程、这里没有执行器；
/// 这两个 future 的完成只靠后台线程的 `Waker`，所以「登记 waker + park 线程」就够，不必为一
/// 次初始化引 pollster 进来。
fn block_on<F: Future>(future: F) -> F::Output {
  use std::task::{Context, Poll, Wake, Waker};

  struct ThreadWake(std::thread::Thread);

  impl Wake for ThreadWake {
    fn wake(self: Arc<Self>) {
      self.0.unpark();
    }

    fn wake_by_ref(self: &Arc<Self>) {
      self.0.unpark();
    }
  }

  let waker = Waker::from(Arc::new(ThreadWake(std::thread::current())));
  let mut context = Context::from_waker(&waker);
  let mut future = std::pin::pin!(future);
  loop {
    match future.as_mut().poll(&mut context) {
      Poll::Ready(output) => return output,
      Poll::Pending => std::thread::park(),
    }
  }
}

/// 把契约层的窗口句柄转成 wgpu 要的原生句柄对。
///
/// `SurfaceTarget::DisplayAndWindow` 要的是 `Box<dyn HasWindowHandle + HasDisplayHandle>`，
/// 而契约层给的是 `Arc<dyn WindowHandle>`（两个 trait 是它的超 trait）。Rust 还不支持 trait
/// object 的向上转型，于是这里做一层薄转发。
///
/// **它同时是表面的所有权依据**：内层是 `Arc`，窗口因此活得不比表面短——`Surface<'static>`
/// 就是靠这一点成立的。
struct SurfaceWindow(Arc<dyn WindowHandle>);

impl HasWindowHandle for SurfaceWindow {
  fn window_handle(&self) -> Result<raw_window_handle::WindowHandle<'_>, HandleError> {
    self.0.window_handle()
  }
}

impl HasDisplayHandle for SurfaceWindow {
  fn display_handle(&self) -> Result<DisplayHandle<'_>, HandleError> {
    self.0.display_handle()
  }
}
