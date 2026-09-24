//! 浮岛地形生成。
//!
//! **纯函数**：只吃 `(seed, pos)` 与方块集，不读世界、不写世界——因此可以整块丢到工作线程
//! 上跑（见 `world` 模块的线程模型）。同 `(seed, pos, palette)` 必得同结果，卸载再加载
//! 也不丢形状。
//!
//! ## 形状怎么来的
//!
//! 一个**椭球遮罩 + 噪声抖动**的密度场：
//!
//! ```text
//!   density(p) = mask(p) * MASK_STRENGTH + fbm(p) * NOISE_AMPLITUDE
//!   mask(p)    = 1 − (dx/R)² − (dy/H)² − (dz/R)²      椭球内为正，表面为 0
//! ```
//!
//! `density > 0` 即实体。遮罩给岛形，噪声把表面揉毛。这样「岛」这一个概念只需要一组半径
//! 参数，不必手写任何高度图——换个 seed 就是一个新岛。
//!
//! 噪声是自写的整数哈希值噪声（见 [`value_noise`]），不引第三方——本模块要能在工作线程上
//! 任意复制跑，无全局状态、无内部可变性。

use crate::world::block::BlockPalette;
use crate::world::chunk::{CHUNK_SIZE, Chunk, ChunkPos};

/// 浮岛的纵向层区间（区块单位）。
///
/// 岛只占这几层，所以流式加载**不需要**纵向跟随相机——相机飞到天上也不必多加载一组区块。
pub const ISLAND_MIN_CHUNK_Y: i32 = -2;
pub const ISLAND_MAX_CHUNK_Y: i32 = 2;

/// 岛心（世界方块坐标）。
const ISLAND_CENTER: [f32; 3] = [0.0, 0.0, 0.0];
/// 岛的水平半径（方块）。
const ISLAND_RADIUS: f32 = 110.0;
/// 岛的纵向半高（方块）。比水平半径小得多——浮岛是扁的。
const ISLAND_HALF_HEIGHT: f32 = 46.0;
/// 遮罩强度：越大表面越平滑（噪声的相对影响越小）。
const MASK_STRENGTH: f32 = 4.0;
/// 噪声幅度：表面起伏的量级，与 [`MASK_STRENGTH`] 一起决定毛边厚度。
const NOISE_AMPLITUDE: f32 = 0.5;
/// 密度噪声频率（1 / 特征尺寸，特征尺寸约 24 方块）。
const NOISE_FREQUENCY: f32 = 1.0 / 24.0;
/// 噪声叠加的倍频层数。
const NOISE_OCTAVES: u32 = 4;
/// 草皮下面几格之内算土（再往下是石）。
const DIRT_DEPTH: usize = 4;
/// 密度场的纵向层数：区块本体 + 上方富余（材料选择要看上面 [`DIRT_DEPTH`] 格）。
const DENSITY_LAYERS: usize = CHUNK_SIZE + DIRT_DEPTH;

/// 浮岛生成器。
///
/// `Copy` 是给流式层用的：生成作业要按值带走 `(seed, palette)`，作业是 `'static` 的，借不到
/// 流式器里的字段。
#[derive(Debug, Clone, Copy)]
pub struct IslandGenerator {
  seed: u64,
  palette: BlockPalette,
}

impl IslandGenerator {
  pub fn new(seed: u64, palette: BlockPalette) -> Self {
    Self { seed, palette }
  }

  /// 固定种子。
  pub fn seed(&self) -> u64 {
    self.seed
  }

  /// 本生成器用的方块集。
  pub fn palette(&self) -> &BlockPalette {
    &self.palette
  }

  /// 生成一个区块。
  pub fn generate(&self, pos: ChunkPos) -> Chunk {
    let mut chunk = Chunk::new(pos);
    let origin = pos.origin();

    // 绝大多数区块整块在岛外——先用 AABB 判掉，免得白算 3.5 万次噪声。
    if !self.may_contain_blocks(origin) {
      return chunk;
    }

    let density = self.density_field(origin);
    for y in 0..CHUNK_SIZE {
      for z in 0..CHUNK_SIZE {
        for x in 0..CHUNK_SIZE {
          if density[density_index(x, y, z)] <= 0.0 {
            continue;
          }
          // 上面是空 → 这是暴露的表面，铺草；再往下几格是土；更深是石。
          let material = if density[density_index(x, y + 1, z)] <= 0.0 {
            self.palette.grass
          } else if density[density_index(x, y + DIRT_DEPTH, z)] <= 0.0 {
            self.palette.dirt
          } else {
            self.palette.stone
          };
          chunk.set([x, y, z], material);
        }
      }
    }

    chunk
  }

