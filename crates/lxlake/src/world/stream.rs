//! 区块流式：视距内异步生成 / 网格化，出视距卸载，**被改过的区块回头重算**。
//!
//! **本层是世界唯一的写者**（区块的增删），且只在帧边界写（`update` 的调用点）。一个区块的
//! 生命周期：
//!
//! ```text
//!   申请 ──→ Generating ──→ PendingMesh ──→ Meshing ──→ Ready ──→ 卸载
//!           生成作业在跑    等邻居就位      网格化作业在跑   世界+GPU 都就位
//!                                              ↑            │
//!                                              └──── 标脏 ───┘
//! ```
//!
//! - 网格化前要确认**此刻视距内的六个邻居都已进世界**：这样每块只网格化一次，不必等邻居
//!   补齐再回头重算。视距外的邻居按空气处理——那边本就没有地形，越界那面照常长出来
//!   （岛顶、岛底的收口面全靠它）；相机移过去后真实邻居会把它挡成内面，看不见
//! - **M1 是单向流，M2 给槽位加了「脏」状态**：方块被改 → 标脏 → 下一轮重新提交网格化作业
//!   （吃的是**此刻**的邻域快照）→ 收割后走 `Renderer::upload` 覆盖上传。作业在跑的时候又被
//!   改，那份结果按「旧了」处理：再排一次（见 `Slot::reap`）
//! - 卸载是**同步**的：摘除区块立刻发生，作业结果回来的话在主线程丢弃（顺带 cancel）
//! - 网格化结果经 [`StreamOutput`] 交给渲染侧：本层不认识 GPU

use crate::meshing::ChunkMesh;
use crate::runtime::{JobContext, JobHandle, JobPool};
use crate::world::World;
use crate::world::block::Face;
use crate::world::chunk::{Chunk, ChunkPos};
use crate::world::terrain::IslandGenerator;
use std::collections::HashMap;
use std::sync::Arc;

/// 每帧最多**新申请**的区块数。
///
/// 一格区块要算三万多次噪声，一帧把整片视距全申请掉，只会在 worker 端堆出一条长尾。按距离
/// 排完序再切片推进，近处先出画面，远处随后补上——总的完成时间不变，但首帧不再难看。
const SPAWN_BUDGET: usize = 32;

/// 要加载的区块范围。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StreamBounds {
  /// 水平半径（区块数）：以焦点为中心的正方形边长是 `2 * radius + 1`。
  pub radius: i32,
  /// 纵向层区间（含两端）。
  pub min_y: i32,
  pub max_y: i32,
}

impl StreamBounds {
  /// 本范围覆盖的区块总数。
  pub fn chunk_count(&self) -> usize {
    let side = (2 * self.radius + 1) as usize;
    let layers = (self.max_y - self.min_y + 1).max(0) as usize;
    side * side * layers
  }

  /// 焦点区块是否落在范围内（含纵向）。
  pub fn contains(&self, focus: ChunkPos, pos: ChunkPos) -> bool {
    let in_plane = (pos.x - focus.x).abs() <= self.radius && (pos.z - focus.z).abs() <= self.radius;
    in_plane && pos.y >= self.min_y && pos.y <= self.max_y
  }
}

/// 本帧的流式产出：帧边界上交给渲染侧的东西。
#[derive(Debug, Default)]
pub struct StreamOutput {
  /// 本帧待上传 GPU 的区块网格：新网格化的，以及**重算过的**（渲染侧按 pos 覆盖上传）。
  ///
  /// 全空气的区块也会出现在这里（空网格）：`Renderer::upload` 对空网格就是回收资源，方块被
  /// 挖光的区块正需要这一下。
  pub meshed: Vec<ChunkMesh>,
  /// 本帧已卸载的区块：渲染侧要丢掉对应的 GPU 资源。
  pub unloaded: Vec<ChunkPos>,
}

/// 区块流式器：把「相机在哪」翻译成「该有哪些区块」。
///
/// 池不归它持有——池是共享服务（作业线程数是全局口径），由应用持有并传进来。
pub struct ChunkStreamer {
  bounds: StreamBounds,
  generator: IslandGenerator,
  slots: HashMap<ChunkPos, Slot>,
}

