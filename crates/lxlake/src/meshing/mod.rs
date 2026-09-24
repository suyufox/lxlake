//! 网格化：区块 → 顶点 / 索引，greedy meshing。
//!
//! **纯函数、无 GPU 类型**：输入是不可变的邻域快照（`Arc<Chunk>`，见 `world::chunk`），
//! 输出是 POD，渲染侧按同一布局解释它——本模块因此既能整块丢到工作线程上跑，也不会把
//! wgpu 拖进框架主线。
//!
//! 顶点格式与图集的分工：
//!
//! - 顶点带**图集格索引**（`tile`）与面内 uv
//! - uv 的**单位是方块**，不是 0..1：一个合并成 N 格宽的面，uv 取 0..N。着色器取 `fract(uv)`
//!   当格内坐标，于是贴图逐格重复而不是被拉成一大张（greedy meshing 下这是对的做法）
//! - 「格 index → 图集 uv」的换算在着色器里做（图集尺寸当 uniform 传），CPU 侧不必知道
//!   图集多大，改图集排布不动网格化

use crate::world::block::{BlockId, BlockRegistry, Face};
use crate::world::chunk::{CHUNK_SIZE, ChunkNeighborhood, ChunkPos};

/// 网格顶点：**POD**，`render` 按同一布局建 vertex buffer。
///
/// `Pod` 是硬要求的落实方式：GPU 看到的字节布局与这里的字段**逐字段一致**，没有隐式填充。
/// 顶点里没有区块偏移——上传时把区块原点加进来（见 `render`），因此全局只要一份 uniform。
/// 布局由 `#[repr(C)]` 定死、与特性无关；`Pod` 只是给渲染侧的 `cast_slice` 打卡，故随
/// `render` 特性开关（不开渲染就不必依赖 bytemuck）。
#[repr(C)]
#[cfg_attr(feature = "render", derive(bytemuck::Pod, bytemuck::Zeroable))]
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct Vertex {
  /// 区块**局部**坐标；渲染侧上传时加上区块原点，落到世界里。
  pub position: [f32; 3],
  /// 面法线。
  pub normal: [f32; 3],
  /// 面内 uv，**单位是方块**：合并成 N 格宽的面取 0..N，着色器按 `fract` 逐格重复贴图。
  pub uv: [f32; 2],
  /// 图集格索引。
  pub tile: u16,
  /// 对齐填充：让顶点是 4 字节对齐（`repr(C)` 下 `u16` 后面本来也会补，显式写出来更清楚）。
  pub _pad: u16,
}

/// 一个区块的网格。
///
/// greedy meshing 的产物，也是渲染侧的输入：`pos` 决定它摆在世界哪里，`unloaded` 时按
/// `pos` 回收 GPU 资源。
#[derive(Debug, Clone, PartialEq)]
pub struct ChunkMesh {
  pub pos: ChunkPos,
  pub vertices: Vec<Vertex>,
  pub indices: Vec<u32>,
}

impl ChunkMesh {
  /// 空网格（全空气区块）。
  pub fn empty(pos: ChunkPos) -> Self {
    Self {
      pos,
      vertices: Vec::new(),
      indices: Vec::new(),
    }
  }

  /// 没有任何面。
  pub fn is_empty(&self) -> bool {
    self.indices.is_empty()
  }
}

