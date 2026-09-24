//! 世界：体素区块的容器 + 方块定义表，外加两类**查询**（射线、碰撞）。
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
//! - **世界只被主线程写**：区块的增删、方块的改动都发生在主线程
//! - **区块仍是不可变的**：装进世界的是 `Arc<Chunk>`，作业拿到的是克隆句柄，既不加锁也
//!   改不到世界（见 `chunk::ChunkNeighborhood`）。M2 的改方块走 **copy-on-write**——换一份
//!   新快照，而不是改旧的那份（见 [`World::set_block`]）
//! - **结果只在帧边界收**：作业完成时经 `Wakeup` 打断主循环的等待，但结果由主线程 poll
//!   （见 `runtime::jobs`）
//!
//! 几何体怎么从区块里长出来是 `meshing` 的事，本层只负责「有哪些方块」，以及「从哪到哪被
//! 什么挡着」——后者是 [`raycast`] 与 [`collision`] 两件纯查询，它们自己不改世界。

pub mod block;
pub mod chunk;
pub mod collision;
pub mod raycast;
pub mod stream;
pub mod terrain;

pub use block::{BlockDef, BlockId, BlockPalette, BlockRegistry, Face, FaceTiles};
pub use chunk::{CHUNK_SIZE, CHUNK_VOLUME, Chunk, ChunkNeighborhood, ChunkPos};
pub use collision::Aabb;
pub use raycast::RayHit;
pub use stream::{ChunkStreamer, StreamBounds, StreamOutput};
pub use terrain::IslandGenerator;

