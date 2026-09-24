//! 体素射线：DDA 步进（Amanatides & Woo），返回命中方块与命中面。
//!
//! **必须沿网格步进，不能沿射线采样**——采样会漏掉斜面上的方块与角落（两次采样之间可以整块
//! 穿过去）。DDA 每一步都跨过一条网格边界、只检查新进的那一格，因此「射线上第一个实体方块」
//! 一定找得到，而且还知道是**从哪个面进去的**。
//!
//! 实心判定统一走 [`World::is_solid`]：缺失区块按空气，所以朝视距外打自然是落空。
//!
//! 命中面是这里唯一能给出「点的是哪一面」的信息：放置位置 = 命中方块 + 命中面法线，
//! 破坏位置 = 命中方块本身（见 `docs/roadmap.md` 的「射线投射」）。

use crate::world::World;
use crate::world::block::Face;

/// 射线命中。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct RayHit {
  /// 命中方块的**世界方块坐标**。
  pub block: [i32; 3],
  /// 命中面：法线朝外，也就是指向射线来路的那一侧。
  pub face: Face,
  /// 起点到命中面的距离（方块）。
  pub distance: f32,
}

/// 从 `origin` 沿 `direction` 步进，返回第一个实体方块。
///
/// - `direction` 会被归一化；零向量（或非有限值）返回 `None`
/// - `max_distance` 是射程上限（方块），超出即落空
/// - 起点所在方块若已是实体（眼位埋在实心里）返回 `None`——那里没有可点的一面
pub fn raycast(
  world: &World,
  origin: [f32; 3],
  direction: [f32; 3],
  max_distance: f32,
) -> Option<RayHit> {
  let length =
    (direction[0] * direction[0] + direction[1] * direction[1] + direction[2] * direction[2])
      .sqrt();
  if !length.is_finite() || length <= f32::EPSILON {
    return None;
  }
  let dir = [
    direction[0] / length,
    direction[1] / length,
    direction[2] / length,
  ];

  // 起点所在格：`floor`。正好落在边界上时归上面那一格（与 `ChunkPos::from_world` 的取整口径
  // 一致），往下看因此看得见脚下那块。
  let mut cell = [
    origin[0].floor() as i32,
    origin[1].floor() as i32,
    origin[2].floor() as i32,
  ];
  if world.is_solid(cell) {
    return None;
  }

  // 每轴三个量：步进方向、跨一格需要的射线长度（分量为 0 的轴永不参与）、到下一次边界的距离。
  // 一次算好，循环里只剩比较与加法——这正是 DDA 便宜的原因。
  let mut step = [0i32; 3];
  let mut t_max = [f32::INFINITY; 3];
  let mut t_delta = [f32::INFINITY; 3];
  for axis in 0..3 {
    if dir[axis] > 0.0 {
      step[axis] = 1;
      t_delta[axis] = 1.0 / dir[axis];
      t_max[axis] = (cell[axis] as f32 + 1.0 - origin[axis]) / dir[axis];
    } else if dir[axis] < 0.0 {
      step[axis] = -1;
      t_delta[axis] = -1.0 / dir[axis];
      t_max[axis] = (origin[axis] - cell[axis] as f32) / -dir[axis];
    }
  }

  loop {
    // 走 t 最小的那个轴：先跨过最近的一条网格边界。
    let axis = if t_max[0] < t_max[1] && t_max[0] < t_max[2] {
      0
    } else if t_max[1] < t_max[2] {
      1
    } else {
      2
    };

    let distance = t_max[axis];
    if distance > max_distance {
      return None;
    }
    t_max[axis] += t_delta[axis];
    cell[axis] += step[axis];

    if world.is_solid(cell) {
      return Some(RayHit {
        block: cell,
        // 沿 +轴 跨进去 = 从那一格朝 −轴 的那一面进去的，反之亦然。
        face: entered_face(axis, step[axis]),
        distance,
      });
    }
  }
}

