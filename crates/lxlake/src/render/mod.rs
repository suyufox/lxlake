//! wgpu 渲染：设备 / 表面（[`GpuContext`]）、自绘方片管线（`ui.rs`）、3D 管线与区块资源。
//!
//! 模块随 **`gpu`** 特性编译，内部再按两档收：
//!
//! ```text
//!   gpu        GpuContext —— 设备 / 队列 / 表面 / 配置 / resize / 取帧 / present
//!   ui-render  + ui.rs    —— 方片管线与 UI 字形图集（框架主线的自绘 UI 走这一档）
//!   render     + 3D       —— 管线 / 方块图集 / shader.wgsl / 深度与阴影 / 区块资源
//! ```
//!
//! 分档按**消费者**切，不按概念切：`gpu` 那一档的消费者是将来 CEF 的纹理模式（要设备与取帧，
//! 不要方片管线），`ui-render` 那一档的消费者是编辑器（要自绘 UI，不要 3D）。
//! 于是 [`Renderer`] **只有一个类型**、在三种档位下都存在，只是字段与方法按档收——
//! `runtime` 侧因此不必知道当前是哪一档（见 `docs/architecture.md` 的「feature 分档」）。
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
//! 最后一趟是自绘 UI（`ui.rs`）：**只共用这个 encoder**，图集、管线、顶点布局都是它自己的。
//! 只开 `ui-render` 时这条链路没有前两趟——见 [`Renderer::draw_ui`]。
//!
//! ## 与世界的分界
//!
//! 本层不认识 `World`：世界内容经 [`crate::world::stream::StreamOutput`] 以 `ChunkMesh` 的形式
//! 递进来（[`Renderer::upload`]），卸载只给一个 `ChunkPos`。于是渲染可以整个换掉而不动世界，
//! 反之亦然。

#[cfg(feature = "render")]
mod atlas;
mod gpu;
#[cfg(feature = "render")]
mod pipeline;
#[cfg(feature = "ui-render")]
mod ui;

#[cfg(feature = "render")]
pub use atlas::Atlas;
pub use gpu::{GpuContext, RenderError};
#[cfg(feature = "render")]
pub use pipeline::{Globals, Pipelines};

use crate::core::geometry::PhysicalSize;
use crate::core::window::WindowHandle;
#[cfg(feature = "ui-render")]
use crate::ui::{GlyphBitmap, Quad};
use std::sync::Arc;

#[cfg(feature = "render")]
use crate::camera::FlyCamera;
#[cfg(feature = "render")]
use crate::meshing::{ChunkMesh, Vertex};
#[cfg(feature = "render")]
use crate::world::chunk::ChunkPos;
#[cfg(feature = "render")]
use glam::Vec3;
#[cfg(feature = "render")]
use glam::camera::rh::{proj, view};
#[cfg(feature = "render")]
use std::collections::HashMap;

/// 深度格式。`Depth32Float` 不用 stencil（M1 没有需要模板的东西），且精度足够。
#[cfg(feature = "render")]
const DEPTH_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Depth32Float;

/// 阴影贴图边长（像素）。
#[cfg(feature = "render")]
const SHADOW_SIZE: u32 = 2048;

/// 阴影正交视锥的水平半宽（方块）。
///
/// 视距是 5 区块（160 方块），这张贴图罩住相机周围的一带；再远的地方阴影误差会被雾吃掉。
#[cfg(feature = "render")]
const SHADOW_EXTENT: f32 = 96.0;

/// 光源到阴影靶点的距离：正交投影的远近平面由它推出。
#[cfg(feature = "render")]
const LIGHT_DISTANCE: f32 = 180.0;

/// 方向光的行进方向（从光源指向场景，已归一化）。斜着打光，面与面的明暗才拉得开。
#[cfg(feature = "render")]
const LIGHT_DIR: [f32; 3] = [-0.3995, -0.7990, -0.4494];

/// 清屏色，同时也是雾色（线性空间；末端由 sRGB 表面目标编码）。
#[cfg(feature = "render")]
const SKY_COLOR: [f32; 3] = [0.46, 0.62, 0.82];