/// 网格化一个区块。
///
/// 纯函数：只吃快照与方块表，可在任意工作线程上跑；同输入必得同输出（可用来核对流式的
/// 卸载 / 重载是否稳）。六个面邻居缺席时按空气处理——那面照常长出几何体。
///
/// 做法是逐轴的 **greedy meshing**：对每个轴向、每个切片平面，先按「两侧实心与否」定出这
/// 一层要画的面，再把**同朝向同图集格**的相邻面合并成尽可能大的矩形。合并的是「同格同朝向」
/// ——不同方块只要图集格一致就一起并，视觉上没有区别。
pub fn mesh_chunk(neighborhood: &ChunkNeighborhood, blocks: &BlockRegistry) -> ChunkMesh {
  let center = neighborhood.center();
  let pos = center.pos();
  if center.is_empty() {
    return ChunkMesh::empty(pos);
  }

  let origin = pos.origin();
  let mut mesh = ChunkMesh {
    pos,
    vertices: Vec::new(),
    indices: Vec::new(),
  };

  // 读方块：块内走快路，越界才查邻域快照（缺失的邻居按空气）。
  let block_at = |local: [i32; 3]| -> BlockId {
    let inside = local.iter().all(|c| (0..CHUNK_SIZE as i32).contains(c));
    if inside {
      center.get([local[0] as usize, local[1] as usize, local[2] as usize])
    } else {
      neighborhood.block_at([
        origin[0] + local[0],
        origin[1] + local[1],
        origin[2] + local[2],
      ])
    }
  };
  let solid = |id: BlockId| blocks.get(id).solid;
  let tile_of = |id: BlockId, face: Face| blocks.get(id).tiles.get(face);

  // 每个切片平面一张掩码，复用同一块缓冲（省掉 96 次分配）。
  let mut mask: Vec<Option<MaskCell>> = vec![None; CHUNK_SIZE * CHUNK_SIZE];

  for axis in 0..3 {
    let plane = Plane::new(axis);
    let positive_face = axis_face(axis, true);
    let negative_face = axis_face(axis, false);

    // 切片从 0 到 CHUNK_SIZE（含两端）：`a` 是切片负侧那格，`b` 是正侧那格。
    for slice in 0..=CHUNK_SIZE {
      mask.fill(None);

      for v in 0..CHUNK_SIZE {
        for u in 0..CHUNK_SIZE {
          let mut local = [0i32; 3];
          local[plane.axis] = slice as i32 - 1;
          local[plane.u] = u as i32;
          local[plane.v] = v as i32;
          let a = block_at(local);

          local[plane.axis] = slice as i32;
          let b = block_at(local);

          let (solid_a, solid_b) = (solid(a), solid(b));
          let cell = if solid_a == solid_b {
            // 两侧同为空或同为实心：这里没有面。
            None
          } else if solid_a {
            Some(MaskCell {
              face: positive_face,
              tile: tile_of(a, positive_face),
            })
          } else {
            // 面属于正侧那格，朝负向。
            Some(MaskCell {
              face: negative_face,
              tile: tile_of(b, negative_face),
            })
          };
          mask[v * CHUNK_SIZE + u] = cell;
        }
      }

      merge_plane(&mut mesh, &mut mask, plane, slice);
    }
  }

  mesh
}

/// 一个轴向上的切片平面：`axis` 是法线轴，`u` / `v` 是面内的两个轴。
#[derive(Debug, Clone, Copy)]
struct Plane {
  axis: usize,
  u: usize,
  v: usize,
}

impl Plane {
  /// 三个轴轮换着当法线，面内两轴取其后继。
  const fn new(axis: usize) -> Self {
    Self {
      axis,
      u: (axis + 1) % 3,
      v: (axis + 2) % 3,
    }
  }
}

/// 平面上的一个矩形（面内起点 + 尺寸，单位都是方块）。
#[derive(Debug, Clone, Copy)]
struct Rect {
  u: usize,
  v: usize,
  width: usize,
  height: usize,
}

/// 平面上要画的一个面：朝向 + 图集格。同 `MaskCell` 才能合并。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct MaskCell {
  face: Face,
  tile: u16,
}

/// 轴 `axis` 的正 / 负向面。
///
/// [`Face::ALL`] 就是按「正负成对、逐轴排列」摆的，这里复用它，别另建一张表。
const fn axis_face(axis: usize, positive: bool) -> Face {
  Face::ALL[axis * 2 + if positive { 0 } else { 1 }]
}

