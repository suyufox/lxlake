//! 管线与全局 uniform：GPU 侧的固定开销，与世界内容无关。
//!
//! 两条管线共用一套顶点布局；绑定拆成两组（见 [`Pipelines`]）。
//!
//! - **主光路**：背面剔除 + 深度测试，输出颜色
//! - **阴影**：正面剔除 + 深度偏移，只写深度；顶点缓冲直接复用（顶点已经是世界坐标）
//!
//! 只写深度的 pass 刻意**不带片元阶段**：没有任何东西要算，留着它只是白白让驱动跑一趟。

use crate::meshing::Vertex;

/// 全局 uniform 的浮点数个数。
///
/// 与 `shader.wgsl` 的 `Globals` 逐字段对齐：两个 `mat4x4`（32）+ 五个 `vec4`（20）= 52。
const GLOBALS_FLOATS: usize = 52;

/// 全局 uniform：一份数据描述「这一帧的相机与光照」，与区块数量无关。
///
/// `repr(C)` + `Pod`：写入 GPU 的字节就是这里的字段顺序，且**不能有隐式填充**——GPU 不会
/// 帮你对齐，多出来的空洞会把后面的字段整体读错。
#[repr(C)]
#[derive(Debug, Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
pub struct Globals {
  /// 相机观察投影矩阵（列主序）。
  pub view_proj: [[f32; 4]; 4],
  /// 方向光的观察投影矩阵（列主序），阴影采样用。
  pub light_view_proj: [[f32; 4]; 4],
  /// 相机世界坐标（xyz；w 仅作对齐占位）。
  pub camera_pos: [f32; 4],
  /// 方向光的**行进方向**（从光源指向场景，已归一化）。
  pub light_dir: [f32; 4],
  /// 图集排布：`(列数, 行数, 格边长像素, 保留)`。
  pub atlas: [f32; 4],
  /// 雾：`(r, g, b, 结束距离)`。
  pub fog: [f32; 4],
  /// NPR 开关：`(是否启用, ramp 阶数, 边缘光强度, 保留)`。
  pub params: [f32; 4],
}

impl Globals {
  /// uniform 缓冲需要的大小（字节）。
  pub const SIZE: u64 = GLOBALS_FLOATS as u64 * 4;

  /// 字节视图，交给 `Queue::write_buffer`。
  pub fn as_bytes(&self) -> &[u8] {
    bytemuck::bytes_of(self)
  }
}

/// 顶点布局：与 [`Vertex`] 的 `repr(C)` 内存布局逐字段对齐。
///
/// 这条对照一旦写错，画面不会报错、只会花掉，所以偏移量就按字段顺序手写出来，别用推导。
const ATTRIBUTES: [wgpu::VertexAttribute; 4] = [
  wgpu::VertexAttribute {
    format: wgpu::VertexFormat::Float32x3,
    offset: 0,
    shader_location: 0,
  },
  wgpu::VertexAttribute {
    format: wgpu::VertexFormat::Float32x3,
    offset: 12,
    shader_location: 1,
  },
  wgpu::VertexAttribute {
    format: wgpu::VertexFormat::Float32x2,
    offset: 24,
    shader_location: 2,
  },
  wgpu::VertexAttribute {
    format: wgpu::VertexFormat::Uint16x2,
    offset: 32,
    shader_location: 3,
  },
];

fn vertex_layout() -> wgpu::VertexBufferLayout<'static> {
  wgpu::VertexBufferLayout {
    array_stride: std::mem::size_of::<Vertex>() as wgpu::BufferAddress,
    step_mode: wgpu::VertexStepMode::Vertex,
    attributes: &ATTRIBUTES,
  }
}

/// 一套建好就不再改的管线。
///
/// 绑定**分两组**是有意的：0 号组只有全局 uniform（两条管线都要——阴影也要光的视图投影），
/// 1 号组是图集与阴影贴图（只有主管线用）。
///
/// 分开不是审美：阴影贴图在阴影 pass 里是**深度附件**，而「同一 pass 内一张纹理不能既是附件
/// 又是着色器资源」是硬约束。阴影 pass 因此只绑 0 号组，不碰 1 号组里那张自己正在写的图。
pub struct Pipelines {
  pub globals_layout: wgpu::BindGroupLayout,
  pub textures_layout: wgpu::BindGroupLayout,
  pub main: wgpu::RenderPipeline,
  pub shadow: wgpu::RenderPipeline,
}

