//! UI 出图：独立的字形图集 + 一条正交方片管线。
//!
//! 与 [`crate::render`] 里那套的关系：**只共用设备与 encoder**，别的什么都不共用。方块图集存的是
//! 颜色（16 像素格、Nearest、sRGB），UI 图集存的是**覆盖度**（单通道、64 像素格、Linear、线性
//! 格式），两者的格尺寸、采样方式、生命周期都不同，塞进一张纹理只会让两边都别扭。
//!
//! 一帧的形状（接在主管线之后，同一个 encoder）：
//!
//! ```text
//!   阴影 pass（只写深度）→ 主 pass（颜色 Clear + 深度）→ ui pass（颜色 Load、无深度）→ present
//! ```
//!
//! ui pass **不 clear 颜色**：它就是在已经画好的画面上盖一层。**不带深度附件**意味着 UI 不参与
//! 世界的深度测试，永远在最上——这正是自绘 HUD 要的语义（覆盖层是原生子窗口，在它之上另算）。
//!
//! ## 坐标
//!
//! [`Quad`] 的矩形是**逻辑像素**，而顶点必须是 NDC。换算只用逻辑视口尺寸：物理 = 逻辑 × DPI，
//! 而 NDC = 逻辑 / 逻辑视口，两头一约，DPI 就消失了（逻辑视口 = 物理 / DPI，见 [`super::Renderer`]）。
//! 这条管线因此**不需要任何 uniform**。

use crate::core::geometry::{LogicalRect, LogicalSize};
use crate::ui::{GlyphBitmap, GlyphKey, Quad, QuadSource};
use std::collections::HashMap;

/// 图集每格的边长（像素）。
///
/// 一格装一个字形。64 像素意味着「16 逻辑像素的字 × 4 倍 DPI」仍在格内，HUD 字号远用不满；
/// 装不下的字形会被拒绝上传（见 [`GlyphAtlas::upload`]）。
const CELL: u32 = 64;

/// 图集的列数 / 行数。
const COLUMNS: u32 = 16;
const ROWS: u32 = 16;

/// 图集尺寸（像素）。
const WIDTH: u32 = COLUMNS * CELL;
const HEIGHT: u32 = ROWS * CELL;

/// 能装的字形数上限：总格数减掉 0 号那格——那格是纯白，给实心方片采样。
const MAX_GLYPHS: u32 = COLUMNS * ROWS - 1;

/// 顶点缓冲的初始容量（顶点数）。HUD 一帧几十个方片，几千个顶点是宽裕的起步值。
const INITIAL_CAPACITY: usize = 1024;

/// UI 顶点：NDC 位置 + 图集 uv + sRGB 颜色。
#[repr(C)]
#[derive(Debug, Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct UiVertex {
  position: [f32; 2],
  uv: [f32; 2],
  color: [u8; 4],
}

/// 顶点布局：与 [`UiVertex`] 的 `repr(C)` 内存布局逐字段对齐（对照写错只会花屏，不报错）。
const ATTRIBUTES: [wgpu::VertexAttribute; 3] = [
  wgpu::VertexAttribute {
    format: wgpu::VertexFormat::Float32x2,
    offset: 0,
    shader_location: 0,
  },
  wgpu::VertexAttribute {
    format: wgpu::VertexFormat::Float32x2,
    offset: 8,
    shader_location: 1,
  },
  // 归一化到 0..1 的 sRGB 颜色；线性化在着色器里做一次（混合发生在线性空间）。
  wgpu::VertexAttribute {
    format: wgpu::VertexFormat::Unorm8x4,
    offset: 16,
    shader_location: 2,
  },
];

fn vertex_layout() -> wgpu::VertexBufferLayout<'static> {
  wgpu::VertexBufferLayout {
    array_stride: std::mem::size_of::<UiVertex>() as wgpu::BufferAddress,
    step_mode: wgpu::VertexStepMode::Vertex,
    attributes: &ATTRIBUTES,
  }
}