/// 雾的结束距离（方块）。
///
/// 取在视距略内侧（5 区块 = 160 方块）：雾把地形铺满的地方先糊掉，流式边界就不会在眼前
/// 凭空出现。
#[cfg(feature = "render")]
const FOG_END: f32 = 150.0;

/// NPR 开关（roadmap：M1 只留开关，不做美术调参）。
#[cfg(feature = "render")]
const NPR_ENABLED: bool = false;

/// NPR 的参数：`(启用时用 1, ramp 阶数, 边缘光强度, 保留)`。
#[cfg(feature = "render")]
const NPR_PARAMS: [f32; 4] = [1.0, 4.0, 0.18, 0.0];

/// 不带 3D 时的底色（线性空间）。UI pass 走 `LoadOp::Load`，不清屏，所以这一档必须自己先垫一层——
/// 否则第一帧拿到的是未初始化的表面纹理。带 3D 时不需要它：主 pass 已经用天空色清过。
#[cfg(all(feature = "ui-render", not(feature = "render")))]
const SHELL_COLOR: wgpu::Color = wgpu::Color {
  r: 0.07,
  g: 0.07,
  b: 0.09,
  a: 1.0,
};

/// 一个区块在 GPU 上的资源。
#[cfg(feature = "render")]
struct ChunkDraw {
  vertex_buffer: wgpu::Buffer,
  index_buffer: wgpu::Buffer,
  index_count: u32,
}

/// 渲染器：持有表面上下文与（按档）自绘方片管线、3D 资源。
///
/// **单一类型、三种档位**：字段按档增减，方法按档收。这样 `runtime` 的「按窗建一份、resize
/// 时喂给它、销毁时收掉」在三种档位下是同一份代码，不必分叉。
pub struct Renderer {
  /// 地板档：设备 / 队列 / 表面 / 配置 / DPI。
  gpu: GpuContext,
  /// 自绘 UI：自带图集与管线，只借用同一个 encoder（见 `ui.rs`）。
  #[cfg(feature = "ui-render")]
  ui: ui::UiRenderer,
  #[cfg(feature = "render")]
  pipelines: Pipelines,
  #[cfg(feature = "render")]
  globals: wgpu::Buffer,
  /// 0 号组：全局 uniform。阴影 pass 只绑它。
  #[cfg(feature = "render")]
  globals_bind_group: wgpu::BindGroup,
  /// 1 号组：图集 / 阴影贴图 / 采样器。只给主管线——阴影 pass 里那张贴图正被当深度附件写，
  /// 同一 pass 内不能又当资源绑（见 [`Pipelines`]）。
  #[cfg(feature = "render")]
  textures_bind_group: wgpu::BindGroup,
  #[cfg(feature = "render")]
  atlas: Atlas,
  /// 主 pass 的深度附件：视图内部持有纹理，随窗口尺寸重建。
  #[cfg(feature = "render")]
  depth_view: wgpu::TextureView,
  #[cfg(feature = "render")]
  shadow_view: wgpu::TextureView,
  #[cfg(feature = "render")]
  chunks: HashMap<ChunkPos, ChunkDraw>,
}

/// 只开 `gpu` / `ui-render` 时的构造：没有 3D，所以没有图集与阴影参数要传。
#[cfg(not(feature = "render"))]
impl Renderer {
  /// 建渲染器：表面上下文（+ 自绘方片管线）。
  ///
  /// 窗口以 `Arc` 交进来是**必须的**：表面要活得比这次调用久（见 [`GpuContext`]）。
  pub fn new(window: Arc<dyn WindowHandle>) -> Result<Self, RenderError> {
    let gpu = GpuContext::new(window)?;
    #[cfg(feature = "ui-render")]
    let ui = ui::UiRenderer::new(gpu.device(), gpu.queue(), gpu.format());
    Ok(Self {
      gpu,
      #[cfg(feature = "ui-render")]
      ui,
    })
  }
}