/// 沿某轴跨入一格时，露在来路上的那一面。
const fn entered_face(axis: usize, step: i32) -> Face {
  match axis {
    0 if step > 0 => Face::NegX,
    0 => Face::PosX,
    1 if step > 0 => Face::NegY,
    1 => Face::PosY,
    2 if step > 0 => Face::NegZ,
    _ => Face::PosZ,
  }
}

#[cfg(test)]
mod tests {
  use super::*;
  use crate::world::test_support;

  /// 一块 `y = 0` 是石头地面的世界：从上方打下来都能命中。
  fn world_with_floor() -> World {
    test_support::floor_world().0
  }

  #[test]
  fn straight_down_hits_the_floor_top() {
    let world = world_with_floor();

    let hit = raycast(&world, [0.5, 4.5, 0.5], [0.0, -1.0, 0.0], 8.0).expect("该命中地面");

    assert_eq!(hit.block, [0, 0, 0]);
    assert_eq!(hit.face, Face::PosY, "从上面下来，看见的是顶面");
    assert!((hit.distance - 3.5).abs() < 1e-5, "实得 {}", hit.distance);
  }

  #[test]
  fn ray_from_a_face_boundary_still_hits() {
    let world = world_with_floor();

    // 站在地面顶面（y = 1.0）上往下看：脚下那块就在 0 距离处。
    let hit = raycast(&world, [0.5, 1.0, 0.5], [0.0, -1.0, 0.0], 8.0).expect("该命中脚下");

    assert_eq!(hit.block, [0, 0, 0]);
    assert_eq!(hit.face, Face::PosY);
    assert!(hit.distance.abs() < 1e-5);
  }

  #[test]
  fn horizontal_ray_reports_the_face_it_came_from() {
    let (mut world, stone) = test_support::floor_world();
    world.set_block([4, 2, 0], stone);

    let hit = raycast(&world, [0.5, 2.5, 0.5], [1.0, 0.0, 0.0], 8.0).expect("该命中那面墙");

    assert_eq!(hit.block, [4, 2, 0]);
    assert_eq!(hit.face, Face::NegX, "沿 +x 打过去，看见的是它的 −x 面");
    assert!((hit.distance - 3.5).abs() < 1e-5, "实得 {}", hit.distance);
  }

  #[test]
  fn slanted_ray_steps_cell_by_cell_and_reports_the_face() {
    let (mut world, stone) = test_support::floor_world();
    // 远处单独一块：斜射能不能不漏格地走到它，就看这里。
    world.set_block([10, 1, 0], stone);

    let hit = raycast(&world, [0.5, 1.5, 0.5], [1.0, -0.05, 0.0], 16.0).expect("该命中那块");

    assert_eq!(hit.block, [10, 1, 0]);
    assert_eq!(hit.face, Face::NegX);
    // 一路沿 +x 步进，y 还没掉到地面那一格就先撞上了。
    assert!(
      (hit.distance - 9.5).abs() < 0.05,
      "实得 {}（该在 y 掉到地面之前命中）",
      hit.distance
    );
  }

  #[test]
  fn respects_the_reach_limit() {
    let world = world_with_floor();

    assert!(raycast(&world, [0.5, 4.5, 0.5], [0.0, -1.0, 0.0], 3.4).is_none());
    assert!(raycast(&world, [0.5, 4.5, 0.5], [0.0, -1.0, 0.0], 8.0).is_some());
  }

  #[test]
  fn missing_chunks_are_treated_as_air() {
    let (world, _) = test_support::world();

    assert!(raycast(&world, [0.5, 4.5, 0.5], [0.0, -1.0, 0.0], 8.0).is_none());
  }

  #[test]
  fn zero_direction_returns_none() {
    let world = world_with_floor();

    assert!(raycast(&world, [0.5, 4.5, 0.5], [0.0, 0.0, 0.0], 8.0).is_none());
  }

  #[test]
  fn origin_inside_a_solid_returns_none() {
    let world = world_with_floor();

    assert!(
      raycast(&world, [0.5, 0.5, 0.5], [1.0, 0.0, 0.0], 8.0).is_none(),
      "眼位埋在实心里没有可点的一面"
    );
  }
}