/// 一个区块在流水线上的位置。
struct Slot {
  state: SlotState,
  /// 方块被改过：手里的几何旧了，等作业腾出手来重算。
  ///
  /// 提交网格化作业时清掉、结果回来时若又被改过就重新置上——「脏」表达的是「此刻的几何不是
  /// 世界的样子」，与作业在不在跑无关。
  dirty: bool,
}

impl Slot {
  fn new(state: SlotState) -> Self {
    Self {
      state,
      dirty: false,
    }
  }
}

/// 槽位的状态。
enum SlotState {
  /// 生成作业在跑。
  Generating(JobHandle<Chunk>),
  /// 区块已进世界，但邻居还没齐，暂不网格化。
  PendingMesh,
  /// 网格化作业在跑。
  Meshing(JobHandle<ChunkMesh>),
  /// 世界与渲染两侧都已就位。
  Ready,
}

impl Slot {
  /// 收割一个已完成的作业；还没完成就什么都不做。
  fn reap(&mut self, world: &mut World, output: &mut StreamOutput) {
    let next = match &mut self.state {
      SlotState::Generating(handle) => match handle.poll() {
        Some(chunk) => {
          world.insert(Arc::new(chunk));
          // 进世界了，但能不能网格化还得看邻居（见 `neighbors_ready`）。
          Some(SlotState::PendingMesh)
        }
        None => None,
      },
      SlotState::Meshing(handle) => match handle.poll() {
        Some(mesh) => {
          // 空网格也交出去：`Renderer::upload` 对空网格就是回收。方块被挖光的区块得把旧的
          // 那份几何撤掉——M1 的「空网格不必交给渲染侧」到 M2 就不成立了。
          output.meshed.push(mesh);
          // 作业跑着的时候又被改过：手里这份结果已经旧了，退回待网格化再排一次。
          Some(if self.dirty {
            SlotState::PendingMesh
          } else {
            SlotState::Ready
          })
        }
        None => None,
      },
      SlotState::PendingMesh | SlotState::Ready => None,
    };

    if let Some(next) = next {
      self.state = next;
    }
  }
}

impl ChunkStreamer {
  pub fn new(bounds: StreamBounds, generator: IslandGenerator) -> Self {
    Self {
      bounds,
      generator,
      slots: HashMap::new(),
    }
  }

  pub fn bounds(&self) -> StreamBounds {
    self.bounds
  }

  pub fn generator(&self) -> &IslandGenerator {
    &self.generator
  }

  /// 在流水线上的区块数（含正在生成 / 网格化的）。
  pub fn tracked(&self) -> usize {
    self.slots.len()
  }

  /// 标脏：这些区块的方块被改过，几何要重算。
  ///
  /// 只认流水线上的区块——视距外的本就该卸载，改了也留不住。真正的重算发生在下一轮 `update`
  /// 的 [`ChunkStreamer::mesh_ready`]：作业要重新吃一遍**此刻**的邻域快照。
  pub fn mark_dirty(&mut self, positions: &[ChunkPos]) {
    for pos in positions {
      if let Some(slot) = self.slots.get_mut(pos) {
        slot.dirty = true;
      }
    }
  }

  /// 帧边界调用一次：补申请 → 收割结果 → 卸载出视距的。
  pub fn update(
    &mut self,
    world: &mut World,
    pool: &JobPool,
    focus: ChunkPos,
    output: &mut StreamOutput,
  ) {
    output.meshed.clear();
    output.unloaded.clear();

    // 顺序有讲究：先收结果（生成好的装进世界），再卸载，再申请，最后网格化——这样本帧刚
    // 进世界的区块本帧就能进网格化队列，只要它的邻居已经在位；本帧刚标脏的区块同理。
    self.reap(world, output);
    self.unload(world, focus, output);
    self.request(pool, focus);
    self.mesh_ready(world, pool, focus);
  }

  /// 收割已完成的作业：生成完的装进世界，网格化完的交给渲染侧。
  fn reap(&mut self, world: &mut World, output: &mut StreamOutput) {
    for slot in self.slots.values_mut() {
      slot.reap(world, output);
    }
  }