/// 三档共用的表面侧方法。
impl Renderer {
  /// 窗口物理尺寸变了：重配表面（开 3D 时还要换掉主 pass 的深度附件）。
  ///
  /// 尺寸为 0（最小化）或没变时整个忽略——那时建纹理是非法的，而且也没东西可画。
  pub fn resize(&mut self, size: PhysicalSize) {
    if self.gpu.resize(size) {
      self.on_surface_resized();
    }
  }

  /// 表面真被重配之后的后续动作。**不带 3D 时没有后续动作**——深度附件是 3D 那一档的资源。
  #[cfg(not(feature = "render"))]
  fn on_surface_resized(&mut self) {}

  #[cfg(feature = "render")]
  fn on_surface_resized(&mut self) {
    let (width, height) = self.gpu.physical_size();
    self.depth_view = create_depth_view(self.gpu.device(), width, height, "main.depth");
  }

  /// DPI 缩放因子变了（跨显示器拖动、系统缩放调整）。只影响自绘 UI 的换算，表面不用重配。
  pub fn set_scale_factor(&mut self, scale_factor: f64) {
    self.gpu.set_scale_factor(scale_factor);
  }

  /// 交来本帧新光栅化的字形，增量补进 UI 图集（见 [`crate::ui::TextShaper::take_new_glyphs`]）。
  #[cfg(feature = "ui-render")]
  pub fn upload_glyphs(&mut self, glyphs: &[GlyphBitmap]) {
    self.ui.upload_glyphs(self.gpu.queue(), glyphs);
  }

  /// 把自绘方片盖到 `view` 上（接在同一个 encoder 的最后）。
  ///
  /// `LoadOp::Load`：它就是在**已经画好的**画面上盖一层，另起一次提交只会多一次开销。
  #[cfg(feature = "ui-render")]
  fn encode_ui(
    &mut self,
    encoder: &mut wgpu::CommandEncoder,
    view: &wgpu::TextureView,
    quads: &[Quad],
  ) {
    self.ui.draw(
      self.gpu.device(),
      self.gpu.queue(),
      encoder,
      view,
      quads,
      self.gpu.logical_viewport(),
    );
  }
}

/// 只画自绘方片的那一档（`ui-render`，不带 `render`）：框架主线的入口。
///
/// 带 3D 时走 [`Renderer::render`]——它内部会调同一个 `encode_ui`，两条路径出图完全一致。
#[cfg(all(feature = "ui-render", not(feature = "render")))]
impl Renderer {
  /// 画一帧：清屏 + 盖上方片。`quads` 是逻辑像素；空切片时连 UI pass 都不发生（只剩清屏）。
  pub fn draw_ui(&mut self, quads: &[Quad]) -> Result<(), RenderError> {
    let Some(frame) = self.gpu.acquire()? else {
      return Ok(());
    };
    let view = frame
      .texture
      .create_view(&wgpu::TextureViewDescriptor::default());

    let mut encoder = self
      .gpu
      .device()
      .create_command_encoder(&wgpu::CommandEncoderDescriptor {
        label: Some("frame"),
      });

    // 只在开 3D 时才有「主 pass 先清屏」；这里得自己垫一层（不然第一帧是未初始化的表面纹理）。
    encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
      label: Some("shell"),
      color_attachments: &[Some(wgpu::RenderPassColorAttachment {
        view: &view,
        depth_slice: None,
        resolve_target: None,
        ops: wgpu::Operations {
          load: wgpu::LoadOp::Clear(SHELL_COLOR),
          store: wgpu::StoreOp::Store,
        },
      })],
      depth_stencil_attachment: None,
      timestamp_writes: None,
      occlusion_query_set: None,
      multiview_mask: None,
    });
    self.encode_ui(&mut encoder, &view, quads);

    self.gpu.queue().submit(Some(encoder.finish()));
    self.gpu.present(frame);
    Ok(())
  }
}