/// 一个字形在图集里的落点。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct GlyphSlot {
  /// 格下标（0 号格是纯白，字形从 1 起）。
  cell: u32,
  /// 位图的像素尺寸。uv 只覆盖位图那几列几行、**不是整格**，否则方片会把格里的空白也采进来。
  width: u32,
  height: u32,
}

/// 字形图集：单通道覆盖度 + 0 号格一个不透明像素。
struct GlyphAtlas {
  texture: wgpu::Texture,
  view: wgpu::TextureView,
  sampler: wgpu::Sampler,
  /// 字形 → 落点。**只增不减**——UI 文案里出现的字形种类是有限的。
  slots: HashMap<GlyphKey, GlyphSlot>,
  /// 下一个待分配的格下标（0 号格已被纯白占用）。
  next: u32,
}

impl GlyphAtlas {
  fn new(device: &wgpu::Device, queue: &wgpu::Queue) -> Self {
    let texture = device.create_texture(&wgpu::TextureDescriptor {
      label: Some("ui.atlas"),
      size: wgpu::Extent3d {
        width: WIDTH,
        height: HEIGHT,
        depth_or_array_layers: 1,
      },
      mip_level_count: 1,
      sample_count: 1,
      dimension: wgpu::TextureDimension::D2,
      // **不是 sRGB 格式**：这一通道存的是覆盖度而不是颜色，不能被色彩空间转换碰。
      format: wgpu::TextureFormat::R8Unorm,
      usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
      view_formats: &[],
    });

    // 0 号格写一个不透明像素就够：实心方片四个角的 uv 都钉在这一个纹素上（见 [`solid_uv`]），
    // 采样结果恒为 1.0，等价于「不采纹理」。
    queue.write_texture(
      wgpu::TexelCopyTextureInfo {
        texture: &texture,
        mip_level: 0,
        origin: wgpu::Origin3d::ZERO,
        aspect: wgpu::TextureAspect::All,
      },
      &[255],
      wgpu::TexelCopyBufferLayout {
        offset: 0,
        bytes_per_row: Some(1),
        rows_per_image: Some(1),
      },
      wgpu::Extent3d {
        width: 1,
        height: 1,
        depth_or_array_layers: 1,
      },
    );

    Self {
      view: texture.create_view(&wgpu::TextureViewDescriptor::default()),
      sampler: device.create_sampler(&wgpu::SamplerDescriptor {
        label: Some("ui.atlas.sampler"),
        address_mode_u: wgpu::AddressMode::ClampToEdge,
        address_mode_v: wgpu::AddressMode::ClampToEdge,
        address_mode_w: wgpu::AddressMode::ClampToEdge,
        // 线性过滤：字形方片的矩形落在**逻辑**像素上，高 DPI 下未必对齐纹素中心，线性过滤比
        // 最近邻少一些跳动。（方块图集用 Nearest 是因为那是像素风，这里正好相反。）
        mag_filter: wgpu::FilterMode::Linear,
        min_filter: wgpu::FilterMode::Linear,
        ..wgpu::SamplerDescriptor::default()
      }),
      texture,
      slots: HashMap::new(),
      next: 1,
    }
  }

  fn view(&self) -> &wgpu::TextureView {
    &self.view
  }

  fn sampler(&self) -> &wgpu::Sampler {
    &self.sampler
  }