/// 建两条管线与它们各自的 bind group 布局。
pub fn create_pipelines(device: &wgpu::Device, surface_format: wgpu::TextureFormat) -> Pipelines {
  let globals_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
    label: Some("globals"),
    entries: &[wgpu::BindGroupLayoutEntry {
      binding: 0,
      visibility: wgpu::ShaderStages::VERTEX_FRAGMENT,
      ty: wgpu::BindingType::Buffer {
        ty: wgpu::BufferBindingType::Uniform,
        has_dynamic_offset: false,
        min_binding_size: None,
      },
      count: None,
    }],
  });

  let textures_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
    label: Some("textures"),
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
      wgpu::BindGroupLayoutEntry {
        binding: 2,
        visibility: wgpu::ShaderStages::FRAGMENT,
        ty: wgpu::BindingType::Texture {
          sample_type: wgpu::TextureSampleType::Depth,
          view_dimension: wgpu::TextureViewDimension::D2,
          multisampled: false,
        },
        count: None,
      },
      wgpu::BindGroupLayoutEntry {
        binding: 3,
        visibility: wgpu::ShaderStages::FRAGMENT,
        ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Comparison),
        count: None,
      },
    ],
  });

  let main_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
    label: Some("main.layout"),
    bind_group_layouts: &[Some(&globals_layout), Some(&textures_layout)],
    immediate_size: 0,
  });

  let shadow_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
    label: Some("shadow.layout"),
    bind_group_layouts: &[Some(&globals_layout)],
    immediate_size: 0,
  });

  let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
    label: Some("shader.wgsl"),
    source: wgpu::ShaderSource::Wgsl(include_str!("shader.wgsl").into()),
  });

  let main = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
    label: Some("main"),
    layout: Some(&main_layout),
    vertex: wgpu::VertexState {
      module: &module,
      entry_point: Some("vs_main"),
      compilation_options: wgpu::PipelineCompilationOptions::default(),
      buffers: &[Some(vertex_layout())],
    },
    primitive: wgpu::PrimitiveState {
      topology: wgpu::PrimitiveTopology::TriangleList,
      front_face: wgpu::FrontFace::Ccw,
      // 区块是封闭的实心体：背面永远被正面挡住，剔掉省一半光栅化。
      cull_mode: Some(wgpu::Face::Back),
      ..wgpu::PrimitiveState::default()
    },
    depth_stencil: Some(wgpu::DepthStencilState {
      format: wgpu::TextureFormat::Depth32Float,
      depth_write_enabled: Some(true),
      depth_compare: Some(wgpu::CompareFunction::Less),
      stencil: wgpu::StencilState::default(),
      bias: wgpu::DepthBiasState::default(),
    }),
    multisample: wgpu::MultisampleState::default(),
    fragment: Some(wgpu::FragmentState {
      module: &module,
      entry_point: Some("fs_main"),
      compilation_options: wgpu::PipelineCompilationOptions::default(),
      targets: &[Some(wgpu::ColorTargetState {
        format: surface_format,
        blend: Some(wgpu::BlendState::REPLACE),
        write_mask: wgpu::ColorWrites::ALL,
      })],
    }),
    multiview_mask: None,
    cache: None,
  });

  let shadow = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
    label: Some("shadow"),
    layout: Some(&shadow_layout),
    vertex: wgpu::VertexState {
      module: &module,
      entry_point: Some("vs_shadow"),
      compilation_options: wgpu::PipelineCompilationOptions::default(),
      buffers: &[Some(vertex_layout())],
    },
    primitive: wgpu::PrimitiveState {
      topology: wgpu::PrimitiveTopology::TriangleList,
      front_face: wgpu::FrontFace::Ccw,
      // 只画背面：把正面留在深度里会让「贴地的一面」自己遮住自己。剩下的漏光交给深度偏移。
      cull_mode: Some(wgpu::Face::Front),
      ..wgpu::PrimitiveState::default()
    },
    depth_stencil: Some(wgpu::DepthStencilState {
      format: wgpu::TextureFormat::Depth32Float,
      depth_write_enabled: Some(true),
      depth_compare: Some(wgpu::CompareFunction::Less),
      stencil: wgpu::StencilState::default(),
      bias: wgpu::DepthBiasState {
        constant: 2,
        slope_scale: 2.0,
        clamp: 0.0,
      },
    }),
    multisample: wgpu::MultisampleState::default(),
    // 阴影 pass 只写深度，不需要片元阶段。
    fragment: None,
    multiview_mask: None,
    cache: None,
  });

  Pipelines {
    globals_layout,
    textures_layout,
    main,
    shadow,
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn globals_layout_matches_the_shader() {
    // uniform 缓冲的实际大小必须等于声明的字节数，且是 16 的整数倍（uniform 对齐要求）。
    assert_eq!(std::mem::size_of::<Globals>() as u64, Globals::SIZE);
    assert_eq!(Globals::SIZE % 16, 0);
    assert_eq!(bytemuck::bytes_of(&sample()).len() as u64, Globals::SIZE);
  }

  #[test]
  fn vertex_attributes_cover_the_stride_without_overlap() {
    let stride = std::mem::size_of::<Vertex>() as u64;
    assert_eq!(stride, 36, "顶点布局的偏移量是按这个步长手写的");
    for attribute in &ATTRIBUTES {
      assert!(attribute.offset < stride, "属性落在步长之外");
    }
    // 最后一个属性的起点 + 它的宽度，就是步长（没有尾部空洞）。
    assert_eq!(ATTRIBUTES[3].offset + 4, stride);
  }

  fn sample() -> Globals {
    Globals {
      view_proj: [[0.0; 4]; 4],
      light_view_proj: [[0.0; 4]; 4],
      camera_pos: [1.0, 2.0, 3.0, 1.0],
      light_dir: [0.0, -1.0, 0.0, 0.0],
      atlas: [8.0, 1.0, 16.0, 0.0],
      fog: [0.5, 0.7, 0.9, 220.0],
      params: [0.0, 4.0, 0.25, 0.0],
    }
  }
}
