//! 命令总线：帧内攒命令、帧边界**按序**派发。
//!
//! 命令是**数据**（[`Command`]），落地方式由执行器说话（[`CommandExecutor`]）。分开的理由是
//! 命令的来源会越来越多（HUD、脚本、回放、将来的网络），而落地只有那么几种：方块在世界，
//! 实体在实体存储。总线只管「顺序」这一件事——保序是 M2 验收第 4 条，也是 M4 存档与联机的
//! 前提（回放的命令序列必须与原局逐条一致）。
//!
//! 执行器**不注册在总线里**，而是派发时按序传进来（[`CommandBus::flush`]）：世界由应用自己托管，
//! 执行器要可变借用它（[`WorldExecutor`](crate::world::WorldExecutor) 就是一层这样的借用视图），
//! 把执行器收进总线就得把世界也一并搬进总线，应用其余部分（射线、碰撞、网格化）全会被迫改道。

use crate::core::command::Command;
use crate::world::ChunkPos;

/// 命令执行器：认得哪些命令、怎么落地，由实现说话。
///
/// **不认识的返回空**——总线按序问下去，第一个给出非空结果的执行器算认领了这条命令（后面的
/// 不再问）；一条都不认得就当丢弃（命令是数据，丢弃不该炸）。
///
/// 于是「空」有两层含义：不认识，或者认得但没什么可改（世界里的空操作，例如挖一块本来就是
/// 空气的地方）。两者对总线是同一件事——**继续问下一个**。所以执行器不该把手伸到别人的命令
/// 种类上，否则会把本该属于自己的空操作让给下一个。
///
/// 返回值与 [`World::apply`](crate::world::World::apply) 一致：几何可能因此变化的区块，交给
/// 流式层标脏。
pub trait CommandExecutor {
  fn execute(&mut self, command: &Command) -> Vec<ChunkPos>;
}

/// 命令总线：攒与派发。**不含执行器**（见模块文档）。
#[derive(Default)]
pub struct CommandBus {
  queued: Vec<Command>,
}

impl CommandBus {
  pub fn new() -> Self {
    Self::default()
  }

  /// 攒一条命令。帧内可以攒任意多条。
  pub fn enqueue(&mut self, command: Command) {
    self.queued.push(command);
  }

  /// 攒着几条。
  pub fn len(&self) -> usize {
    self.queued.len()
  }

  /// 有没有攒着的。
  pub fn is_empty(&self) -> bool {
    self.queued.is_empty()
  }

  /// 丢掉攒着的（不清执行器——总线里本来就没有）。
  pub fn clear(&mut self) {
    self.queued.clear();
  }

  /// 攒着的命令的一行一条形式，` | ` 相连——命令流的打印口径（M2 验收第 4 条）。
  pub fn describe(&self) -> String {
    self
      .queued
      .iter()
      .map(Command::to_string)
      .collect::<Vec<_>>()
      .join(" | ")
  }

  /// 帧边界派发：**按攒入顺序**逐条交给 `executors`（次序即优先级），收集要标脏的区块。
  ///
  /// 派发后队列清空；没有任何执行器认得命令也不报错（丢弃即可，命令是数据）。
  pub fn flush(&mut self, executors: &mut [&mut dyn CommandExecutor]) -> Vec<ChunkPos> {
    let mut dirty = Vec::new();
    let queued = std::mem::take(&mut self.queued);
    for command in &queued {
      let mut claimed = false;
      for executor in executors.iter_mut() {
        let touched = executor.execute(command);
        if !touched.is_empty() {
          dirty.extend(touched);
          claimed = true;
          break;
        }
      }
      tracing::trace!(command = %command, claimed, "派发命令");
    }
    dirty
  }
}

#[cfg(test)]
mod tests {
  use super::*;
  use crate::core::command::{EntityCommand, EntityId};
  use crate::world::ChunkPos;

  /// 记账执行器：只认一类命令，认领时返回固定区块。
  struct Recorder {
    /// 认领哪些命令（用 `Debug` 前缀判——测试里比字符串最省事）。
    accepts: &'static str,
    seen: Vec<String>,
  }

  impl CommandExecutor for Recorder {
    fn execute(&mut self, command: &Command) -> Vec<ChunkPos> {
      let text = command.to_string();
      if !text.starts_with(self.accepts) {
        return Vec::new();
      }
      self.seen.push(text);
      vec![ChunkPos::new(0, 0, 0)]
    }
  }

