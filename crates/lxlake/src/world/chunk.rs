//! 区块：32³ 的方块数组，以及给网格化作业用的**不可变邻域快照**。
//!
//! 线程模型的关键就在这个文件：区块**生成后不可变**，世界只持 `Arc<Chunk>`。网格化作业
//! 拿到的是快照（克隆 `Arc`，不是克隆数据），因此既不需要锁，也不可能改到世界。

use crate::world::block::BlockId;
use std::sync::Arc;

/// 区块边长（方块数），三个轴同长。
pub const CHUNK_SIZE: usize = 32;

/// 区块内的方块总数。
pub const CHUNK_VOLUME: usize = CHUNK_SIZE * CHUNK_SIZE * CHUNK_SIZE;

/// 区块坐标（以区块为单位，可为负）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ChunkPos {
  pub x: i32,
  pub y: i32,
  pub z: i32,
}

impl ChunkPos {
  pub const fn new(x: i32, y: i32, z: i32) -> Self {
    Self { x, y, z }
  }

  /// 世界方块坐标 → 区块坐标。用**欧几里得除法**，负坐标才不会偏一格。
  pub const fn from_world(world: [i32; 3]) -> Self {
    Self {
      x: world[0].div_euclid(CHUNK_SIZE as i32),
      y: world[1].div_euclid(CHUNK_SIZE as i32),
      z: world[2].div_euclid(CHUNK_SIZE as i32),
    }
  }

  /// 本区块的世界方块坐标原点。
  pub const fn origin(self) -> [i32; 3] {
    [
      self.x * CHUNK_SIZE as i32,
      self.y * CHUNK_SIZE as i32,
      self.z * CHUNK_SIZE as i32,
    ]
  }

  /// 按区块步进偏移。
  pub const fn offset(self, step: [i32; 3]) -> Self {
    Self {
      x: self.x + step[0],
      y: self.y + step[1],
      z: self.z + step[2],
    }
  }
}

/// 区块：`CHUNK_SIZE³` 个方块。
///
/// 生成阶段用 `&mut self` 的 [`Chunk::set`] 填数据；填完装进 [`Arc`] 就再没人能改它
/// （世界侧只拿 `Arc`，`Arc::get_mut` 拿不到独占就当不可变处理）。
#[derive(Debug, Clone)]
pub struct Chunk {
  pos: ChunkPos,
  blocks: Box<[BlockId]>,
  /// 非空气方块计数：为 0 时网格化直接返回空网格，跳过整块 32³ 的扫描。
  non_air: u32,
}

impl Chunk {
  /// 建一个全空气区块。
  pub fn new(pos: ChunkPos) -> Self {
    Self {
      pos,
      blocks: vec![BlockId::AIR; CHUNK_VOLUME].into_boxed_slice(),
      non_air: 0,
    }
  }

  pub fn pos(&self) -> ChunkPos {
    self.pos
  }

  /// 全空气区块。
  pub fn is_empty(&self) -> bool {
    self.non_air == 0
  }

  /// 非空气方块数。
  pub fn non_air(&self) -> u32 {
    self.non_air
  }

  /// 全部方块，按 [`Chunk::index`] 的布局。
  pub fn blocks(&self) -> &[BlockId] {
    &self.blocks
  }

  /// 区块内坐标 → 数组下标。布局：x 最快，其次 z，最后 y。
  pub const fn index(local: [usize; 3]) -> usize {
    (local[1] * CHUNK_SIZE + local[2]) * CHUNK_SIZE + local[0]
  }

  /// 世界方块坐标 → 区块内坐标。
  pub fn local_of(world: [i32; 3]) -> [usize; 3] {
    [
      world[0].rem_euclid(CHUNK_SIZE as i32) as usize,
      world[1].rem_euclid(CHUNK_SIZE as i32) as usize,
      world[2].rem_euclid(CHUNK_SIZE as i32) as usize,
    ]
  }

  /// 读一个方块。坐标必须落在本区块内——跨区块的读走 [`ChunkNeighborhood`]。
  pub fn get(&self, local: [usize; 3]) -> BlockId {
    self.blocks[Self::index(local)]
  }

  /// 写一个方块（**只在生成阶段**用；区块装进世界后不再可变）。
  pub fn set(&mut self, local: [usize; 3], id: BlockId) {
    let index = Self::index(local);
    let previous = self.blocks[index];
    if previous.is_air() && !id.is_air() {
      self.non_air += 1;
    } else if !previous.is_air() && id.is_air() {
      self.non_air -= 1;
    }
    self.blocks[index] = id;
  }
}

/// 区块邻域快照：中心区块 + 六个面邻居。
///
/// 这是网格化作业的**全部**输入，也是「工作线程不碰世界状态」的落实方式——作业拿到的
/// 是这里的 `Arc<Chunk>` 克隆，世界之后怎么增删区块都与它无关。
///
/// 邻居数组下标同 [`crate::world::block::Face::index`]；缺席 = 该方向尚未加载，按空气处理
/// （表现为那面照常长出几何体，加载完成后重新网格化即可修正）。
pub struct ChunkNeighborhood {
  center: Arc<Chunk>,
  neighbors: [Option<Arc<Chunk>>; 6],
}

impl ChunkNeighborhood {
  pub fn new(center: Arc<Chunk>, neighbors: [Option<Arc<Chunk>>; 6]) -> Self {
    Self { center, neighbors }
  }

  /// 中心区块。
  pub fn center(&self) -> &Arc<Chunk> {
    &self.center
  }

  /// 按**世界方块坐标**读；落在邻居区块上的读直接从快照解答，缺失的邻居按空气。
  pub fn block_at(&self, world: [i32; 3]) -> BlockId {
    let pos = ChunkPos::from_world(world);
    let chunk = std::iter::once(&self.center)
      .chain(self.neighbors.iter().flatten())
      .find(|chunk| chunk.pos() == pos);

    match chunk {
      Some(chunk) => chunk.get(Chunk::local_of(world)),
      None => BlockId::AIR,
    }
  }
}