  /// 上传本帧新光栅化的字形。空位图（空格这类没有轮廓的字）不必上传——按约定它根本不产生方片。
  fn upload(&mut self, queue: &wgpu::Queue, glyphs: &[GlyphBitmap]) {
    for glyph in glyphs {
      if glyph.width == 0 || glyph.height == 0 {
        continue;
      }
      if glyph.width > CELL || glyph.height > CELL {
        // 不是 HUD 该用的字号（64 像素格够 16 逻辑像素的字撑到 4 倍 DPI），拒绝而非硬塞，
        // 免得采样出一个糊掉的字形还不知道为什么。
        eprintln!(
          "lxlake: UI 字形 {}×{} 超出图集格子，未上传",
          glyph.width, glyph.height
        );
        continue;
      }
      if self.slots.contains_key(&glyph.key) {
        continue;
      }
      if self.next > MAX_GLYPHS {
        eprintln!(
          "lxlake: UI 字形图集已满（{MAX_GLYPHS} 格），{:?} 未上传",
          glyph.key
        );
        continue;
      }

      let slot = GlyphSlot {
        cell: self.next,
        width: glyph.width,
        height: glyph.height,
      };
      self.next += 1;

      let (x, y) = cell_origin(slot.cell);
      queue.write_texture(
        wgpu::TexelCopyTextureInfo {
          texture: &self.texture,
          mip_level: 0,
          origin: wgpu::Origin3d { x, y, z: 0 },
          aspect: wgpu::TextureAspect::All,
        },
        &glyph.coverage,
        wgpu::TexelCopyBufferLayout {
          offset: 0,
          // 单通道格式，一行就是位图宽那么多字节。`write_texture` 是唯一**不要求** 256 字节
          // 行对齐的拷贝路径（它内部走暂存），所以小位图可以整块灌进去。
          bytes_per_row: Some(glyph.width),
          rows_per_image: Some(glyph.height),
        },
        wgpu::Extent3d {
          width: glyph.width,
          height: glyph.height,
          depth_or_array_layers: 1,
        },
      );

      self.slots.insert(glyph.key, slot);
    }
  }

  /// 一个字形方片的 uv（`u0, v0, u1, v1`）。没上传成功的字形返回 `None`，方片跳过不画。
  fn uv(&self, key: GlyphKey) -> Option<[f32; 4]> {
    self.slots.get(&key).map(|slot| uv_rect(*slot))
  }
}

/// 实心方片的 uv：四个角都指向 0 号格左上角那一个不透明纹素的**中心**。
///
/// 四角取同一点，插值出来的 uv 就是常量，采样结果恒为 1.0——「不采纹理」与「采一张纯白纹理」
/// 在着色器里因此可以是同一条路径。
fn solid_uv() -> [f32; 4] {
  let u = 0.5 / WIDTH as f32;
  let v = 0.5 / HEIGHT as f32;
  [u, v, u, v]
}

/// 字形位图在图集里的 uv。**四周内缩半个纹素**：uv 压在纹素中心上，线性过滤就不会把邻格的
/// 覆盖度混进来（图集最经典的串色）。
fn uv_rect(slot: GlyphSlot) -> [f32; 4] {
  let (x, y) = cell_origin(slot.cell);
  let (x, y) = (x as f32, y as f32);
  let (w, h) = (slot.width as f32, slot.height as f32);
  [
    (x + 0.5) / WIDTH as f32,
    (y + 0.5) / HEIGHT as f32,
    (x + w - 0.5) / WIDTH as f32,
    (y + h - 0.5) / HEIGHT as f32,
  ]
}

/// 格下标 → 网格里那一格的左上角像素。
fn cell_origin(cell: u32) -> (u32, u32) {
  ((cell % COLUMNS) * CELL, (cell / COLUMNS) * CELL)
}

/// 逻辑矩形 → NDC 的 `[left, top, right, bottom]`。
///
/// NDC 的 y 朝上、UI 的 y 朝下，所以上下要翻一次——这是这一层最容易写错的一行。
fn ndc_rect(rect: LogicalRect, viewport: LogicalSize) -> [f32; 4] {
  let to_x = |x: f64| (x / viewport.width * 2.0 - 1.0) as f32;
  let to_y = |y: f64| (1.0 - y / viewport.height * 2.0) as f32;
  [
    to_x(rect.x),
    to_y(rect.y),
    to_x(rect.x + rect.width),
    to_y(rect.y + rect.height),
  ]
}

