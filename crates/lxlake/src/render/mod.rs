//! wgpu 渲染管线：设备 / 表面 / 深度 / 图集 / 区块资源，以及每帧的两趟绘制。
//!
//! 全部落在 `render` 特性之后：不开该特性时，本模块与 wgpu / naga 都不参与编译，
//! 框架主线因此不依赖渲染（见 `docs/architecture.md` 分层）。M1 起实装。
//!
//! ## 一帧的形状
//!
//! ```text
//!   write_buffer(全局 uniform) → 阴影 pass（只写深度）→ 主 pass（颜色 + 深度）
//!   → ui pass（颜色 Load、无深度）→ present
//! ```
//!
//! 绘制循环里**不切任何绑定**：顶点已经烘焙成世界坐标，因此「区块在哪」不体现在 uniform 上，
//! 一份全局数据就能画完全部区块；区块之间只有顶点 / 索引缓冲的区别。（主 pass 的两组绑定在
//! 进循环前各设一次，循环里不再动。）
//!
//! 最后一趟是自绘 UI（见 [`ui`]）：**只共用这个 encoder**，图集、管线、顶点布局都是它自己的。
//!
//! ## 与世界的分界
//!
//! 本层不认识 `World`：世界内容经 [`crate::world::stream::StreamOutput`] 以
//! [`ChunkMesh`] 的形式递进来（[`Renderer::upload`]），卸载只给一个 [`ChunkPos`]。
//! 于是渲染可以整个换掉而不动世界，反之亦然。

mod atlas;
mod pipeline;
mod ui;

pub use atlas::Atlas;
pub use pipeline::{Globals, Pipelines};

use crate::camera::FlyCamera;
use crate::core::geometry::{PhysicalSize, sanitize_scale};
use crate::core::window::WindowHandle;
use crate::meshing::{ChunkMesh, Vertex};
use crate::ui::{GlyphBitmap, Quad};
use crate::world::chunk::ChunkPos;
use glam::Vec3;
use glam::camera::rh::{proj, view};
use raw_window_handle::{DisplayHandle, HandleError, HasDisplayHandle, HasWindowHandle};
use std::collections::HashMap;
use std::future::Future;
use std::sync::Arc;

/// 深度格式。`Depth32Float` 不用 stencil（M1 没有需要模板的东西），且精度足够。
const DEPTH_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Depth32Float;

/// 阴影贴图边长（像素）。
const SHADOW_SIZE: u32 = 2048;

/// 阴影正交视锥的水平半宽（方块）。
///
/// 视距是 5 区块（160 方块），这张贴图罩住相机周围的一带；再远的地方阴影误差会被雾吃掉。
const SHADOW_EXTENT: f32 = 96.0;

/// 光源到阴影靶点的距离：正交投影的远近平面由它推出。
const LIGHT_DISTANCE: f32 = 180.0;

/// 方向光的行进方向（从光源指向场景，已归一化）。斜着打光，面与面的明暗才拉得开。
const LIGHT_DIR: [f32; 3] = [-0.3995, -0.7990, -0.4494];

/// 清屏色，同时也是雾色（线性空间；末端由 sRGB 表面目标编码）。
const SKY_COLOR: [f32; 3] = [0.46, 0.62, 0.82];

/// 雾的结束距离（方块）。
///
/// 取在视距略内侧（5 区块 = 160 方块）：雾把地形铺满的地方先糊掉，流式边界就不会在眼前
/// 凭空出现。
const FOG_END: f32 = 150.0;

/// NPR 开关（roadmap：M1 只留开关，不做美术调参）。
const NPR_ENABLED: bool = false;

/// NPR 的参数：`(启用时用 1, ramp 阶数, 边缘光强度, 保留)`。
const NPR_PARAMS: [f32; 4] = [1.0, 4.0, 0.18, 0.0];

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

/// 一个区块在 GPU 上的资源。
struct ChunkDraw {
  vertex_buffer: wgpu::Buffer,
  index_buffer: wgpu::Buffer,
  index_count: u32,
}

