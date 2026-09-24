//! 体素碰撞：AABB 扫掠查询，只回答「这一帧我能走到哪」。
//!
//! 不含重力、地面判定、跳跃、台阶、站立 / 蹲下——那些属于「玩家控制器」，排在 M2 之后。
//! 相机因此仍是自由飞行：位移先过一遍这里的夹紧，撞墙就停（见 `docs/roadmap.md` 的「碰撞」）。
//!
//! 做法是**沿轴分离**：先解 x、再 y、再 z，解完一轴就把这一轴的位移落定，下一轴用的是落定后的
//! 位置。斜着撞角因此不会穿过去（三轴一起解时，「两轴同时被挡」的那一格角会算漏）。
//! 每个轴内部只看**该轴前方的第一格实体**，沿轴找最近的挡路格，所以求解是常数级的。
//!
//! 缺失区块按空气：那边没有地形，也就没有可撞的东西。

use crate::world::World;

/// 贴面判定的容差：包围盒内缩这么多再取整，正好贴在格子边界上时不算重叠。
const EPS: f32 = 1e-4;

/// 轴对齐包围盒（世界坐标，浮点）。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Aabb {
  pub center: [f32; 3],
  /// 各轴的半长。
  pub half: [f32; 3],
}

impl Aabb {
  pub const fn new(center: [f32; 3], half: [f32; 3]) -> Self {
    Self { center, half }
  }

  /// 最小角。
  pub fn min(&self) -> [f32; 3] {
    std::array::from_fn(|axis| self.center[axis] - self.half[axis])
  }

  /// 最大角。
  pub fn max(&self) -> [f32; 3] {
    std::array::from_fn(|axis| self.center[axis] + self.half[axis])
  }
}

/// 扫掠：`aabb` 按 `delta` 平移，返回**实际能走的位移**（被实体挡住的轴夹到贴面）。
///
/// 逐轴推进：解完一轴就把中心挪过去，下一轴吃的是挪过之后的包围盒——这正是「斜撞不穿角」的
/// 由来。前提是**起始位置不与实体重叠**（调用方每帧都过一遍夹紧，位置一直是合法的）。
pub fn sweep(world: &World, aabb: Aabb, delta: [f32; 3]) -> [f32; 3] {
  let mut center = aabb.center;
  let mut allowed = [0.0; 3];

  for axis in 0..3 {
    let moved = sweep_axis(world, center, aabb.half, axis, delta[axis]);
    allowed[axis] = moved;
    center[axis] += moved;
  }

  allowed
}

/// 单轴扫掠：返回这一轴上允许的位移。
fn sweep_axis(world: &World, center: [f32; 3], half: [f32; 3], axis: usize, amount: f32) -> f32 {
  if amount == 0.0 {
    return 0.0;
  }

  // 另外两轴覆盖的体素区间：这就是要检查的「一列」的截面。
  let others: [usize; 2] = match axis {
    0 => [1, 2],
    1 => [0, 2],
    _ => [0, 1],
  };
  let span = std::array::from_fn(|slot| {
    let other = others[slot];
    let lo = center[other] - half[other];
    let hi = center[other] + half[other];
    let lo_cell = lo.floor() as i32;
    // 最大角内缩 EPS 再取整：正好贴在格子上时不算进那一格。
    let hi_cell = ((hi - EPS).floor() as i32).max(lo_cell);
    (other, lo_cell, hi_cell)
  });

  let lo = center[axis] - half[axis];
  let hi = center[axis] + half[axis];
  let destination = lo + amount;

  if amount > 0.0 {
    // 前方第一格：`ceil(hi)`——贴在某格边界上时，那一格就是正前方的第一格。
    let mut cell = hi.ceil() as i32;
    while (cell as f32) < hi + amount {
      if hits_solid(world, axis, cell, span) {
        // 让最大角贴住这一格的最小面。
        return cell as f32 - hi;
      }
      cell += 1;
    }
  } else {
    // 反方向：前方第一格是 `floor(lo) − 1`（内含 lo 的那一格默认是空的，见上面的前提）。
    let mut cell = lo.floor() as i32 - 1;
    while (cell as f32 + 1.0) > destination {
      if hits_solid(world, axis, cell, span) {
        // 让最小角贴住这一格的最大面。
        return cell as f32 + 1.0 - lo;
      }
      cell -= 1;
    }
  }

  amount
}

/// 「该轴一格 × 另外两轴一段」这一列里有没有实体。
fn hits_solid(world: &World, axis: usize, cell: i32, span: [(usize, i32, i32); 2]) -> bool {
  let mut pos = [0i32; 3];
  pos[axis] = cell;
  for first in span[0].1..=span[0].2 {
    for second in span[1].1..=span[1].2 {
      pos[span[0].0] = first;
      pos[span[1].0] = second;
      if world.is_solid(pos) {
        return true;
      }
    }
  }
  false
}

#[cfg(test)]
mod tests {
  use super::*;
  use crate::world::block::BlockId;
  use crate::world::test_support;

  /// 相机那种小方块脑袋。
  const HALF: [f32; 3] = [0.3, 0.3, 0.3];

  /// 在 `x = at` 立一堵墙，覆盖给定的 y / z 区间。
  fn wall_at_x(world: &mut World, stone: BlockId, at: i32, y: [i32; 2], z: [i32; 2]) {
    for y in y[0]..=y[1] {
      for z in z[0]..=z[1] {
        world.set_block([at, y, z], stone);
      }
    }
  }