/// UI 渲染器：图集、管线、以及每帧重建的顶点缓冲。
pub(super) struct UiRenderer {
  atlas: GlyphAtlas,
  pipeline: wgpu::RenderPipeline,
  bind_group: wgpu::BindGroup,
  /// 顶点缓冲：容量按 2 的幂增长，稳态下每帧只 `write_buffer`，不再重建。
  vertices: wgpu::Buffer,
  /// `vertices` 装得下的顶点数。
  capacity: usize,
  /// 每帧复用的顶点暂存：逐帧分配在这一层是纯浪费（见 `apps/lxlake-demo` 的 `StreamOutput`）。
  scratch: Vec<UiVertex>,
}

impl UiRenderer {
  pub(super) fn new(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    format: wgpu::TextureFormat,
  ) -> Self {
    let atlas = GlyphAtlas::new(device, queue);

    let atlas_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
      label: Some("ui.atlas.layout"),
      entries: &[
        wgpu::BindGroupLayoutEntry {
          binding: 0,
          visibility: wgpu::ShaderStages::FRAGMENT,
          ty: wgpu::BindingType::Texture {
            sample_type: wgpu::TextureSampleType::Float { filterable: true },
            view_dimension: wgpu::TextureViewDimension::D2,
            multisampled: false,
          },
          count: None,
        },
        wgpu::BindGroupLayoutEntry {
          binding: 1,
          visibility: wgpu::ShaderStages::FRAGMENT,
          ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
          count: None,
        },
      ],
    });

    let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
      label: Some("ui.atlas"),
      layout: &atlas_layout,
      entries: &[
        wgpu::BindGroupEntry {
          binding: 0,
          resource: wgpu::BindingResource::TextureView(atlas.view()),
        },
        wgpu::BindGroupEntry {
          binding: 1,
          resource: wgpu::BindingResource::Sampler(atlas.sampler()),
        },
      ],
    });

    let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
      label: Some("ui.layout"),
      bind_group_layouts: &[Some(&atlas_layout)],
      immediate_size: 0,
    });

    let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
      label: Some("ui.wgsl"),
      source: wgpu::ShaderSource::Wgsl(include_str!("ui.wgsl").into()),
    });

    let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
      label: Some("ui"),
      layout: Some(&pipeline_layout),
      vertex: wgpu::VertexState {
        module: &module,
        entry_point: Some("vs_ui"),
        compilation_options: wgpu::PipelineCompilationOptions::default(),
        buffers: &[Some(vertex_layout())],
      },
      primitive: wgpu::PrimitiveState {
        topology: wgpu::PrimitiveTopology::TriangleList,
        front_face: wgpu::FrontFace::Ccw,
        // 不剔面：方片没有正反面这回事，剔了只会让绕序写错的方片凭空消失。
        cull_mode: None,
        ..wgpu::PrimitiveState::default()
      },
      // 没有深度附件：UI 不参与世界的深度测试，永远盖在画面之上。
      depth_stencil: None,
      multisample: wgpu::MultisampleState::default(),
      fragment: Some(wgpu::FragmentState {
        module: &module,
        entry_point: Some("fs_ui"),
        compilation_options: wgpu::PipelineCompilationOptions::default(),
        targets: &[Some(wgpu::ColorTargetState {
          format,
          // 覆盖度与不透明度都靠 alpha 混进画面；混合发生在表面目标的线性空间里，顶点色的
          // sRGB → 线性转换因此在着色器里做（见 `ui.wgsl`）。
          blend: Some(wgpu::BlendState::ALPHA_BLENDING),
          write_mask: wgpu::ColorWrites::ALL,
        })],
      }),
      multiview_mask: None,
      cache: None,
    });

    Self {
      atlas,
      pipeline,
      bind_group,
      vertices: create_vertex_buffer(device, INITIAL_CAPACITY),
      capacity: INITIAL_CAPACITY,
      scratch: Vec::new(),
    }
  }

  /// 交来本帧新光栅化的字形（见 [`crate::ui::TextShaper::take_new_glyphs`]）。
  pub(super) fn upload_glyphs(&mut self, queue: &wgpu::Queue, glyphs: &[GlyphBitmap]) {
    self.atlas.upload(queue, glyphs);
  }

  /// 在 `view` 上盖一层 UI。接在主管线之后、同一个 encoder：多余的提交换不来任何东西。
  pub(super) fn draw(
    &mut self,
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    encoder: &mut wgpu::CommandEncoder,
    view: &wgpu::TextureView,
    quads: &[Quad],
    viewport: LogicalSize,
  ) {
    // 视口退化成 0（窗口最小化的那一帧）会让 NDC 变成 `inf`：这一帧不画。
    if quads.is_empty() || viewport.width <= 0.0 || viewport.height <= 0.0 {
      return;
    }

    // 出图。字段级拆分借用：图集要不可变借（查 uv），暂存要可变借——拆开才不打架。
    let count = {
      let Self { atlas, scratch, .. } = self;
      scratch.clear();
      for quad in quads {
        let uv = match quad.source {
          QuadSource::Solid => solid_uv(),
          // 没上传成功的字形（超格 / 图集满）安静跳过：漏一个方片好过整屏画错。
          QuadSource::Glyph(key) => match atlas.uv(key) {
            Some(uv) => uv,
            None => continue,
          },
        };
        let [left, top, right, bottom] = ndc_rect(quad.rect, viewport);
        let color = quad.color;
        // 两个三角形拼一个矩形。uv 的下标与 NDC 的上下相反（图集 y 朝下）。
        scratch.extend_from_slice(&[
          UiVertex {
            position: [left, bottom],
            uv: [uv[0], uv[3]],
            color,
          },
          UiVertex {
            position: [left, top],
            uv: [uv[0], uv[1]],
            color,
          },
          UiVertex {
            position: [right, top],
            uv: [uv[2], uv[1]],
            color,
          },
          UiVertex {
            position: [left, bottom],
            uv: [uv[0], uv[3]],
            color,
          },
          UiVertex {
            position: [right, top],
            uv: [uv[2], uv[1]],
            color,
          },
          UiVertex {
            position: [right, bottom],
            uv: [uv[2], uv[3]],
            color,
          },
        ]);
      }
      scratch.len()
    };

    if count == 0 {
      return;
    }
    self.ensure_capacity(device, count);
    queue.write_buffer(
      &self.vertices,
      0,
      bytemuck::cast_slice(&self.scratch[..count]),
    );

    let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
      label: Some("ui"),
      color_attachments: &[Some(wgpu::RenderPassColorAttachment {
        view,
        depth_slice: None,
        resolve_target: None,
        // **Load 而不是 Clear**：这一层是盖在主管线画好的画面上的。
        ops: wgpu::Operations {
          load: wgpu::LoadOp::Load,
          store: wgpu::StoreOp::Store,
        },
      })],
      depth_stencil_attachment: None,
      timestamp_writes: None,
      occlusion_query_set: None,
      multiview_mask: None,
    });
    pass.set_pipeline(&self.pipeline);
    pass.set_bind_group(0, &self.bind_group, &[]);
    pass.set_vertex_buffer(0, self.vertices.slice(..));
    pass.draw(0..count as u32, 0..1);
  }

  /// 顶点缓冲按需扩容。容量翻倍而不是刚好够：扩容要重建缓冲，每帧重建就白搭了暂存的意义。
  fn ensure_capacity(&mut self, device: &wgpu::Device, vertices: usize) {
    if vertices <= self.capacity {
      return;
    }
    self.capacity = vertices.next_power_of_two();
    self.vertices = create_vertex_buffer(device, self.capacity);
  }
}