/// 渲染器：持有设备、表面与全部 GPU 资源。
pub struct Renderer {
  device: wgpu::Device,
  queue: wgpu::Queue,
  /// 表面借窗口的句柄建出来，但把窗口的所有权收进了自己的 `Arc`（见 [`SurfaceWindow`]），
  /// 因此这里的生命周期是 `'static`——窗口不会先于表面消失。
  surface: wgpu::Surface<'static>,
  config: wgpu::SurfaceConfiguration,
  pipelines: Pipelines,
  globals: wgpu::Buffer,
  /// 0 号组：全局 uniform。阴影 pass 只绑它。
  globals_bind_group: wgpu::BindGroup,
  /// 1 号组：图集 / 阴影贴图 / 采样器。只给主管线——阴影 pass 里那张贴图正被当深度附件写，
  /// 同一 pass 内不能又当资源绑（见 [`Pipelines`]）。
  textures_bind_group: wgpu::BindGroup,
  atlas: Atlas,
  /// 主 pass 的深度附件：视图内部持有纹理，随窗口尺寸重建。
  depth_view: wgpu::TextureView,
  shadow_view: wgpu::TextureView,
  chunks: HashMap<ChunkPos, ChunkDraw>,
  /// 自绘 UI：自带图集与管线，只借用同一个 encoder（见 [`ui`]）。
  ui: ui::UiRenderer,
  /// DPI 缩放因子（来自窗口）。UI 的方片是逻辑像素，要靠它换算成逻辑视口。
  scale_factor: f64,
}

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