  /// 卸载出视距的区块：取消在跑的作业、从世界里摘掉、告诉渲染侧回收。
  fn unload(&mut self, world: &mut World, focus: ChunkPos, output: &mut StreamOutput) {
    let bounds = self.bounds;
    self.slots.retain(|&pos, slot| {
      if bounds.contains(focus, pos) {
        return true;
      }
      // 协作式取消：作业可能刚好跑完，结果没人取就地丢弃——这正是想要的。
      match &slot.state {
        SlotState::Generating(handle) => handle.cancel(),
        SlotState::Meshing(handle) => handle.cancel(),
        SlotState::PendingMesh | SlotState::Ready => {}
      }
      world.remove(pos);
      output.unloaded.push(pos);
      false
    });
  }

  /// 补申请视距内还缺的区块，每帧至多 [`SPAWN_BUDGET`] 块。
  fn request(&mut self, pool: &JobPool, focus: ChunkPos) {
    let bounds = self.bounds;
    let mut missing = Vec::new();
    for y in bounds.min_y..=bounds.max_y {
      for z in (focus.z - bounds.radius)..=(focus.z + bounds.radius) {
        for x in (focus.x - bounds.radius)..=(focus.x + bounds.radius) {
          let pos = ChunkPos::new(x, y, z);
          if !self.slots.contains_key(&pos) {
            missing.push(pos);
          }
        }
      }
    }

    // 近的先来：视距边缘晚一拍不要紧，脚下不能是空的。`sort_by_key` 稳定，同距的按扫描
    // 顺序排，因此每帧申请哪几块是确定的。
    missing.sort_by_key(|pos| chunk_distance_sq(focus, *pos));

    for pos in missing.into_iter().take(SPAWN_BUDGET) {
      // 生成是纯函数：作业只带走 (seed, palette, pos)，摸不到世界。
      let generator = self.generator;
      let handle = pool.spawn(move |_cx: &JobContext<'_>| generator.generate(pos));
      self
        .slots
        .insert(pos, Slot::new(SlotState::Generating(handle)));
    }
  }

  /// 给该网格化（或该重算）的区块提交网格化作业。
  fn mesh_ready(&mut self, world: &World, pool: &JobPool, focus: ChunkPos) {
    // 先收集再改 map：边遍历边插入会破坏借用。
    let pending: Vec<ChunkPos> = self
      .slots
      .iter()
      .filter(|(_, slot)| match slot.state {
        // 刚进世界：还没网格化过。
        SlotState::PendingMesh => true,
        // 方块被改过：手里这份几何旧了。
        SlotState::Ready => slot.dirty,
        // 作业在跑：等它回来（回来时若还脏，会退回待网格化，见 `Slot::reap`）——在这里再排
        // 一次只会让同一区块有两份在跑的结果打架。
        SlotState::Generating(_) | SlotState::Meshing(_) => false,
      })
      .map(|(&pos, _)| pos)
      .collect();

    for pos in pending {
      if !self.neighbors_ready(world, focus, pos) {
        continue;
      }
      // 中心必然在世界上（`PendingMesh` 就是「刚装进世界」的意思）；真取不到就留到下一帧。
      // 重算的区块也走这里：快照是**此刻**的，所以吃得到刚改过的方块。
      let Some(neighborhood) = world.neighborhood(pos) else {
        continue;
      };
      // 注册表建好就不再改，网格化作业拿一份只读句柄走，既不加锁也不影响世界。
      let blocks = world.registry_handle();
      let handle =
        pool.spawn(move |_cx: &JobContext<'_>| crate::meshing::mesh_chunk(&neighborhood, &blocks));
      let slot = self.slots.get_mut(&pos).expect("刚遍历出来的区块");
      slot.state = SlotState::Meshing(handle);
      // 脏在这里清掉：这份作业吃的是此刻的快照。跑着的时候再被改，脏会重新置上。
      slot.dirty = false;
    }
  }

  /// 能不能网格化了：**落在视距内的**邻居必须都已进世界。
  ///
  /// 视距外的邻居按空气处理是有意的——那边本没有地形，越界那面照常长出来；相机移过去、
  /// 真实邻居补上后，这一面会被邻居的实体几何挡在内部，看不见，也就不必回头重算。
  fn neighbors_ready(&self, world: &World, focus: ChunkPos, pos: ChunkPos) -> bool {
    Face::ALL.iter().all(|face| {
      let neighbor = pos.offset(face.step());
      world.chunk(neighbor).is_some() || !self.bounds.contains(focus, neighbor)
    })
  }
}

/// 两个区块位置的距离平方：只用来定「先申请哪块」的先后。
fn chunk_distance_sq(a: ChunkPos, b: ChunkPos) -> i64 {
  let dx = i64::from(a.x - b.x);
  let dy = i64::from(a.y - b.y);
  let dz = i64::from(a.z - b.z);
  dx * dx + dy * dy + dz * dz
}

#[cfg(test)]
mod tests {
  use super::*;
  use crate::runtime::Wakeup;
  use crate::world::block::{BlockDef, BlockId, BlockPalette, BlockRegistry, FaceTiles};
  use std::time::{Duration, Instant};

  /// 不做任何事的唤醒：单测只看世界与产出，不关心主循环有没有被打断。
  struct NullWakeup;

  impl Wakeup for NullWakeup {
    fn wake(&self) {}
  }

  /// 建表、解析方块集、起一个世界。返回世界与配套的生成器。
  fn world_and_generator(seed: u64) -> (World, IslandGenerator) {
    let mut registry = BlockRegistry::new();
    let mut register = |key: &'static str, tile: u16| {
      registry.register(BlockDef {
        key,
        solid: true,
        tiles: FaceTiles::uniform(tile),
      })
    };
    let palette = BlockPalette {
      stone: register("stone", 1),
      dirt: register("dirt", 2),
      grass: register("grass", 3),
    };
    (World::new(registry), IslandGenerator::new(seed, palette))
  }

  fn pool(workers: usize) -> JobPool {
    JobPool::with_workers(workers, Arc::new(NullWakeup))
  }

  /// 诊断用：槽位状态名。
  fn slot_name(slot: &Slot) -> &'static str {
    match slot.state {
      SlotState::Generating(_) => "generating",
      SlotState::PendingMesh => "pending",
      SlotState::Meshing(_) => "meshing",
      SlotState::Ready => "ready",
    }
  }

