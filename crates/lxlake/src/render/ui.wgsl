// lxlake 的自绘 UI 着色器：一条正交方片管线。
//
// 顶点里的 position **已经是 NDC**（CPU 侧按逻辑视口换算好），所以这里不碰任何 uniform：UI 的
// 画法就只有「往屏幕上贴一块带覆盖度的方片」，没有相机、没有光照、没有深度。
//
// 图集是单通道覆盖度：红通道就是 alpha。实心方片采样的是图集 0 号格那个纯白像素，于是底色与
// 字形共用一条管线、一份顶点布局。
//
// 颜色要从 sRGB 过一次线性：顶点里的颜色是 sRGB 0..255（与 `Quad::color` 的契约一致），而混合
// 发生在表面目标的**线性**空间里（sRGB 目标只在最后编码一次），不转换的话 UI 会整体偏亮。

struct VertexIn {
  // NDC（x 向右、y 向上），z 恒为 0。
  @location(0) position: vec2<f32>,
  @location(1) uv: vec2<f32>,
  // 已归一化的 sRGB + alpha。
  @location(2) color: vec4<f32>,
};

struct VertexOut {
  @builtin(position) clip_position: vec4<f32>,
  @location(0) uv: vec2<f32>,
  @location(1) color: vec4<f32>,
};

@group(0) @binding(0) var glyph_atlas: texture_2d<f32>;
@group(0) @binding(1) var glyph_sampler: sampler;

@vertex
fn vs_ui(in: VertexIn) -> VertexOut {
  var out: VertexOut;
  out.clip_position = vec4<f32>(in.position, 0.0, 1.0);
  out.uv = in.uv;
  out.color = in.color;
  return out;
}

fn srgb_to_linear(channel: vec3<f32>) -> vec3<f32> {
  let low = channel / 12.92;
  let high = pow((channel + 0.055) / 1.055, vec3<f32>(2.4));
  return select(high, low, channel <= vec3<f32>(0.04045));
}

@fragment
fn fs_ui(in: VertexOut) -> @location(0) vec4<f32> {
  let coverage = textureSample(glyph_atlas, glyph_sampler, in.uv).r;
  // 非预乘：alpha 由管线的混合状态去乘（见 `BlendState::ALPHA_BLENDING`）。
  return vec4<f32>(srgb_to_linear(in.color.rgb), in.color.a * coverage);
}