impl Renderer {
  /// 建渲染器：表面 → 适配器 → 设备 → 管线 / 图集 / 全局资源。
  ///
  /// `tiles` 是图集每格的基色（见 [`Atlas`]）。窗口以 `Arc` 交进来是**必须的**：
  /// 表面要活得比这次调用久（见 [`SurfaceWindow`]）。
  pub fn new(window: Arc<dyn WindowHandle>, tiles: &[[u8; 3]]) -> Result<Self, RenderError> {
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

    let pipelines = pipeline::create_pipelines(&device, config.format);
    let atlas = Atlas::new(&device, &queue, tiles);
    let depth_view = create_depth_view(&device, width, height, "main.depth");
    let shadow_view = create_depth_view(&device, SHADOW_SIZE, SHADOW_SIZE, "shadow.depth");

    let shadow_sampler = device.create_sampler(&wgpu::SamplerDescriptor {
      label: Some("shadow.sampler"),
      address_mode_u: wgpu::AddressMode::ClampToEdge,
      address_mode_v: wgpu::AddressMode::ClampToEdge,
      address_mode_w: wgpu::AddressMode::ClampToEdge,
      mag_filter: wgpu::FilterMode::Linear,
      min_filter: wgpu::FilterMode::Linear,
      // 只有比较采样器能喂给 textureSampleCompareLevel。
      compare: Some(wgpu::CompareFunction::LessEqual),
      ..wgpu::SamplerDescriptor::default()
    });

    // 缓冲先建空：第一帧的 render 会在画之前写满它，GPU 永远读不到未初始化的内容。
    let globals = device.create_buffer(&wgpu::BufferDescriptor {
      label: Some("globals"),
      size: Globals::SIZE,
      usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
      mapped_at_creation: false,
    });

    let globals_bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
      label: Some("globals"),
      layout: &pipelines.globals_layout,
      entries: &[wgpu::BindGroupEntry {
        binding: 0,
        resource: globals.as_entire_binding(),
      }],
    });

    let textures_bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
      label: Some("textures"),
      layout: &pipelines.textures_layout,
      entries: &[
        wgpu::BindGroupEntry {
          binding: 0,
          resource: wgpu::BindingResource::TextureView(atlas.view()),
        },
        wgpu::BindGroupEntry {
          binding: 1,
          resource: wgpu::BindingResource::Sampler(atlas.sampler()),
        },
        wgpu::BindGroupEntry {
          binding: 2,
          resource: wgpu::BindingResource::TextureView(&shadow_view),
        },
        wgpu::BindGroupEntry {
          binding: 3,
          resource: wgpu::BindingResource::Sampler(&shadow_sampler),
        },
      ],
    });

    let ui = ui::UiRenderer::new(&device, &queue, config.format);

    Ok(Self {
      device,
      queue,
      surface,
      config,
      pipelines,
      globals,
      globals_bind_group,
      textures_bind_group,
      atlas,
      depth_view,
      shadow_view,
      chunks: HashMap::new(),
      ui,
      // 窗口就在手边，起始的 DPI 不用等一个事件（后续变化见 `set_scale_factor`）。
      scale_factor: sanitize_scale(window.scale_factor()),
    })
  }

  /// 窗口物理尺寸变了：重配表面，并换掉主 pass 的深度附件。
  ///
  /// 尺寸为 0（最小化）时直接忽略——那时建纹理是非法的，而且也没东西可画。
  pub fn resize(&mut self, size: PhysicalSize) {
    if size.width == 0 || size.height == 0 {
      return;
    }
    if size.width == self.config.width && size.height == self.config.height {
      return;
    }
    self.config.width = size.width;
    self.config.height = size.height;
    self.surface.configure(&self.device, &self.config);
    self.depth_view = create_depth_view(&self.device, size.width, size.height, "main.depth");
  }

  /// DPI 缩放因子变了（跨显示器拖动、系统缩放调整）。只影响自绘 UI 的换算，表面不用重配。
  pub fn set_scale_factor(&mut self, scale_factor: f64) {
    self.scale_factor = sanitize_scale(scale_factor);
  }

  /// 交来本帧新光栅化的字形，增量补进 UI 图集（见 [`crate::ui::TextShaper::take_new_glyphs`]）。
  pub fn upload_glyphs(&mut self, glyphs: &[GlyphBitmap]) {
    self.ui.upload_glyphs(&self.queue, glyphs);
  }

  /// 上传（或覆盖）一个区块的几何体。
  pub fn upload(&mut self, mesh: &ChunkMesh) {
    if mesh.is_empty() {
      self.unload(mesh.pos);
      return;
    }

    let vertices = to_world_vertices(mesh);
    let vertex_buffer = self.create_buffer(
      "chunk.vertices",
      bytemuck::cast_slice(&vertices),
      wgpu::BufferUsages::VERTEX,
    );
    let index_buffer = self.create_buffer(
      "chunk.indices",
      bytemuck::cast_slice(&mesh.indices),
      wgpu::BufferUsages::INDEX,
    );

    self.chunks.insert(
      mesh.pos,
      ChunkDraw {
        vertex_buffer,
        index_buffer,
        index_count: mesh.indices.len() as u32,
      },
    );
  }

  /// 回收一个区块的 GPU 资源。没上传过的位置安静跳过（流式那边可能已经先卸载了）。
  pub fn unload(&mut self, pos: ChunkPos) {
    self.chunks.remove(&pos);
  }

  /// 当前在 GPU 上的区块数。
  pub fn uploaded_chunks(&self) -> usize {
    self.chunks.len()
  }

  /// 画一帧。`camera` 提供观察投影与眼睛位置，`ui` 是要盖在画面上的自绘方片（逻辑像素；
  /// 没有 HUD 时给空切片，那个 pass 连带整个 UI 渲染都不发生）。
  pub fn render(&mut self, camera: &FlyCamera, ui: &[Quad]) -> Result<(), RenderError> {
    let eye = camera.position();
    let globals = Globals {
      view_proj: camera.view_projection(self.aspect()),
      light_view_proj: light_view_projection(eye),
      camera_pos: [eye[0], eye[1], eye[2], 1.0],
      light_dir: [LIGHT_DIR[0], LIGHT_DIR[1], LIGHT_DIR[2], 0.0],
      atlas: [
        self.atlas.columns() as f32,
        self.atlas.rows() as f32,
        atlas::TILE_SIZE as f32,
        0.0,
      ],
      fog: [SKY_COLOR[0], SKY_COLOR[1], SKY_COLOR[2], FOG_END],
      params: if NPR_ENABLED { NPR_PARAMS } else { [0.0; 4] },
    };
    self
      .queue
      .write_buffer(&self.globals, 0, globals.as_bytes());

    let frame = match self.surface.get_current_texture() {
      wgpu::CurrentSurfaceTexture::Success(texture)
      | wgpu::CurrentSurfaceTexture::Suboptimal(texture) => texture,
      // 超时 / 被遮挡（最小化、被别的窗口压住）：这一帧没得画，跳过即可，下一帧自然恢复。
      wgpu::CurrentSurfaceTexture::Timeout | wgpu::CurrentSurfaceTexture::Occluded => {
        return Ok(());
      }
      // 配置过期：按当前配置重配一次，下一帧就正常了。
      wgpu::CurrentSurfaceTexture::Outdated => {
        self.surface.configure(&self.device, &self.config);
        return Ok(());
      }
      wgpu::CurrentSurfaceTexture::Lost | wgpu::CurrentSurfaceTexture::Validation => {
        return Err(RenderError::SurfaceLost);
      }
    };
    let view = frame
      .texture
      .create_view(&wgpu::TextureViewDescriptor::default());

    let mut encoder = self
      .device
      .create_command_encoder(&wgpu::CommandEncoderDescriptor {
        label: Some("frame"),
      });

    {
      let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
        label: Some("shadow"),
        color_attachments: &[],
        depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
          view: &self.shadow_view,
          depth_ops: Some(wgpu::Operations {
            load: wgpu::LoadOp::Clear(1.0),
            store: wgpu::StoreOp::Store,
          }),
          stencil_ops: None,
        }),
        timestamp_writes: None,
        occlusion_query_set: None,
        multiview_mask: None,
      });
      pass.set_pipeline(&self.pipelines.shadow);
      pass.set_bind_group(0, &self.globals_bind_group, &[]);
      draw_chunks(&mut pass, &self.chunks);
    }

    {
      let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
        label: Some("main"),
        color_attachments: &[Some(wgpu::RenderPassColorAttachment {
          view: &view,
          depth_slice: None,
          resolve_target: None,
          ops: wgpu::Operations {
            load: wgpu::LoadOp::Clear(wgpu::Color {
              r: f64::from(SKY_COLOR[0]),
              g: f64::from(SKY_COLOR[1]),
              b: f64::from(SKY_COLOR[2]),
              a: 1.0,
            }),
            store: wgpu::StoreOp::Store,
          },
        })],
        depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
          view: &self.depth_view,
          depth_ops: Some(wgpu::Operations {
            load: wgpu::LoadOp::Clear(1.0),
            store: wgpu::StoreOp::Store,
          }),
          stencil_ops: None,
        }),
        timestamp_writes: None,
        occlusion_query_set: None,
        multiview_mask: None,
      });
      pass.set_pipeline(&self.pipelines.main);
      pass.set_bind_group(0, &self.globals_bind_group, &[]);
      pass.set_bind_group(1, &self.textures_bind_group, &[]);
      draw_chunks(&mut pass, &self.chunks);
    }

    // UI 接在同一个 encoder 的最后：它要盖在**已经画好的**画面上（`LoadOp::Load`），另起一次
    // 提交只会多一次开销。
    self.ui.draw(
      &self.device,
      &self.queue,
      &mut encoder,
      &view,
      ui,
      PhysicalSize::new(self.config.width, self.config.height).to_logical(self.scale_factor),
    );

    self.queue.submit(Some(encoder.finish()));
    self.queue.present(frame);
    Ok(())
  }

  /// 当前表面的宽高比（`宽 / 高`）。
  fn aspect(&self) -> f32 {
    self.config.width.max(1) as f32 / self.config.height.max(1) as f32
  }

  fn create_buffer(&self, label: &str, contents: &[u8], usage: wgpu::BufferUsages) -> wgpu::Buffer {
    let size = contents.len().max(4) as u64;
    let buffer = self.device.create_buffer(&wgpu::BufferDescriptor {
      label: Some(label),
      size,
      // 内容走 `write_buffer` 灌进去，所以 COPY_DST 是必加的：调用方只声明它要**用**这份缓冲
      // 干什么（顶点 / 索引），灌数据那一侧的用途由这里统一补。
      usage: usage | wgpu::BufferUsages::COPY_DST,
      mapped_at_creation: false,
    });
    self.queue.write_buffer(&buffer, 0, contents);
    buffer
  }
}