use crate::core::command::Command;
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

  /// 该世界位置是不是**实体**方块（缺失区块按空气 = 非实体）。
  ///
  /// 射线与碰撞的实心判定都走这里——「哪些方块挡路」是一个口径，不该有第二份。
  pub fn is_solid(&self, world: [i32; 3]) -> bool {
    let id = self.block_at(world);
    !id.is_air() && self.registry.get(id).solid
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

  /// 改一个世界方块，返回**几何可能因此变化的区块**（本体 + 落在边界 1 格内的面邻居）。
  ///
  /// 走 **copy-on-write**：区块仍是 `Arc<Chunk>`，改动时 clone 出新的一份替换旧的。正在跑的
  /// 网格化作业手里还握着旧快照，它会安全跑完，只是结果作废——调用方拿返回的区块去标脏，
  /// 脏标记会再排一次网格化（见 [`ChunkStreamer::mark_dirty`]）。代价是改一个方块复制 32KB，
  /// 放在「玩家点一下」这个频率上可以忽略；将来要高频批量改（爆炸、流体），再引入 16³ 子区块
  /// 细分——M2 不做。
  ///
  /// 边界那一条是接缝正确性的关键：改动点落在区块边界 1 格内时，**该轴上的面邻居也要重算**
  /// ——那一面是否可见变了，只重算自己会在接缝处留下错面。角上的邻居不必（它与改动点只擦边，
  /// 不共享面，快照里也没有这块方块）。
  ///
  /// 返回空 = 什么都没发生：目标区块没加载（改空气没有意义，而且那片会随流式生成覆盖掉），
  /// 或者本来就是这个方块（不必换快照，也免得白标一次脏）。
  pub fn set_block(&mut self, world: [i32; 3], id: BlockId) -> Vec<ChunkPos> {
    let pos = ChunkPos::from_world(world);
    let Some(current) = self.chunks.get(&pos) else {
      return Vec::new();
    };

    let local = Chunk::local_of(world);
    if current.get(local) == id {
      return Vec::new();
    }

    // 换一份新快照，而不是改旧的那份——跑着的作业读的是旧快照，它不该看见半个改动。
    let mut next = current.as_ref().clone();
    next.set(local, id);
    self.chunks.insert(pos, Arc::new(next));

    let mut touched = vec![pos];
    for axis in 0..3 {
      let step = match local[axis] {
        0 => -1,
        last if last == CHUNK_SIZE - 1 => 1,
        _ => continue,
      };
      let mut offset = [0; 3];
      offset[axis] = step;
      touched.push(pos.offset(offset));
    }
    touched
  }

  /// 命令执行器：把一条命令落到世界上，返回几何可能因此变化的区块。
  ///
  /// 校验放在这里而不是交给生成方：命令可以来自任何地方（HUD、脚本、回放、将来的网络），
  /// 越界与「本来就那样」的命令都安静作废（返回空），不制造临时状态。
  pub fn apply(&mut self, command: &Command) -> Vec<ChunkPos> {
    match *command {
      // 本来就是空气：没什么可挖的。
      Command::Break { pos } => {
        if self.block_at(pos).is_air() {
          Vec::new()
        } else {
          self.set_block(pos, BlockId::AIR)
        }
      }
      // 只往空气里放：往实体方块里塞一块等于替换地形，那是另一条命令的事。
      Command::Place { pos, block } => {
        if self.block_at(pos).is_air() {
          self.set_block(pos, block)
        } else {
          Vec::new()
        }
      }
    }
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

/// 单测共用的世界搭建：`raycast` / `collision` 都要一块有地形的世界，别各搭各的。
#[cfg(test)]
pub(crate) mod test_support {
  use super::*;

  /// 建一个只注册了 `stone` 的空世界，返回世界与 stone 的 id。
  pub fn world() -> (World, BlockId) {
    let mut registry = BlockRegistry::new();
    let stone = registry.register(BlockDef {
      key: "stone",
      solid: true,
      tiles: FaceTiles::uniform(1),
    });
    (World::new(registry), stone)
  }

  /// 往世界里放一块区块：`y = layer`（区块内坐标）那层铺满 `id`，其余是空气。
  ///
  /// 直接填区块而不是走 `set_block`：这是**生成期**的形状（生成器就是这么干的），也省掉
  /// 一层层 copy-on-write 的复制。
  pub fn layered_chunk(world: &mut World, pos: ChunkPos, layer: usize, id: BlockId) {
    let mut chunk = Chunk::new(pos);
    for z in 0..CHUNK_SIZE {
      for x in 0..CHUNK_SIZE {
        chunk.set([x, layer, z], id);
      }
    }
    world.insert(Arc::new(chunk));
  }

  /// 铺一块 `y = 0` 是实心地面的区块（原点那块），返回世界与 stone 的 id。
  pub fn floor_world() -> (World, BlockId) {
    let (mut world, stone) = world();
    layered_chunk(&mut world, ChunkPos::new(0, 0, 0), 0, stone);
    (world, stone)
  }
}

#[cfg(test)]
mod tests {
  use super::test_support;
  use super::*;

  #[test]
  fn set_block_swaps_the_snapshot_and_reports_the_chunk() {
    let (mut world, stone) = test_support::floor_world();
    let pos = ChunkPos::new(0, 0, 0);
    // 作业手里那份快照：改动之后它必须还是旧的。
    let in_flight = Arc::clone(world.chunk(pos).expect("区块该在"));

    let touched = world.set_block([3, 5, 7], stone);

    assert_eq!(touched, vec![pos]);
    assert_eq!(world.block_at([3, 5, 7]), stone);
    assert_eq!(world.chunk_count(), 1, "换快照不该多出一块区块");
    assert_eq!(
      in_flight.get(Chunk::local_of([3, 5, 7])),
      BlockId::AIR,
      "copy-on-write：跑着的作业读旧快照，看不见这次改动"
    );
  }

  #[test]
  fn set_block_outside_a_loaded_chunk_does_nothing() {
    let (mut world, stone) = test_support::floor_world();

    let touched = world.set_block([100, 5, 7], stone);

    assert!(touched.is_empty());
    assert_eq!(
      world.block_at([100, 5, 7]),
      BlockId::AIR,
      "不会凭空造出区块"
    );
    assert_eq!(world.chunk_count(), 1);
  }

  #[test]
  fn set_block_to_the_same_value_reports_nothing() {
    let (mut world, stone) = test_support::floor_world();

    assert!(
      world.set_block([3, 0, 7], stone).is_empty(),
      "那格本来就是石头：不必换快照，也不必标脏"
    );
  }

  #[test]
  fn editing_on_a_chunk_border_also_flags_the_face_neighbour() {
    let (mut world, stone) = test_support::floor_world();
    let pos = ChunkPos::new(0, 0, 0);

    // 区块内 (0, 5, 7)：x 贴着负向边界 → 邻居 (x−1) 的那一面是否可见变了。
    let touched = world.set_block([0, 5, 7], stone);
    assert_eq!(touched, vec![pos, pos.offset([-1, 0, 0])]);

    // 区块内 (31, 5, 7)：x 贴正向边界。
    let touched = world.set_block([CHUNK_SIZE as i32 - 1, 5, 7], stone);
    assert_eq!(touched, vec![pos, pos.offset([1, 0, 0])]);

    // 区块正中：只有它自己。
    let touched = world.set_block([15, 5, 7], stone);
    assert_eq!(touched, vec![pos]);

    // 角上：三个轴各带一个面邻居（对角邻居不共享面，不必重算）。
    let touched = world.set_block([0, CHUNK_SIZE as i32 - 1, 0], stone);
    assert_eq!(
      touched,
      vec![
        pos,
        pos.offset([-1, 0, 0]),
        pos.offset([0, 1, 0]),
        pos.offset([0, 0, -1]),
      ]
    );
  }

  #[test]
  fn break_and_place_validate_their_target() {
    let (mut world, stone) = test_support::floor_world();

    // 挖空气：什么也不发生。
    assert!(world.apply(&Command::Break { pos: [3, 5, 7] }).is_empty());
    // 挖实体：挖掉。
    assert!(!world.apply(&Command::Break { pos: [3, 0, 7] }).is_empty());
    assert_eq!(world.block_at([3, 0, 7]), BlockId::AIR);

    // 往实体里放：拒绝（那是替换地形，另一条命令的事）。
    assert!(
      world
        .apply(&Command::Place {
          pos: [3, 0, 8],
          block: stone,
        })
        .is_empty()
    );
    assert_eq!(world.block_at([3, 0, 8]), stone);

    // 往空气里放：生效。
    assert!(
      !world
        .apply(&Command::Place {
          pos: [3, 5, 8],
          block: stone,
        })
        .is_empty()
    );
    assert_eq!(world.block_at([3, 5, 8]), stone);
  }

  #[test]
  fn is_solid_reads_the_registry_for_solidity() {
    let (mut world, stone) = test_support::floor_world();

    assert!(world.is_solid([0, 0, 0]));
    assert!(!world.is_solid([0, 5, 0]), "空气不是实体");
    assert!(!world.is_solid([100, 0, 0]), "区块缺失按空气");

    world.set_block([5, 5, 5], stone);
    assert!(world.is_solid([5, 5, 5]));
  }
}