  /// 把盒子坐标按位移挪过去。
  fn moved(center: [f32; 3], delta: [f32; 3]) -> [f32; 3] {
    std::array::from_fn(|axis| center[axis] + delta[axis])
  }

  /// 盒子（按 EPS 内缩）是否与实体重叠——用来断言「没穿进去」。
  ///
  /// 两端都收 EPS 再取整：夹紧算出的是「贴住面」（比如最小角落在 1.0），f32 舍入会让它差个
  /// 几百分之一格落在面里侧，不内缩就会把「贴着」误判成「重叠」。
  fn overlaps_solid(world: &World, center: [f32; 3], half: [f32; 3]) -> bool {
    let aabb = Aabb::new(center, half);
    let lo = aabb.min();
    let hi = aabb.max();
    for y in (lo[1] + EPS).floor() as i32..=(hi[1] - EPS).floor() as i32 {
      for z in (lo[2] + EPS).floor() as i32..=(hi[2] - EPS).floor() as i32 {
        for x in (lo[0] + EPS).floor() as i32..=(hi[0] - EPS).floor() as i32 {
          if world.is_solid([x, y, z]) {
            return true;
          }
        }
      }
    }
    false
  }

  #[test]
  fn empty_space_returns_the_delta_unchanged() {
    let (world, _) = test_support::world();

    let delta = sweep(&world, Aabb::new([0.5, 40.5, 0.5], HALF), [1.5, -2.0, 3.0]);

    assert_eq!(delta, [1.5, -2.0, 3.0]);
  }

  #[test]
  fn moving_down_stops_on_the_floor() {
    let (world, _) = test_support::floor_world();
    let center = [2.0, 3.0, 2.0];

    let delta = sweep(&world, Aabb::new(center, HALF), [0.0, -5.0, 0.0]);

    // 地面顶面在 y = 1.0，最小角从 2.7 落到 1.0 就走不动了。
    assert!((delta[1] + 1.7).abs() < EPS, "实得 {}", delta[1]);
    assert!(!overlaps_solid(&world, moved(center, delta), HALF));
  }

  #[test]
  fn moving_up_into_a_ceiling_stops_below_it() {
    let (mut world, stone) = test_support::floor_world();
    // 头顶一层天花板（世界 y = 8）。
    wall_at_x(&mut world, stone, 2, [8, 8], [1, 3]);
    let center = [2.0, 3.0, 2.0];

    // 天花板只在 x = 2 那一格，盒子中心在 x = 2.0，横向必然压住它。
    let delta = sweep(&world, Aabb::new(center, HALF), [0.0, 5.0, 0.0]);

    // 天花板底面在 y = 8.0，最大角从 3.3 升到 8.0 就到头了。
    assert!((delta[1] - 4.7).abs() < EPS, "实得 {}", delta[1]);
    assert!(!overlaps_solid(&world, moved(center, delta), HALF));
  }

  #[test]
  fn walking_into_a_wall_stops_at_its_face() {
    let (mut world, stone) = test_support::floor_world();
    wall_at_x(&mut world, stone, 5, [1, 3], [1, 3]);
    let center = [2.0, 2.0, 2.0];

    let delta = sweep(&world, Aabb::new(center, HALF), [5.0, 0.0, 0.0]);

    // 墙面在 x = 5.0，最大角从 2.3 推到 5.0。
    assert!((delta[0] - 2.7).abs() < EPS, "实得 {}", delta[0]);
    assert!(!overlaps_solid(&world, moved(center, delta), HALF));
  }

  #[test]
  fn sliding_along_a_wall_keeps_the_free_axis() {
    let (mut world, stone) = test_support::floor_world();
    wall_at_x(&mut world, stone, 5, [1, 3], [1, 3]);
    let center = [2.0, 2.0, 2.0];

    let delta = sweep(&world, Aabb::new(center, HALF), [5.0, 0.0, 3.0]);

    // 沿轴分离的好处：x 被挡死，z 照走不误（贴着墙滑过去）。
    assert!(
      (delta[0] - 2.7).abs() < EPS,
      "x 该被夹住，实得 {}",
      delta[0]
    );
    assert!(
      (delta[2] - 3.0).abs() < EPS,
      "z 该原样通过，实得 {}",
      delta[2]
    );
    assert!(!overlaps_solid(&world, moved(center, delta), HALF));
  }

  #[test]
  fn sweeping_diagonally_never_ends_up_inside_a_solid() {
    let (mut world, stone) = test_support::floor_world();
    // 一根柱子：斜着从它角上蹭过去。
    world.set_block([5, 2, 5], stone);
    let center = [4.0, 2.0, 4.0];

    let delta = sweep(&world, Aabb::new(center, HALF), [2.0, 0.0, 2.0]);

    assert!(!overlaps_solid(&world, moved(center, delta), HALF));
    assert!(
      delta[0] > 0.0 || delta[2] > 0.0,
      "该往前走了一截，而不是原地不动"
    );
  }

  #[test]
  fn missing_chunks_are_not_solid() {
    let (world, _) = test_support::world();

    // 世界是空的：视距外没有地形，也就没有可撞的东西。
    let delta = sweep(
      &world,
      Aabb::new([200.0, 40.0, 200.0], HALF),
      [0.0, -9.0, 0.0],
    );

    assert_eq!(delta, [0.0, -9.0, 0.0]);
  }
}