/// 把区块局部坐标烘焙成世界坐标。
///
/// 顶点里带世界坐标是 M1 的一个关键取舍：于是「区块在哪」不进 uniform，一份全局数据就能画完
/// 所有区块，绘制循环里没有逐区块的绑定切换。代价是区块移动时得重传顶点——M1 的区块**生成
/// 后不再改**，这个代价不存在。
fn to_world_vertices(mesh: &ChunkMesh) -> Vec<Vertex> {
  let origin = mesh.pos.origin();
  mesh
    .vertices
    .iter()
    .map(|vertex| Vertex {
      position: [
        vertex.position[0] + origin[0] as f32,
        vertex.position[1] + origin[1] as f32,
        vertex.position[2] + origin[2] as f32,
      ],
      ..*vertex
    })
    .collect()
}

/// 方向光的观察投影：以相机所在处为靶，正交范围固定。
fn light_view_projection(focus: [f32; 3]) -> [[f32; 4]; 4] {
  // 靶点对齐到方块网格（向下取整，和方块坐标同一套换算）：相机在方块内的小幅移动不会让
  // 光源跟着动，否则阴影边缘会一格一格地爬。
  let target = Vec3::new(focus[0].floor(), focus[1].floor(), focus[2].floor());
  let direction = Vec3::from_array(LIGHT_DIR);
  let eye = target - direction * LIGHT_DISTANCE;
  let view = view::look_to_mat4(eye, direction, Vec3::Y);
  // directx 那套 = 「Z 在 0..1、Y 朝上」，与 wgpu 的 NDC 一致（见 `camera`）。
  let projection = proj::directx::orthographic(
    -SHADOW_EXTENT,
    SHADOW_EXTENT,
    -SHADOW_EXTENT,
    SHADOW_EXTENT,
    1.0,
    LIGHT_DISTANCE * 2.0,
  );
  (projection * view).to_cols_array_2d()
}