/// 贪心合并一张平面上的掩码，把矩形逐个吐进网格。
///
/// 扫描顺序固定（先 v 后 u），合并出的矩形因此也是确定的——同输入必得同输出。
fn merge_plane(mesh: &mut ChunkMesh, mask: &mut [Option<MaskCell>], plane: Plane, slice: usize) {
  for row in 0..CHUNK_SIZE {
    let mut column = 0;
    while column < CHUNK_SIZE {
      let Some(cell) = mask[row * CHUNK_SIZE + column] else {
        column += 1;
        continue;
      };

      // 先沿 u 铺宽。
      let mut width = 1;
      while column + width < CHUNK_SIZE && mask[row * CHUNK_SIZE + column + width] == Some(cell) {
        width += 1;
      }

      // 再沿 v 铺高：整行都得同格才吃得下。
      let mut height = 1;
      'grow: while row + height < CHUNK_SIZE {
        for offset in 0..width {
          if mask[(row + height) * CHUNK_SIZE + column + offset] != Some(cell) {
            break 'grow;
          }
        }
        height += 1;
      }

      // 消费掉的格子抹平，后续扫描自然跳过。
      for dv in 0..height {
        for du in 0..width {
          mask[(row + dv) * CHUNK_SIZE + column + du] = None;
        }
      }

      push_quad(
        mesh,
        plane,
        slice,
        Rect {
          u: column,
          v: row,
          width,
          height,
        },
        cell,
      );
      column += width;
    }
  }
}

/// 把一个矩形面写成四个顶点 + 两个三角形。
///
/// 顶点顺序决定绕序：`u` 轴与 `v` 轴的单位向量叉乘恰好是 `+axis`，所以正向面按
/// `(u0,v0) → (u1,v0) → (u1,v1) → (u0,v1)` 走就是朝外，负向面反过来。
fn push_quad(mesh: &mut ChunkMesh, plane: Plane, slice: usize, rect: Rect, cell: MaskCell) {
  let (u0, v0) = (rect.u as f32, rect.v as f32);
  let (u1, v1) = (u0 + rect.width as f32, v0 + rect.height as f32);

  let corner = |du: f32, dv: f32| {
    let mut position = [0.0f32; 3];
    position[plane.axis] = slice as f32;
    position[plane.u] = du;
    position[plane.v] = dv;
    position
  };

  // uv 用**方块**做单位、以矩形左下角为原点：着色器 fract 一下就是格内坐标，贴图逐格重复。
  let corners = if cell.face.step()[plane.axis] > 0 {
    [
      (corner(u0, v0), [0.0, 0.0]),
      (corner(u1, v0), [u1 - u0, 0.0]),
      (corner(u1, v1), [u1 - u0, v1 - v0]),
      (corner(u0, v1), [0.0, v1 - v0]),
    ]
  } else {
    [
      (corner(u0, v0), [0.0, 0.0]),
      (corner(u0, v1), [0.0, v1 - v0]),
      (corner(u1, v1), [u1 - u0, v1 - v0]),
      (corner(u1, v0), [u1 - u0, 0.0]),
    ]
  };

  let base = mesh.vertices.len() as u32;
  for (position, uv) in corners {
    mesh.vertices.push(Vertex {
      position,
      normal: cell.face.normal(),
      uv,
      tile: cell.tile,
      _pad: 0,
    });
  }
  mesh
    .indices
    .extend_from_slice(&[base, base + 1, base + 2, base, base + 2, base + 3]);
}

#[cfg(test)]
mod tests {
  use super::*;
  use crate::world::block::{BlockDef, FaceTiles};
  use crate::world::chunk::Chunk;
  use std::sync::Arc;

  /// 一张表里三种实心方块，图集格各不相同。返回表与石头 id（注册顺序：石 / 土 / 草）。
  fn setup() -> (BlockRegistry, BlockId) {
    let mut registry = BlockRegistry::new();
    let mut ids = Vec::new();
    for (key, tile) in [("stone", 1u16), ("dirt", 2), ("grass", 3)] {
      ids.push(registry.register(BlockDef {
        key,
        solid: true,
        tiles: FaceTiles::uniform(tile),
      }));
    }
    (registry, ids[0])
  }