  /// 反复推进到没有在跑的作业，返回期间累计的产出。
  ///
  /// 流式是异步的，单测只做有界等待。超时给得很宽——noise 生成在 debug 下本来就慢，这个
  /// 上限是用来抓「卡死」的，不是拿来测性能的。
  fn settle(
    streamer: &mut ChunkStreamer,
    world: &mut World,
    pool: &JobPool,
    focus: ChunkPos,
  ) -> StreamOutput {
    let deadline = Instant::now() + Duration::from_secs(30);
    let mut merged = StreamOutput::default();

    loop {
      let mut output = StreamOutput::default();
      streamer.update(world, pool, focus, &mut output);
      merged.meshed.extend(output.meshed);
      merged.unloaded.extend(output.unloaded);

      // 收敛判据只看**槽位状态**，不看句柄的 `is_finished`：作业可能刚好在上一次收割与这次
      // 判断之间跑完，那时结果还没被收走，槽位仍停在 Generating——退出早了，断言就会看见
      // 半成品。槽位脱离 Generating / Meshing，才说明结果都进过世界、进过产出。
      let busy = streamer
        .slots
        .values()
        .any(|slot| matches!(slot.state, SlotState::Generating(_) | SlotState::Meshing(_)));
      if !busy {
        return merged;
      }

      assert!(
        Instant::now() < deadline,
        "流式未在 30s 内收敛：{:?}",
        streamer
          .slots
          .iter()
          .map(|(pos, slot)| (pos.x, pos.y, pos.z, slot_name(slot)))
          .collect::<Vec<_>>()
      );
      std::thread::sleep(Duration::from_millis(1));
    }
  }