/// 建一张深度纹理，返回它的视图。
///
/// 只留视图：`TextureView` 内部持有纹理的引用，纹理不会因为局部变量离开作用域就没了；
/// 深度图除了当附件（阴影还要当纹理）之外没有别的用途，多存一个句柄只是多一处要同步重建。
fn create_depth_view(
  device: &wgpu::Device,
  width: u32,
  height: u32,
  label: &str,
) -> wgpu::TextureView {
  device
    .create_texture(&wgpu::TextureDescriptor {
      label: Some(label),
      size: wgpu::Extent3d {
        width,
        height,
        depth_or_array_layers: 1,
      },
      mip_level_count: 1,
      sample_count: 1,
      dimension: wgpu::TextureDimension::D2,
      format: DEPTH_FORMAT,
      // 主 pass 的深度只当附件，但阴影贴图要被采样；两者共用一条构造路径，多要一个用途不亏。
      usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::TEXTURE_BINDING,
      view_formats: &[],
    })
    .create_view(&wgpu::TextureViewDescriptor::default())
}

/// 两趟 pass 共用的绘制循环：管线与绑定由调用方先设好。
fn draw_chunks(pass: &mut wgpu::RenderPass<'_>, chunks: &HashMap<ChunkPos, ChunkDraw>) {
  for draw in chunks.values() {
    pass.set_vertex_buffer(0, draw.vertex_buffer.slice(..));
    pass.set_index_buffer(draw.index_buffer.slice(..), wgpu::IndexFormat::Uint32);
    pass.draw_indexed(0..draw.index_count, 0, 0..1);
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

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn light_direction_is_normalized() {
    let length = Vec3::from_array(LIGHT_DIR).length();
    assert!((length - 1.0).abs() < 1e-3, "光方向要归一化，实得 {length}");
  }

  #[test]
  fn world_vertices_bake_the_chunk_origin() {
    let mesh = ChunkMesh {
      pos: ChunkPos::new(1, -1, 2),
      vertices: vec![
        Vertex {
          position: [0.0, 0.0, 0.0],
          normal: [0.0, 1.0, 0.0],
          uv: [0.0, 0.0],
          tile: 3,
          _pad: 0,
        },
        Vertex {
          position: [32.0, 5.0, 32.0],
          normal: [1.0, 0.0, 0.0],
          uv: [2.0, 1.0],
          tile: 7,
          _pad: 0,
        },
      ],
      indices: vec![0, 1, 0],
    };

    let vertices = to_world_vertices(&mesh);
    // 区块原点 = (32, -32, 64)。
    assert_eq!(vertices[0].position, [32.0, -32.0, 64.0]);
    assert_eq!(vertices[1].position, [64.0, -27.0, 96.0]);
    // 除位置之外的字段原样带过去。
    assert_eq!(vertices[1].normal, [1.0, 0.0, 0.0]);
    assert_eq!(vertices[1].uv, [2.0, 1.0]);
    assert_eq!(vertices[1].tile, 7);
  }

  #[test]
  fn light_view_projection_puts_the_target_inside_the_clip_range() {
    let focus = [12.0, 40.0, -8.0];
    let light_view_proj = glam::Mat4::from_cols_array_2d(&light_view_projection(focus));
    let clip = light_view_proj * Vec3::from_array(focus).extend(1.0);

    assert!(clip.w > 0.0);
    let ndc = clip.truncate() / clip.w;
    assert!(ndc.x.abs() <= 1.0 && ndc.y.abs() <= 1.0, "靶点在视锥内");
    assert!((0.0..=1.0).contains(&ndc.z), "深度落在 wgpu 的 0..1");
  }

  #[test]
  fn light_view_projection_is_stable_within_a_block() {
    // 靶点在方块内移动时，光的视图投影不该变——否则阴影边缘会随相机爬行。
    let a = light_view_projection([10.1, 4.2, -3.4]);
    let b = light_view_projection([10.9, 4.4, -3.1]);
    assert_eq!(a, b);
  }
}