/// 建顶点缓冲。内容每帧走 `write_buffer` 灌，所以 `COPY_DST` 是必加的。
fn create_vertex_buffer(device: &wgpu::Device, capacity: usize) -> wgpu::Buffer {
  device.create_buffer(&wgpu::BufferDescriptor {
    label: Some("ui.vertices"),
    size: (capacity * std::mem::size_of::<UiVertex>()) as u64,
    usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
    mapped_at_creation: false,
  })
}

#[cfg(test)]
mod tests {
  use super::*;

  /// 与 `render::pipeline` 的同名测试一个道理：偏移量是手写的，步长一旦不是 20 就会花屏。
  #[test]
  fn vertex_attributes_cover_the_stride_without_overlap() {
    let stride = std::mem::size_of::<UiVertex>() as u64;
    assert_eq!(stride, 20, "顶点布局的偏移量是按这个步长手写的");
    for attribute in &ATTRIBUTES {
      assert!(attribute.offset < stride, "属性落在步长之外");
    }
    assert_eq!(ATTRIBUTES[2].offset + 4, stride);
  }

  /// NDC 的 y 与 UI 的 y 方向相反：铺满视口的矩形要翻成 `top = +1`、`bottom = −1`。
  #[test]
  fn a_full_viewport_rect_maps_to_the_whole_ndc() {
    let viewport = LogicalSize::new(1000.0, 600.0);
    let full = ndc_rect(LogicalRect::new(0.0, 0.0, 1000.0, 600.0), viewport);

    assert_close(full, [-1.0, 1.0, 1.0, -1.0]);
  }