  #[test]
  fn fills_bounds_and_settles_every_chunk() {
    let (mut world, generator) = world_and_generator(7);
    let pool = pool(4);
    // 单层：纵向邻居全在视距外，按空气处理——顺带覆盖「越界即空气」这条口径。
    // 范围再放大只是多生成几块同样重的岛，对断言没有增量，单测不铺张。
    let bounds = StreamBounds {
      radius: 1,
      min_y: 0,
      max_y: 0,
    };
    let mut streamer = ChunkStreamer::new(bounds, generator);
    let focus = ChunkPos::new(0, 0, 0);

    let output = settle(&mut streamer, &mut world, &pool, focus);

    assert_eq!(streamer.tracked(), bounds.chunk_count());
    assert_eq!(
      world.chunk_count(),
      bounds.chunk_count(),
      "该有的区块都进世界了"
    );
    assert!(
      streamer
        .slots
        .values()
        .all(|slot| matches!(slot.state, SlotState::Ready)),
      "邻居都在位的区块该全部走完网格化"
    );
    assert!(!output.meshed.is_empty(), "岛内区块该产出几何体");
    assert!(
      output
        .meshed
        .iter()
        .all(|mesh| bounds.contains(focus, mesh.pos)),
      "只该网格化视距内的区块"
    );
    assert!(output.unloaded.is_empty(), "没动过焦点，不该有卸载");
  }

  #[test]
  fn chunks_beyond_bounds_are_treated_as_air() {
    let (mut world, generator) = world_and_generator(7);
    let pool = pool(4);
    // 只有一个区块，六个邻居全在视距外 → 全部按空气处理，于是它自己四周的面都该长出来。
    let bounds = StreamBounds {
      radius: 0,
      min_y: -1,
      max_y: -1,
    };
    let mut streamer = ChunkStreamer::new(bounds, generator);
    let focus = ChunkPos::new(0, -1, 0);

    let output = settle(&mut streamer, &mut world, &pool, focus);

    assert_eq!(streamer.tracked(), 1);
    assert!(matches!(
      streamer.slots.get(&focus).map(|slot| &slot.state),
      Some(SlotState::Ready)
    ));
    // 这块整块埋在岛里（世界 y -32..-1），里面没有面可画，只有六个边界面。
    let mesh = output.meshed.first().expect("岛内区块该产出几何体");
    assert_eq!(mesh.pos, focus);
    assert_eq!(mesh.indices.len() / 6, 6, "六个朝向的收口面各一张矩形");
  }

  #[test]
  fn leaving_the_bounds_unloads_everything() {
    let (mut world, generator) = world_and_generator(7);
    let pool = pool(4);
    let bounds = StreamBounds {
      radius: 1,
      min_y: 0,
      max_y: 0,
    };
    let mut streamer = ChunkStreamer::new(bounds, generator);
    settle(&mut streamer, &mut world, &pool, ChunkPos::new(0, 0, 0));

    let mut output = StreamOutput::default();
    streamer.update(&mut world, &pool, ChunkPos::new(8, 0, 0), &mut output);

    assert_eq!(
      output.unloaded.len(),
      bounds.chunk_count(),
      "焦点一走远，旧范围整片卸载"
    );
    assert_eq!(world.chunk_count(), 0, "世界里的区块该同步摘干净");
    // 新范围当场就申请上了，只是还在生成。
    assert_eq!(streamer.tracked(), bounds.chunk_count());
  }

  #[test]
  fn spawn_budget_limits_one_frame() {
    let (mut world, generator) = world_and_generator(7);
    let pool = pool(2);
    let bounds = StreamBounds {
      radius: 4,
      min_y: 0,
      max_y: 0,
    };
    let mut streamer = ChunkStreamer::new(bounds, generator);
    let focus = ChunkPos::new(0, 0, 0);

    let mut output = StreamOutput::default();
    streamer.update(&mut world, &pool, focus, &mut output);
    assert_eq!(streamer.tracked(), SPAWN_BUDGET, "首帧只申请一格预算");

    // 先申请的必是近处的那批：距焦点最远的一块不该挤进第一帧。
    let far = ChunkPos::new(4, 0, 4);
    assert!(!streamer.slots.contains_key(&far), "远处区块该排在近处之后");
  }

