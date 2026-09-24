//! 方块图集：程序化生成的方格格子 + 采样器。
//!
//! M1 **不引图片解码依赖**（那会把 `image` / `png` 一整串拖进依赖图，而 M1 要的是「看得见
//! 形状」而不是「贴图好看」）：每格由应用给一个基色，这里按格索引做**确定性的逐像素扰动**，
//! 于是石头有石头的颗粒、草有草的杂色，看上去不像一块纯色塑料。
//!
//! 布局是固定的 `COLUMNS` 列网格，格边长 [`TILE_SIZE`] 像素。**0 号格保留**（对应
//! `BlockId::AIR`，永远采样不到），方块定义里的 tile 索引因此可以直接当格下标用。
//!
//! 采样用 `Nearest`：格子是像素风，线性过滤会把邻格的颜色糊进来（图集最常见的串色问题）。

/// 每格的边长（像素）。
pub const TILE_SIZE: u32 = 16;
/// 图集列数。行数按格子数向上取整。
pub const COLUMNS: u32 = 8;

/// 方块图集。
pub struct Atlas {
  view: wgpu::TextureView,
  sampler: wgpu::Sampler,
  cols: u32,
  rows: u32,
}

impl Atlas {
  /// 按每格基色建图集。`tiles[i]` 是 i 号格的基色（sRGB 0..255）。
  ///
  /// 格数不足一整行时右下的空格填黑——它们永远采样不到，只是把纹理补齐成矩形。
  pub fn new(device: &wgpu::Device, queue: &wgpu::Queue, tiles: &[[u8; 3]]) -> Self {
    let cols = COLUMNS;
    let rows = (tiles.len() as u32).div_ceil(cols).max(1);
    let width = cols * TILE_SIZE;
    let height = rows * TILE_SIZE;

    let mut pixels = vec![0u8; (width * height * 4) as usize];
    for (index, base) in tiles.iter().enumerate() {
      let col = index as u32 % cols;
      let row = index as u32 / cols;
      for y in 0..TILE_SIZE {
        for x in 0..TILE_SIZE {
          let shade = pixel_noise(index as u32, x, y);
          let offset = (((row * TILE_SIZE + y) * width) + col * TILE_SIZE + x) as usize * 4;
          pixels[offset] = shade_channel(base[0], shade);
          pixels[offset + 1] = shade_channel(base[1], shade);
          pixels[offset + 2] = shade_channel(base[2], shade);
          // 不透明：M1 的方块全是实心，alpha 只用来占位。
          pixels[offset + 3] = 255;
        }
      }
    }

    let size = wgpu::Extent3d {
      width,
      height,
      depth_or_array_layers: 1,
    };
    let texture = device.create_texture(&wgpu::TextureDescriptor {
      label: Some("atlas"),
      size,
      mip_level_count: 1,
      sample_count: 1,
      dimension: wgpu::TextureDimension::D2,
      // sRGB 格式：着色器采样到的就是线性值，光照算完由表面目标再编码回去。
      format: wgpu::TextureFormat::Rgba8UnormSrgb,
      usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
      view_formats: &[],
    });
    queue.write_texture(
      wgpu::TexelCopyTextureInfo {
        texture: &texture,
        mip_level: 0,
        origin: wgpu::Origin3d::ZERO,
        aspect: wgpu::TextureAspect::All,
      },
      &pixels,
      wgpu::TexelCopyBufferLayout {
        offset: 0,
        bytes_per_row: Some(width * 4),
        rows_per_image: Some(height),
      },
      size,
    );

    Self {
      view: texture.create_view(&wgpu::TextureViewDescriptor::default()),
      sampler: device.create_sampler(&wgpu::SamplerDescriptor {
        label: Some("atlas.sampler"),
        address_mode_u: wgpu::AddressMode::ClampToEdge,
        address_mode_v: wgpu::AddressMode::ClampToEdge,
        address_mode_w: wgpu::AddressMode::ClampToEdge,
        mag_filter: wgpu::FilterMode::Nearest,
        min_filter: wgpu::FilterMode::Nearest,
        ..wgpu::SamplerDescriptor::default()
      }),
      cols,
      rows,
    }
  }

  pub fn view(&self) -> &wgpu::TextureView {
    &self.view
  }

  pub fn sampler(&self) -> &wgpu::Sampler {
    &self.sampler
  }

  pub fn columns(&self) -> u32 {
    self.cols
  }

  pub fn rows(&self) -> u32 {
    self.rows
  }
}

/// 逐像素的明暗扰动，值域 0.82..1.18。
///
/// 是**哈希**而不是随机数：同 `(tile, x, y)` 必得同值，于是图集可重建、单测可断言，
/// 也不会因为跑两次就不一样。
fn pixel_noise(tile: u32, x: u32, y: u32) -> f32 {
  let mut h = tile
    .wrapping_mul(0x9E37_79B9)
    .wrapping_add(x.wrapping_mul(0x85EB_CA6B))
    .wrapping_add(y.wrapping_mul(0xC2B2_AE35));
  h ^= h >> 15;
  h = h.wrapping_mul(0x2545_F491);
  h ^= h >> 13;
  // 先映到千分位的 820..=1180，再一次性除以 1000：端点由整数步进保证精确落在 0.82 / 1.18 上，
  // 不会被浮点舍入顶出值域（0.82 + t * 0.36 在 t = 1 处会舍成 1.1800001）。
  let permille = 820 + (h & 0xFF) * 360 / 255;
  permille as f32 / 1000.0
}

/// 按扰动缩放一个通道并夹回 0..255。
fn shade_channel(channel: u8, shade: f32) -> u8 {
  (f32::from(channel) * shade).round().clamp(0.0, 255.0) as u8
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn noise_is_deterministic_and_spread() {
    assert_eq!(pixel_noise(1, 3, 4), pixel_noise(1, 3, 4), "同输入必得同值");
    // 同一格里不同像素、以及不同格之间，都不该取到同一个扰动（否则看不出质感）。
    let distinct: std::collections::HashSet<u32> = (0..TILE_SIZE)
      .flat_map(|y| (0..TILE_SIZE).map(move |x| pixel_noise(1, x, y).to_bits()))
      .collect();
    assert!(distinct.len() > TILE_SIZE as usize, "扰动要铺得开");
    assert_ne!(pixel_noise(1, 3, 4), pixel_noise(2, 3, 4), "格子之间要错开");
  }

  #[test]
  fn noise_stays_in_range() {
    for tile in 0..8 {
      for y in 0..TILE_SIZE {
        for x in 0..TILE_SIZE {
          let shade = pixel_noise(tile, x, y);
          assert!((0.82..=1.18).contains(&shade), "扰动越界：{shade}");
        }
      }
    }
  }

  #[test]
  fn shading_channels_saturate_instead_of_wrapping() {
    assert_eq!(shade_channel(255, 1.18), 255, "放大不该绕回小值");
    assert_eq!(shade_channel(0, 0.82), 0);
    assert_eq!(shade_channel(128, 1.0), 128);
  }
}
