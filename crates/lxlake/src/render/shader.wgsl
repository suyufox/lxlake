// lxlake 的 M1 着色器：一个顶点布局、两条管线（主光路 + 阴影）。
//
// 顶点里的 position **已经是世界坐标**（CPU 上传时加了区块原点），所以这里没有任何逐区块
// 偏移——全局一份 uniform 就够，绘制循环里也不必切 bind group。
//
// 色彩管线：图集是 sRGB 纹理（采样得到线性值）→ 线性空间里做光照 / 雾 → ACES 近似 tone map
// → 输出；最后由 sRGB 表面目标做编码。雾的混合放在 tone map 之后：这样地平线能跟清屏色
// 严丝合缝地接上（清屏色不经过 tone map）。

struct Globals {
  view_proj: mat4x4<f32>,
  light_view_proj: mat4x4<f32>,
  // xyz 世界坐标；w 只用来占满 16 字节对齐。
  camera_pos: vec4<f32>,
  // xyz 方向光**行进方向**（从光源指向场景），已归一化。
  light_dir: vec4<f32>,
  // x 列数、y 行数、z 格边长（像素）、w 保留。
  atlas: vec4<f32>,
  // rgb 雾色（同时是清屏色）、w 雾的结束距离。
  fog: vec4<f32>,
  // x 是否开 NPR、y ramp 阶数、z 边缘光强度、w 保留。
  params: vec4<f32>,
};

// 0 号组只有全局 uniform：阴影 pass 也要光的视图投影，但绝不能碰 1 号组里那张自己正在写的
// 阴影贴图（同一 pass 内一张纹理不能既是深度附件又是着色器资源）。
@group(0) @binding(0) var<uniform> globals: Globals;

// 1 号组只给主管线。
@group(1) @binding(0) var atlas_texture: texture_2d<f32>;
@group(1) @binding(1) var atlas_sampler: sampler;
@group(1) @binding(2) var shadow_texture: texture_depth_2d;
@group(1) @binding(3) var shadow_sampler: sampler_comparison;

struct VertexIn {
  @location(0) position: vec3<f32>,
  @location(1) normal: vec3<f32>,
  @location(2) uv: vec2<f32>,
  @location(3) tile: vec2<u32>,
};

struct VertexOut {
  @builtin(position) clip_position: vec4<f32>,
  @location(0) world_position: vec3<f32>,
  @location(1) normal: vec3<f32>,
  @location(2) uv: vec2<f32>,
  @location(3) @interpolate(flat) tile: u32,
};

// 接收端沿法线抬起一点距离再查阴影：斜射的面上自阴影最容易从这里冒出来。
const SHADOW_NORMAL_OFFSET: f32 = 0.06;
// 边缘光的固定形状参数：指数越大越细。
const RIM_EXPONENT: f32 = 3.0;

@vertex
fn vs_main(vertex: VertexIn) -> VertexOut {
  var out: VertexOut;
  out.clip_position = globals.view_proj * vec4<f32>(vertex.position, 1.0);
  out.world_position = vertex.position;
  out.normal = vertex.normal;
  out.uv = vertex.uv;
  out.tile = vertex.tile.x;
  return out;
}

// 阴影 pass：只写深度，连片元阶段都没有。
@vertex
fn vs_shadow(vertex: VertexIn) -> @builtin(position) vec4<f32> {
  return globals.light_view_proj * vec4<f32>(vertex.position, 1.0);
}

// 阴影系数：1 = 全亮，0 = 全遮。
//
// 用 textureSampleCompareLevel 而不是 textureSampleCompare：前者不看导数，因此可以在
// 非一致控制流（这里的提前 return）里调用，PCF 的 3x3 循环也不受 quad 约束。
fn shadow_factor(world_position: vec3<f32>, normal: vec3<f32>) -> f32 {
  let clip = globals.light_view_proj * vec4<f32>(world_position + normal * SHADOW_NORMAL_OFFSET, 1.0);
  let ndc = clip.xyz / clip.w;
  // 落在光源视锥之外的一律当全亮：那里本来就没被阴影贴图覆盖。
  if (any(abs(ndc.xy) > vec2<f32>(1.0, 1.0)) || ndc.z < 0.0 || ndc.z > 1.0) {
    return 1.0;
  }

  // 纹理 y 与 NDC y 方向相反（DirectX / WebGPU 约定），所以要翻一下。
  let uv = vec2<f32>(ndc.x * 0.5 + 0.5, 0.5 - ndc.y * 0.5);
  let texel = 1.0 / vec2<f32>(textureDimensions(shadow_texture));

  var sum = 0.0;
  for (var y = -1; y <= 1; y += 1) {
    for (var x = -1; x <= 1; x += 1) {
      let offset = vec2<f32>(f32(x), f32(y)) * texel;
      sum += textureSampleCompareLevel(shadow_texture, shadow_sampler, uv + offset, ndc.z);
    }
  }
  return sum / 9.0;
}

// ACES 的解析近似：只是把高光压住，不追求色彩管理精度。
fn tonemap(color: vec3<f32>) -> vec3<f32> {
  let a = 2.51;
  let b = 0.03;
  let c = 2.43;
  let d = 0.59;
  let e = 0.14;
  return saturate((color * (a * color + b)) / (color * (c * color + d) + e));
}

@fragment
fn fs_main(in: VertexOut) -> @location(0) vec4<f32> {
  // 图集取样：uv 的单位是方块（合并面取 0..N），fract 一下就是格内坐标；再落到像素中心
  // 避免踩到邻格的边界。
  let cols = globals.atlas.x;
  let tile_px = globals.atlas.z;
  let col = in.tile % u32(cols);
  let row = in.tile / u32(cols);
  let in_tile = floor(fract(in.uv) * tile_px) + 0.5;
  let texel = vec2<f32>(f32(col) * tile_px + in_tile.x, f32(row) * tile_px + in_tile.y);
  let atlas_size = vec2<f32>(textureDimensions(atlas_texture));
  let albedo = textureSample(atlas_texture, atlas_sampler, texel / atlas_size).rgb;

  let normal = normalize(in.normal);
  let to_light = max(dot(normal, -globals.light_dir.xyz), 0.0);
  let shadow = shadow_factor(in.world_position, normal);

  // 环境项用半球近似：朝上的面更亮、朝下的更暗，省掉一套 GI。
  let ambient = mix(0.28, 0.62, normal.y * 0.5 + 0.5);
  var key = to_light * shadow;

  if (globals.params.x > 0.5) {
    // NPR：把 lambert 量化成几阶，再补一道边缘光（roadmap 里「只留开关」的那条）。
    let steps = max(globals.params.y, 2.0);
    key = floor(key * steps + 0.5) / steps;
  }

  var color = albedo * (ambient + key * 0.95);

  if (globals.params.x > 0.5) {
    let view_dir = normalize(globals.camera_pos.xyz - in.world_position);
    let rim = pow(1.0 - saturate(dot(normal, view_dir)), RIM_EXPONENT);
    color += rim * globals.params.z * key;
  }

  color = tonemap(color);

  // 雾在 tone map 之后混合，于是远处的东西精确地融进清屏色。
  let distance = length(in.world_position - globals.camera_pos.xyz);
  let fog_mix = smoothstep(globals.fog.w * 0.35, globals.fog.w, distance);
  color = mix(color, globals.fog.rgb, fog_mix);

  return vec4<f32>(color, 1.0);
}