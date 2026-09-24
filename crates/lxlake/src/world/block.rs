//! 方块：注册表、定义、六个面。
//!
//! 方块**不是**枚举而是注册表索引——新增方块不该牵动引擎代码（M2+ 的脚本 / 存档也按
//! `key` 寻址，不按 `BlockId` 数值）。

use std::fmt;

/// 方块类型 id：注册表里的索引，由 [`BlockRegistry::register`] 分配。
///
/// 字段私有：id 只能从注册表出来，`BlockRegistry::get` 因此不必做越界分支。将来存档 /
/// 联机要从字节反序列化 id，那时在**边界上**校验，不要把裸构造放进来。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Default)]
pub struct BlockId(u16);

impl BlockId {
  /// 空气：不渲染、不参与面剔除，恒占 0 号位。
  pub const AIR: Self = Self(0);

  /// 注册表下标。
  pub const fn index(self) -> usize {
    self.0 as usize
  }

  /// 是否空气。
  pub const fn is_air(self) -> bool {
    self.0 == 0
  }
}

impl fmt::Display for BlockId {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    write!(f, "block#{}", self.0)
  }
}

/// 方块的一个面。
///
/// **顺序即下标**：`FaceTiles`、区块邻域快照的邻居数组都按这个顺序排，改动顺序会同时
/// 影响三处。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Face {
  PosX,
  NegX,
  PosY,
  NegY,
  PosZ,
  NegZ,
}

impl Face {
  /// 六个面，按 [`Face::index`] 的顺序。
  pub const ALL: [Face; 6] = [
    Face::PosX,
    Face::NegX,
    Face::PosY,
    Face::NegY,
    Face::PosZ,
    Face::NegZ,
  ];

  /// 面序数（0..6）。
  pub const fn index(self) -> usize {
    match self {
      Face::PosX => 0,
      Face::NegX => 1,
      Face::PosY => 2,
      Face::NegY => 3,
      Face::PosZ => 4,
      Face::NegZ => 5,
    }
  }

  /// 面法线（单位向量）。
  pub const fn normal(self) -> [f32; 3] {
    let step = self.step();
    [step[0] as f32, step[1] as f32, step[2] as f32]
  }

  /// 面的整数步进：法线方向上的相邻方块 / 相邻区块。
  pub const fn step(self) -> [i32; 3] {
    match self {
      Face::PosX => [1, 0, 0],
      Face::NegX => [-1, 0, 0],
      Face::PosY => [0, 1, 0],
      Face::NegY => [0, -1, 0],
      Face::PosZ => [0, 0, 1],
      Face::NegZ => [0, 0, -1],
    }
  }
}

/// 六面图集格：每面一个图集 tile 索引。
///
/// 图集格索引是**纯 CPU 概念**——`(tile, tile 内 uv)` 换算成图集 uv 是着色器的事，
/// 网格化不必知道图集多大（见 `meshing`）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FaceTiles(pub [u16; 6]);

impl FaceTiles {
  /// 六面同格。
  pub const fn uniform(tile: u16) -> Self {
    Self([tile; 6])
  }

  /// 取某一面的图集格。
  pub const fn get(self, face: Face) -> u16 {
    self.0[face.index()]
  }
}

/// 方块定义。M1 只到「是不是实体 + 六面图集格」；硬度、掉落、贴图动画等留给 M2+。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BlockDef {
  /// 稳定标识：存档、脚本、材质命名都以它为准。
  pub key: &'static str,
  /// 是否实体方块。实体之间互相剔除面；非实体（空气，将来的水 / 树叶）不剔除。
  pub solid: bool,
  /// 六面图集格。
  pub tiles: FaceTiles,
}

/// 方块注册表：`BlockId` 的分配者与查询面。
pub struct BlockRegistry {
  defs: Vec<BlockDef>,
}

impl BlockRegistry {
  /// 建表。`AIR` 已占 0 号位且不可覆写。
  pub fn new() -> Self {
    Self {
      defs: vec![BlockDef {
        key: "air",
        solid: false,
        tiles: FaceTiles::uniform(0),
      }],
    }
  }

  /// 注册一个方块，返回它的 id。
  pub fn register(&mut self, def: BlockDef) -> BlockId {
    let id = BlockId(self.defs.len() as u16);
    self.defs.push(def);
    id
  }

  /// 查定义。`id` 只能由本表产出，故越界即内部错误。
  pub fn get(&self, id: BlockId) -> &BlockDef {
    &self.defs[id.index()]
  }

  /// 方块种数（含空气）。
  pub fn len(&self) -> usize {
    self.defs.len()
  }

  /// 是否只有空气。
  pub fn is_empty(&self) -> bool {
    self.defs.len() <= 1
  }
}

impl Default for BlockRegistry {
  fn default() -> Self {
    Self::new()
  }
}

/// 地形用到的方块集合：由应用从 [`BlockRegistry`] 解析后交给生成器。
///
/// 框架**不预设方块名**——用哪几种方块砌岛是内容，不是引擎。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BlockPalette {
  pub stone: BlockId,
  pub dirt: BlockId,
  pub grass: BlockId,
}