/// 带 3D 的那一档（`render`，它同时含 `ui-render`）。
#[cfg(feature = "render")]
impl Renderer {
  /// 建渲染器：表面 → 适配器 → 设备 → 管线 / 图集 / 全局资源。
  ///
  /// `tiles` 是图集每格的基色（见 [`Atlas`]）。窗口以 `Arc` 交进来是**必须的**：
  /// 表面要活得比这次调用久（见 [`GpuContext`]）。
  pub fn new(window: Arc<dyn WindowHandle>, tiles: &[[u8; 3]]) -> Result<Self, RenderError> {
    let gpu = GpuContext::new(window)?;
    let (device, queue, format) = (gpu.device(), gpu.queue(), gpu.format());

    let pipelines = pipeline::create_pipelines(device, format);
    let atlas = Atlas::new(device, queue, tiles);
    let (width, height) = gpu.physical_size();
    let depth_view = create_depth_view(device, width, height, "main.depth");
    let shadow_view = create_depth_view(device, SHADOW_SIZE, SHADOW_SIZE, "shadow.depth");

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

    let ui = ui::UiRenderer::new(device, queue, format);

    Ok(Self {
      gpu,
      ui,
      pipelines,
      globals,
      globals_bind_group,
      textures_bind_group,
      atlas,
      depth_view,
      shadow_view,
      chunks: HashMap::new(),
    })
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

  /// 画一帧。`camera` 提供观察投影与眼睛位置，`quads` 是要盖在画面上的自绘方片（逻辑像素；
  /// 没有 HUD 时给空切片，那个 pass 连带整个 UI 渲染都不发生）。
  pub fn render(&mut self, camera: &FlyCamera, quads: &[Quad]) -> Result<(), RenderError> {
    let eye = camera.position();
    let globals = Globals {
      view_proj: camera.view_projection(self.gpu.aspect()),
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
      .gpu
      .queue()
      .write_buffer(&self.globals, 0, globals.as_bytes());

    // 超时 / 被遮挡 / 配置过期：这一帧没得画，跳过即可，下一帧自然恢复（见 `GpuContext::acquire`）。
    let Some(frame) = self.gpu.acquire()? else {
      return Ok(());
    };
    let view = frame
      .texture
      .create_view(&wgpu::TextureViewDescriptor::default());

    let mut encoder = self
      .gpu
      .device()
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

    self.encode_ui(&mut encoder, &view, quads);

    self.gpu.queue().submit(Some(encoder.finish()));
    self.gpu.present(frame);
    Ok(())
  }

  fn create_buffer(&self, label: &str, contents: &[u8], usage: wgpu::BufferUsages) -> wgpu::Buffer {
    let size = contents.len().max(4) as u64;
    let buffer = self.gpu.device().create_buffer(&wgpu::BufferDescriptor {
      label: Some(label),
      size,
      // 内容走 `write_buffer` 灌进去，所以 COPY_DST 是必加的：调用方只声明它要**用**这份缓冲
      // 干什么（顶点 / 索引），灌数据那一侧的用途由这里统一补。
      usage: usage | wgpu::BufferUsages::COPY_DST,
      mapped_at_creation: false,
    });
    self.gpu.queue().write_buffer(&buffer, 0, contents);
    buffer
  }
}

/// 把区块局部坐标烘焙成世界坐标。
///
/// 顶点里带世界坐标是 M1 的一个关键取舍：于是「区块在哪」不进 uniform，一份全局数据就能画完
/// 所有区块，绘制循环里没有逐区块的绑定切换。代价是区块移动时得重传顶点——M1 的区块**生成
/// 后不再改**，这个代价不存在。
#[cfg(feature = "render")]
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
#[cfg(feature = "render")]
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
#[cfg(feature = "render")]
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
#[cfg(feature = "render")]
fn draw_chunks(pass: &mut wgpu::RenderPass<'_>, chunks: &HashMap<ChunkPos, ChunkDraw>) {
  for draw in chunks.values() {
    pass.set_vertex_buffer(0, draw.vertex_buffer.slice(..));
    pass.set_index_buffer(draw.index_buffer.slice(..), wgpu::IndexFormat::Uint32);
    pass.draw_indexed(0..draw.index_count, 0, 0..1);
  }
}

#[cfg(all(test, feature = "render"))]
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