  /// 区块 AABB 是否可能碰到岛。**保守判定**：判「可能碰」就照常算，判「碰不到」才早退。
  fn may_contain_blocks(&self, origin: [i32; 3]) -> bool {
    let min = [origin[0] as f32, origin[1] as f32, origin[2] as f32];
    let max = [
      min[0] + CHUNK_SIZE as f32,
      min[1] + CHUNK_SIZE as f32,
      min[2] + CHUNK_SIZE as f32,
    ];
    // 遮罩在 AABB 上取到最大值的地方，就是离岛心最近的那个点。
    let closest = [
      ISLAND_CENTER[0].clamp(min[0], max[0]),
      ISLAND_CENTER[1].clamp(min[1], max[1]),
      ISLAND_CENTER[2].clamp(min[2], max[2]),
    ];
    ellipsoid_mask(closest) * MASK_STRENGTH + NOISE_AMPLITUDE > 0.0
  }

  /// 算区块本体 + 上方富余的密度场，布局同 [`Chunk::index`]（x 最快，其次 z，最后 y）。
  ///
  /// 富余那几层是给材料选择用的：判断某个实体方块「上面是不是空」需要读它头顶的密度，
  /// 顶层方块头顶正好落在富余里。一次算好存下来，比每个方块重算三遍噪声便宜得多。
  fn density_field(&self, origin: [i32; 3]) -> Vec<f32> {
    let mut field = vec![0.0f32; CHUNK_SIZE * CHUNK_SIZE * DENSITY_LAYERS];
    for y in 0..DENSITY_LAYERS {
      for z in 0..CHUNK_SIZE {
        for x in 0..CHUNK_SIZE {
          let world = [
            origin[0] + x as i32,
            origin[1] + y as i32,
            origin[2] + z as i32,
          ];
          field[density_index(x, y, z)] = self.density_at(world);
        }
      }
    }
    field
  }

  /// 一个世界方块坐标处的密度：> 0 为实体。
  fn density_at(&self, world: [i32; 3]) -> f32 {
    let point = [world[0] as f32, world[1] as f32, world[2] as f32];
    ellipsoid_mask(point) * MASK_STRENGTH
      + fbm(
        self.seed,
        [
          point[0] * NOISE_FREQUENCY,
          point[1] * NOISE_FREQUENCY,
          point[2] * NOISE_FREQUENCY,
        ],
      ) * NOISE_AMPLITUDE
  }
}

/// 密度场下标：与 [`Chunk::index`] 同布局，但纵向层数是 [`DENSITY_LAYERS`]。
const fn density_index(x: usize, y: usize, z: usize) -> usize {
  (y * CHUNK_SIZE + z) * CHUNK_SIZE + x
}

/// 椭球遮罩：岛内为正，表面为 0，岛外为负。
fn ellipsoid_mask(point: [f32; 3]) -> f32 {
  let dx = (point[0] - ISLAND_CENTER[0]) / ISLAND_RADIUS;
  let dy = (point[1] - ISLAND_CENTER[1]) / ISLAND_HALF_HEIGHT;
  let dz = (point[2] - ISLAND_CENTER[2]) / ISLAND_RADIUS;
  1.0 - dx * dx - dy * dy - dz * dz
}

/// 分形噪声（fbm）：多层 [`value_noise`] 叠加，结果在 `[-1, 1]`。
fn fbm(seed: u64, point: [f32; 3]) -> f32 {
  let mut frequency = 1.0f32;
  let mut amplitude = 1.0f32;
  let mut sum = 0.0f32;
  let mut norm = 0.0f32;

  for octave in 0..NOISE_OCTAVES {
    // 每层换一个种子，免得各层在格点上同相、叠出条纹。
    let layer_seed = seed ^ (u64::from(octave).wrapping_mul(0x9E37_79B9_7F4A_7C15));
    sum += value_noise(
      layer_seed,
      [
        point[0] * frequency,
        point[1] * frequency,
        point[2] * frequency,
      ],
    ) * amplitude;
    norm += amplitude;
    frequency *= 2.0;
    amplitude *= 0.5;
  }

  sum / norm
}

/// 三线性插值的值噪声：格点上取哈希值，格内用五次曲线过渡，结果在 `[-1, 1]`。
///
/// 用五次曲线（而非线性）是为了让一阶、二阶导在格点连续——否则区块边界上会出现肉眼可见
/// 的方向性接痕。
fn value_noise(seed: u64, point: [f32; 3]) -> f32 {
  let base = [point[0].floor(), point[1].floor(), point[2].floor()];
  let x0 = base[0] as i32;
  let y0 = base[1] as i32;
  let z0 = base[2] as i32;
  let u = smootherstep(point[0] - base[0]);
  let v = smootherstep(point[1] - base[1]);
  let w = smootherstep(point[2] - base[2]);

  let corner = |dx: i32, dy: i32, dz: i32| lattice(seed, x0 + dx, y0 + dy, z0 + dz);

  let x00 = lerp(corner(0, 0, 0), corner(1, 0, 0), u);
  let x10 = lerp(corner(0, 1, 0), corner(1, 1, 0), u);
  let x01 = lerp(corner(0, 0, 1), corner(1, 0, 1), u);
  let x11 = lerp(corner(0, 1, 1), corner(1, 1, 1), u);

  lerp(lerp(x00, x10, v), lerp(x01, x11, v), w)
}