  /// 视口中心的一块：左右 ±0.2，上下 ±1/3。这条钉住「换算只用逻辑视口」这句话。
  #[test]
  fn a_centered_rect_lands_around_the_origin() {
    let viewport = LogicalSize::new(1000.0, 600.0);
    let center = ndc_rect(LogicalRect::new(400.0, 200.0, 200.0, 200.0), viewport);

    assert_close(center, [-0.2, 1.0 / 3.0, 0.2, -1.0 / 3.0]);
  }

  /// 字形方片的 uv 必须落在**自己那一格**里，且不是整格——否则线性过滤会把邻格的覆盖度混进来。
  #[test]
  fn glyph_uv_stays_inside_its_own_cell() {
    for cell in 1..=MAX_GLYPHS {
      let [u0, v0, u1, v1] = uv_rect(GlyphSlot {
        cell,
        width: 16,
        height: 24,
      });
      let (x, y) = cell_origin(cell);
      let (x, y) = (x as f32, y as f32);

      assert!(
        u0 > x / WIDTH as f32 && u1 < (x + CELL as f32) / WIDTH as f32,
        "第 {cell} 格横向上越界：{u0} / {u1}"
      );
      assert!(
        v0 > y / HEIGHT as f32 && v1 < (y + CELL as f32) / HEIGHT as f32,
        "第 {cell} 格纵向上越界：{v0} / {v1}"
      );
      assert!(u1 > u0 && v1 > v0, "uv 必须是正矩形");
    }
  }

  /// 实心方片的 uv 落在 0 号格内部（那里被写成了不透明像素）。
  #[test]
  fn solid_uv_points_at_the_white_texel() {
    let [u, v, u2, v2] = solid_uv();

    assert_eq!([u, v], [u2, v2], "四个角取同一点才是常量采样");
    assert!(u > 0.0 && u < CELL as f32 / WIDTH as f32);
    assert!(v > 0.0 && v < CELL as f32 / HEIGHT as f32);
  }

  /// 网格正好铺满纹理，且 0 号格留给了纯白。
  #[test]
  fn the_grid_covers_the_texture_without_a_remainder() {
    assert_eq!(COLUMNS * CELL, WIDTH);
    assert_eq!(ROWS * CELL, HEIGHT);
    assert_eq!(MAX_GLYPHS + 1, COLUMNS * ROWS);
    assert_eq!(cell_origin(0), (0, 0));
    assert_eq!(cell_origin(COLUMNS), (0, CELL));
    assert_eq!(cell_origin(MAX_GLYPHS), (WIDTH - CELL, HEIGHT - CELL));
  }

  fn assert_close(actual: [f32; 4], expected: [f32; 4]) {
    for (got, want) in actual.iter().zip(expected.iter()) {
      assert!(
        (got - want).abs() < 1e-6,
        "{got} 与 {want} 不符（{actual:?} vs {expected:?}）"
      );
    }
  }
}