  /// 把一个区块和它的六个空邻居打包成邻域快照。
  fn neighborhood(center: Chunk) -> ChunkNeighborhood {
    ChunkNeighborhood::new(Arc::new(center), std::array::from_fn(|_| None))
  }

  /// 网格覆盖的总面积（方块²）。合并是否发生就看它和「面数」对不对得上。
  fn total_area(mesh: &ChunkMesh) -> f32 {
    (0..mesh.indices.len() / 6)
      .map(|quad| {
        // 每个矩形四个顶点连排，取首顶点下标即可定位这四角。
        let base = mesh.indices[quad * 6] as usize;
        let (p0, p1, p3) = (
          mesh.vertices[base].position,
          mesh.vertices[base + 1].position,
          mesh.vertices[base + 3].position,
        );
        let e1 = [p1[0] - p0[0], p1[1] - p0[1], p1[2] - p0[2]];
        let e2 = [p3[0] - p0[0], p3[1] - p0[1], p3[2] - p0[2]];
        let cross = [
          e1[1] * e2[2] - e1[2] * e2[1],
          e1[2] * e2[0] - e1[0] * e2[2],
          e1[0] * e2[1] - e1[1] * e2[0],
        ];
        (cross[0] * cross[0] + cross[1] * cross[1] + cross[2] * cross[2]).sqrt()
      })
      .sum()
  }

  fn quad_count(mesh: &ChunkMesh) -> usize {
    mesh.indices.len() / 6
  }

  #[test]
  fn single_block_has_six_faces() {
    let (registry, stone) = setup();
    let mut chunk = Chunk::new(ChunkPos::new(0, 0, 0));
    chunk.set([16, 16, 16], stone);

    let mesh = mesh_chunk(&neighborhood(chunk), &registry);

    assert_eq!(quad_count(&mesh), 6, "孤立方块六个面");
    assert_eq!(mesh.vertices.len(), 24);
    assert_eq!(total_area(&mesh), 6.0);
  }

  #[test]
  fn all_faces_are_present_once() {
    let (registry, stone) = setup();
    let mut chunk = Chunk::new(ChunkPos::new(0, 0, 0));
    chunk.set([16, 16, 16], stone);

    let mesh = mesh_chunk(&neighborhood(chunk), &registry);
    let mut normals = mesh
      .vertices
      .iter()
      .map(|vertex| vertex.normal)
      .collect::<Vec<_>>();
    normals.dedup();

    assert_eq!(normals.len(), 6, "六个朝向各一份");
  }

  #[test]
  fn empty_chunk_yields_empty_mesh() {
    let (registry, _) = setup();
    let chunk = Chunk::new(ChunkPos::new(3, -1, 2));
    let mesh = mesh_chunk(&neighborhood(chunk), &registry);

    assert!(mesh.is_empty());
    assert_eq!(mesh.pos, ChunkPos::new(3, -1, 2));
  }

  #[test]
  fn adjacent_blocks_merge_and_hide_the_shared_face() {
    let (registry, stone) = setup();
    let mut chunk = Chunk::new(ChunkPos::new(0, 0, 0));
    chunk.set([16, 16, 16], stone);
    chunk.set([17, 16, 16], stone);

    let mesh = mesh_chunk(&neighborhood(chunk), &registry);

    // 12 个面里藏掉中间 2 个，剩 10 个方块面；四对同向面各并成一张 → 6 个矩形。
    assert_eq!(total_area(&mesh), 10.0, "隐藏共享面后还剩 10 个面");
    assert_eq!(quad_count(&mesh), 6, "同向同格的相邻面该并成矩形");
  }

  #[test]
  fn far_apart_blocks_do_not_merge() {
    let (registry, stone) = setup();
    let mut chunk = Chunk::new(ChunkPos::new(0, 0, 0));
    chunk.set([4, 4, 4], stone);
    chunk.set([20, 20, 20], stone);

    let mesh = mesh_chunk(&neighborhood(chunk), &registry);

    assert_eq!(quad_count(&mesh), 12, "隔着空格的方块不该合并");
    assert_eq!(total_area(&mesh), 12.0);
  }
}
