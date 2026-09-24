//! 世界侧的命令执行器：把命令落到 [`World`] 上。
//!
//! 是 [`CommandExecutor`] 的一层**借用视图**（`&mut World`），不是世界的持有者——世界仍由应用
//! 托管，应用的射线 / 碰撞 / 网格化都照旧直接用它。于是「落地方式」与「命令顺序」分开：
//! 总线管顺序（`runtime::CommandBus`），本层管落地。

use crate::core::command::Command;
use crate::runtime::CommandExecutor;
use crate::world::{ChunkPos, World};

/// 世界执行器：把命令交给 [`World::apply`]。
pub struct WorldExecutor<'a> {
  world: &'a mut World,
}

impl<'a> WorldExecutor<'a> {
  pub fn new(world: &'a mut World) -> Self {
    Self { world }
  }

  /// 借回世界——派发完还想接着用（渲染、标脏之外的读写）时走它。
  pub fn world_mut(&mut self) -> &mut World {
    self.world
  }
}

impl CommandExecutor for WorldExecutor<'_> {
  fn execute(&mut self, command: &Command) -> Vec<ChunkPos> {
    // 实体命令不属于世界：返回空，让总线问下一个执行器（见 `CommandExecutor` 的文档）。
    if matches!(command, Command::Entity(_)) {
      return Vec::new();
    }
    self.world.apply(command)
  }
}

#[cfg(test)]
mod tests {
  use super::*;
  use crate::core::command::{EntityCommand, EntityId};

  #[test]
  fn breaking_a_block_reports_the_chunk_it_touched() {
    let (mut world, _) = crate::world::test_support::floor_world();
    let mut executor = WorldExecutor::new(&mut world);

    let touched = executor.execute(&Command::Break { pos: [3, 0, 7] });

    assert!(touched.contains(&ChunkPos::new(0, 0, 0)));
    assert!(executor.world_mut().block_at([3, 0, 7]).is_air());
  }

  /// 空操作（挖本来就是空气的地方）返回空——总线据此继续问下一个执行器。
  #[test]
  fn a_no_op_edit_reports_nothing() {
    let (mut world, _) = crate::world::test_support::floor_world();
    let mut executor = WorldExecutor::new(&mut world);

    assert!(
      executor
        .execute(&Command::Break { pos: [5, 5, 5] })
        .is_empty()
    );
  }

  /// 实体命令不属于世界：世界执行器必须放手（返回空），免得把别人的命令吃掉。
  #[test]
  fn entity_commands_are_left_to_the_next_executor() {
    let (mut world, _) = crate::world::test_support::floor_world();
    let mut executor = WorldExecutor::new(&mut world);

    let command = Command::Entity(EntityCommand::Despawn { id: EntityId(1) });
    assert!(executor.execute(&command).is_empty());
  }
}