/// 格点哈希值，映射到 `[-1, 1]`。
fn lattice(seed: u64, x: i32, y: i32, z: i32) -> f32 {
  let hash = seed
    ^ mix64(x as i64 as u64)
    ^ mix64(y as i64 as u64).rotate_left(21)
    ^ mix64(z as i64 as u64).rotate_left(42);
  // 取高 24 位 → 0..2^24，除以 2^23 得 0..2，减 1 得 -1..1。
  (mix64(hash) >> 40) as f32 / (1u32 << 23) as f32 - 1.0
}

/// splitmix64 的混合步：把有规律的下标打散成随机数。
fn mix64(mut value: u64) -> u64 {
  value = value.wrapping_add(0x9E37_79B9_7F4A_7C15);
  value = (value ^ (value >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
  value = (value ^ (value >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
  value ^ (value >> 31)
}

/// 五次平滑曲线（smootherstep）。
fn smootherstep(t: f32) -> f32 {
  t * t * t * (t * (t * 6.0 - 15.0) + 10.0)
}

fn lerp(a: f32, b: f32, t: f32) -> f32 {
  a + (b - a) * t
}

#[cfg(test)]
mod tests {
  use super::*;
  use crate::world::block::{BlockDef, BlockId, BlockRegistry, FaceTiles};

  /// 建表并解析出方块集——应用侧初始化就是这么干的。
  fn setup() -> (BlockRegistry, BlockPalette) {
    let mut registry = BlockRegistry::new();
    let mut register = |key: &'static str, tile: u16| {
      registry.register(BlockDef {
        key,
        solid: true,
        tiles: FaceTiles::uniform(tile),
      })
    };
    let stone = register("stone", 1);
    let dirt = register("dirt", 2);
    let grass = register("grass", 3);
    (registry, BlockPalette { stone, dirt, grass })
  }

  fn generator(seed: u64) -> IslandGenerator {
    let (_, palette) = setup();
    IslandGenerator::new(seed, palette)
  }

  #[test]
  fn same_seed_same_chunk() {
    let a = generator(7).generate(ChunkPos::new(0, 0, 0));
    let b = generator(7).generate(ChunkPos::new(0, 0, 0));
    assert_eq!(a.blocks(), b.blocks(), "同 seed 同 pos 必得同结果");
    assert!(a.non_air() > 0, "岛心区块不该是空的");
  }

  #[test]
  fn different_seed_different_island() {
    // 比表层区块：岛心区块整块埋在岛里（清一色石），换 seed 也看不出差别。
    let a = generator(1).generate(ChunkPos::new(0, 1, 0));
    let b = generator(2).generate(ChunkPos::new(0, 1, 0));
    assert_ne!(a.blocks(), b.blocks(), "换 seed 该换座岛");
  }

  #[test]
  fn far_chunks_are_empty() {
    let generator = generator(7);
    assert!(generator.generate(ChunkPos::new(64, 0, 0)).is_empty());
    assert!(generator.generate(ChunkPos::new(0, 0, -64)).is_empty());
    assert!(generator.generate(ChunkPos::new(-37, 0, 22)).is_empty());
  }

  #[test]
  fn island_stays_within_declared_layers() {
    let generator = generator(7);
    let non_air_at = |y: i32| {
      let mut total = 0;
      for x in 0..12 {
        for z in 0..12 {
          total += generator.generate(ChunkPos::new(x, y, z)).non_air();
        }
      }
      total
    };

    // 声明区间是**上界**，不必每层都长满——AABB 早退让余量层的代价接近零。
    let inside: u32 = (ISLAND_MIN_CHUNK_Y..=ISLAND_MAX_CHUNK_Y)
      .map(non_air_at)
      .sum();
    assert!(inside > 0, "声明区间里该有岛");
    assert_eq!(
      non_air_at(ISLAND_MIN_CHUNK_Y - 1),
      0,
      "声明层之下不该有方块"
    );
    assert_eq!(
      non_air_at(ISLAND_MAX_CHUNK_Y + 1),
      0,
      "声明层之上不该有方块"
    );
  }

  #[test]
  fn surface_is_grass_and_core_is_stone() {
    let (registry, palette) = setup();
    let generator = IslandGenerator::new(7, palette);

    let count = |pos: ChunkPos, wanted: BlockId| {
      let chunk = generator.generate(pos);
      chunk.blocks().iter().filter(|id| **id == wanted).count()
    };

    // 岛心区块整块埋在岛里 → 只有石 / 土；顶上那层区块才暴露表面 → 有草。
    assert!(
      count(ChunkPos::new(0, 0, 0), palette.stone) > 0,
      "岛心该是石"
    );
    assert!(
      count(ChunkPos::new(0, 1, 0), palette.grass) > 0,
      "表面该铺草"
    );
    assert_eq!(
      registry.get(palette.grass).key,
      "grass",
      "方块集里的 id 要能查回定义"
    );
  }
}
