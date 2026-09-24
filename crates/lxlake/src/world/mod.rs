//! 世界：体素区块的只读模型（M1 只读，改方块留给 M2）。
//!
//! ## 线程模型
//!
//! 三条线，边界都在 `Arc<Chunk>` 上：
//!
//! ```text
//!   主线程                                 worker 线程
//!   ──────                                 ──────────
//!   stream.update()  ← 帧边界唯一入口
//!     ├─ 提交生成作业(seed, pos) ─────────→ 纯函数：噪声 → Chunk
//!     ├─ 提交网格化作业(邻域快照) ────────→ 纯函数：greedy meshing
//!     ├─ poll 句柄，收 Chunk / ChunkMesh      （只读输入，无锁，不碰世界）
//!     └─ 写 world（插入 / 卸载）+ 把网格交给渲染侧
//! ```
//!
//! - **世界只被主线程写**：区块的增删都发生在 `stream` 的帧边界收割里
//! - **区块生成后不可变**：装进世界的是 `Arc<Chunk>`，作业拿到的是克隆句柄，既不加锁也
//!   改不到世界（见 `chunk::ChunkNeighborhood`）
//! - **结果只在帧边界收**：作业完成时经 `Wakeup` 打断主循环的等待，但结果由主线程 poll
//!   （见 `runtime::jobs`）
//!
//! 几何体怎么从区块里长出来是 `meshing` 的事，本层只负责「有哪些方块」。

pub mod block;
pub mod chunk;
pub mod stream;
pub mod terrain;

pub use block::{BlockDef, BlockId, BlockPalette, BlockRegistry, Face, FaceTiles};
pub use chunk::{CHUNK_SIZE, CHUNK_VOLUME, Chunk, ChunkNeighborhood, ChunkPos};
pub use stream::{ChunkStreamer, StreamBounds, StreamOutput};
pub use terrain::IslandGenerator;

use std::collections::HashMap;
use std::sync::Arc;

/// 世界：区块容器 + 方块定义表。
pub struct World {
  registry: Arc<BlockRegistry>,
  chunks: HashMap<ChunkPos, Arc<Chunk>>,
}

impl World {
  pub fn new(registry: BlockRegistry) -> Self {
    Self {
      registry: Arc::new(registry),
      chunks: HashMap::new(),
    }
  }

  /// 方块定义表。
  pub fn registry(&self) -> &BlockRegistry {
    &self.registry
  }

  /// 方块定义表的共享句柄。
  ///
  /// 网格化作业要把它当**只读输入**带走，而作业是 `'static` 的，借不到世界里的东西——
  /// 于是注册表放 `Arc` 里发出去。注册表建好就不改，多一份句柄没有一致性代价。
  pub fn registry_handle(&self) -> Arc<BlockRegistry> {
    Arc::clone(&self.registry)
  }

  /// 当前持有的区块数。
  pub fn chunk_count(&self) -> usize {
    self.chunks.len()
  }

  /// 取一个区块（不可变）。
  pub fn chunk(&self, pos: ChunkPos) -> Option<&Arc<Chunk>> {
    self.chunks.get(&pos)
  }

  /// 读一个世界方块；区块缺失按空气。
  pub fn block_at(&self, world: [i32; 3]) -> BlockId {
    let pos = ChunkPos::from_world(world);
    match self.chunks.get(&pos) {
      Some(chunk) => chunk.get(Chunk::local_of(world)),
      None => BlockId::AIR,
    }
  }

  /// 组装中心区块的邻域快照，给网格化作业当输入。
  ///
  /// 六个邻居按 [`Face::index`] 排；缺失 = `None`（按空气处理）。
  pub fn neighborhood(&self, center: ChunkPos) -> Option<ChunkNeighborhood> {
    let center_chunk = self.chunks.get(&center)?;
    let neighbors = std::array::from_fn(|face_index| {
      let step = Face::ALL[face_index].step();
      self.chunks.get(&center.offset(step)).cloned()
    });
    Some(ChunkNeighborhood::new(Arc::clone(center_chunk), neighbors))
  }

  /// 插入一个区块。**只由 `stream` 在帧边界调用**——世界不对外开放写口。
  pub(crate) fn insert(&mut self, chunk: Arc<Chunk>) {
    self.chunks.insert(chunk.pos(), chunk);
  }

  /// 卸载一个区块。同上，只由 `stream` 调用。
  pub(crate) fn remove(&mut self, pos: ChunkPos) {
    self.chunks.remove(&pos);
  }
}