  #[test]
  fn reload_after_unload_restores_the_same_terrain() {
    let (mut world, generator) = world_and_generator(7);
    let pool = pool(4);
    // 换成表层：这一层草、土、空气混在一起，形状比对才有分量（岛心那块清一色石）。
    let bounds = StreamBounds {
      radius: 1,
      min_y: 1,
      max_y: 1,
    };
    let mut streamer = ChunkStreamer::new(bounds, generator);
    let home = ChunkPos::new(0, 0, 0);
    let sample = ChunkPos::new(0, 1, 0);
    settle(&mut streamer, &mut world, &pool, home);
    let before = world.chunk(sample).expect("表层区块该在").blocks().to_vec();

    // 走远 → 回来 → 再看同一块。
    settle(&mut streamer, &mut world, &pool, ChunkPos::new(32, 0, 32));
    assert!(world.chunk(sample).is_none(), "走远后该块已卸载");
    settle(&mut streamer, &mut world, &pool, home);

    let after = world.chunk(sample).expect("回来后又该有").blocks();
    assert_eq!(before, after, "同 seed 同 pos：卸载再加载不丢形状");
  }

  /// 产出的网格里找某区块那份（同一区块至多一份，倒着找只是图省事）。
  fn mesh_of(meshes: &[ChunkMesh], pos: ChunkPos) -> Option<&ChunkMesh> {
    meshes.iter().rev().find(|mesh| mesh.pos == pos)
  }

  #[test]
  fn marking_a_chunk_dirty_remeshes_it() {
    let (mut world, generator) = world_and_generator(7);
    let pool = pool(4);
    // 单层、整层埋在岛里（世界 y −32..−1）：深处全是实心，好构造一块「内部方块」。
    let bounds = StreamBounds {
      radius: 1,
      min_y: -1,
      max_y: -1,
    };
    let mut streamer = ChunkStreamer::new(bounds, generator);
    let focus = ChunkPos::new(0, -1, 0);
    let first = settle(&mut streamer, &mut world, &pool, focus);

    // 区块正中那块：离每个面都远，改动不牵动邻居。
    let pos = ChunkPos::new(0, -1, 0);
    let origin = pos.origin();
    let target = [origin[0] + 16, origin[1] + 16, origin[2] + 16];
    assert!(world.is_solid(target), "岛内深处该是实心");

    let touched = world.set_block(target, BlockId::AIR);
    assert_eq!(touched, vec![pos], "深处的改动只关这一块");

    streamer.mark_dirty(&touched);
    let second = settle(&mut streamer, &mut world, &pool, focus);

    let remeshed: Vec<ChunkPos> = second.meshed.iter().map(|mesh| mesh.pos).collect();
    assert_eq!(remeshed, vec![pos], "只有标脏的那块该重算并再交一份网格");
    assert!(second.unloaded.is_empty(), "重算不该顺手卸载");
    let before = mesh_of(&first.meshed, pos).expect("首轮该网格化过这块");
    let after = mesh_of(&second.meshed, pos).expect("重算后该再交一份");
    assert_ne!(before, after, "挖掉一块后几何该变");
  }

  #[test]
  fn a_border_edit_remeshes_the_face_neighbour_too() {
    let (mut world, generator) = world_and_generator(7);
    let pool = pool(4);
    let bounds = StreamBounds {
      radius: 1,
      min_y: -1,
      max_y: -1,
    };
    let mut streamer = ChunkStreamer::new(bounds, generator);
    let focus = ChunkPos::new(0, -1, 0);
    settle(&mut streamer, &mut world, &pool, focus);

    // x 落在区块的负向面上（local x = 0），面内取正中——免得再牵出 y / z 两个邻居。
    let pos = ChunkPos::new(0, -1, 0);
    let origin = pos.origin();
    let target = [origin[0], origin[1] + 16, origin[2] + 16];
    let touched = world.set_block(target, BlockId::AIR);
    let neighbour = pos.offset([-1, 0, 0]);
    assert_eq!(touched, vec![pos, neighbour]);

    streamer.mark_dirty(&touched);
    let second = settle(&mut streamer, &mut world, &pool, focus);

    let mut remeshed: Vec<ChunkPos> = second.meshed.iter().map(|mesh| mesh.pos).collect();
    remeshed.sort_by_key(|pos| (pos.x, pos.y, pos.z));
    let mut expected = touched.clone();
    expected.sort_by_key(|pos| (pos.x, pos.y, pos.z));
    assert_eq!(remeshed, expected, "本体与贴面的邻居都该重算");
  }
}