  /// 一条都不认的执行器：专门验「不认识就继续问下一个」。
  struct Refuser;

  impl CommandExecutor for Refuser {
    fn execute(&mut self, _command: &Command) -> Vec<ChunkPos> {
      Vec::new()
    }
  }

  #[test]
  fn the_bus_keeps_what_was_queued_until_flushed() {
    let mut bus = CommandBus::new();
    assert!(bus.is_empty());

    bus.enqueue(Command::Break { pos: [1, 2, 3] });
    bus.enqueue(Command::Entity(EntityCommand::Despawn { id: EntityId(1) }));

    assert_eq!(bus.len(), 2);
    assert_eq!(bus.describe(), "break (1, 2, 3) | despawn #1");

    let mut recorder = Recorder {
      accepts: "break",
      seen: Vec::new(),
    };
    let touched = bus.flush(&mut [&mut recorder]);

    assert_eq!(touched.len(), 1, "认领的那条返回一个待标脏区块");
    assert_eq!(recorder.seen, vec!["break (1, 2, 3)".to_owned()]);
    assert!(bus.is_empty(), "派发即清空");
  }

  /// 保序：命令按攒入顺序派发，执行器看到的次序就是这个次序（M2 验收第 4 条）。
  #[test]
  fn commands_are_dispatched_in_the_order_they_were_queued() {
    let mut bus = CommandBus::new();
    for z in 0..5 {
      bus.enqueue(Command::Place {
        pos: [0, 0, z],
        block: crate::world::block::BlockId::AIR,
      });
    }

    let mut recorder = Recorder {
      accepts: "place",
      seen: Vec::new(),
    };
    bus.flush(&mut [&mut recorder]);

    assert_eq!(
      recorder.seen,
      (0..5)
        .map(|z| format!("place (0, 0, {z}) block#0"))
        .collect::<Vec<_>>()
    );
  }

  /// 不认识的命令接着问下一个执行器；一条都不认得就安静丢弃。
  #[test]
  fn unrecognized_commands_fall_through_to_the_next_executor() {
    let mut bus = CommandBus::new();
    bus.enqueue(Command::Entity(EntityCommand::Spawn {
      id: EntityId(2),
      kind: 1,
      pos: [0.0, 0.0, 0.0],
    }));
    bus.enqueue(Command::Break { pos: [0, 0, 0] });

    let mut refuser = Refuser;
    let mut entity = Recorder {
      accepts: "spawn",
      seen: Vec::new(),
    };
    let touched = bus.flush(&mut [&mut refuser, &mut entity]);

    assert_eq!(entity.seen, vec!["spawn #2 kind=1 at (0, 0, 0)".to_owned()]);
    assert_eq!(touched.len(), 1, "只有实体那条被认领");

    // 没有执行器认得它：不报错，也没有待标脏区块。
    bus.enqueue(Command::Entity(EntityCommand::Despawn { id: EntityId(3) }));
    assert!(bus.flush(&mut [&mut refuser]).is_empty());
  }

  /// 认领即止：第一条认得的命令不会再去问后面的执行器（免得同一条命令被落地两次）。
  #[test]
  fn the_first_executor_that_claims_a_command_wins() {
    let mut bus = CommandBus::new();
    bus.enqueue(Command::Break { pos: [0, 0, 0] });

    let mut first = Recorder {
      accepts: "break",
      seen: Vec::new(),
    };
    let mut second = Recorder {
      accepts: "break",
      seen: Vec::new(),
    };
    bus.flush(&mut [&mut first, &mut second]);

    assert_eq!(first.seen.len(), 1, "第一条认领");
    assert!(second.seen.is_empty(), "认领之后不再问后面");
  }

  /// 空队列派发是安静的空操作。
  #[test]
  fn flushing_an_empty_bus_does_nothing() {
    let mut bus = CommandBus::new();
    let mut recorder = Recorder {
      accepts: "",
      seen: Vec::new(),
    };

    assert!(bus.flush(&mut [&mut recorder]).is_empty());
    assert!(recorder.seen.is_empty());

    bus.enqueue(Command::Break { pos: [0, 0, 0] });
    bus.clear();
    assert!(bus.is_empty());
  }
}
