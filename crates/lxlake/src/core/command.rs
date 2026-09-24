//! 命令与意图：**世界可变性的入口**，也是 M4 存档与将来联机的接入点。
//!
//! 两者分层的理由见 `docs/roadmap.md` 的「输入与模拟的分界」：
//!
//! ```text
//!   平台事件 → 键位表 → 意图 → 模拟层 → 命令 → 世界 → 重网格化
//!                       ↑ 本文件前半      ↑ 本文件后半
//! ```
//!
//! - **意图**与设备无关，也不知道世界坐标——将来 UI、脚本、回放都能产生意图
//! - **命令**已经解析出目标坐标，是**可序列化的数据**：能录制、回放、网络传输
//!
//! 混成一层的话，M4 存档与联机就得回头拆，所以现在就分开。
//!
//! 命令落在契约层（分层表见 `docs/architecture.md`），于是这里借了世界侧的 [`BlockId`] 来寻址
//! 方块。它是个 `u16` 包装、不含任何平台或 GPU 类型，契约层的硬约束不受影响。

use crate::world::block::BlockId;
use std::fmt;

/// 意图：设备无关、不含世界坐标。
///
/// 「按住有效」与「按下即触发」两类共用一个枚举：键位表是一张 `输入源 → 意图` 的表，拆成两个
/// 枚举只会让表跟着分叉（分类见 [`Intent::is_axis`]）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Intent {
  /// 前进（前后轴正向）。
  MoveForward,
  /// 后退。
  MoveBackward,
  /// 右移。
  MoveRight,
  /// 左移。
  MoveLeft,
  /// 上升。
  Ascend,
  /// 下降。
  Descend,
  /// 加速。
  Boost,
  /// 挖掉准星指着的那块。
  Break,
  /// 在准星指着的面上放一块。
  Place,
  /// 退出应用。
  Quit,
}

impl Intent {
  /// 是不是**轴意图**：按住期间持续有效，本身不产生命令。
  ///
  /// 轴意图只在输入层叠成轴值；动作意图按下即触发，由模拟层解析成命令。
  pub const fn is_axis(self) -> bool {
    matches!(
      self,
      Self::MoveForward
        | Self::MoveBackward
        | Self::MoveRight
        | Self::MoveLeft
        | Self::Ascend
        | Self::Descend
        | Self::Boost
    )
  }
}

/// 命令：已经解析出目标坐标的、**可序列化的数据**。
///
/// 执行器在世界侧（[`crate::world::World::apply`]）：契约层只管形状，不碰世界状态。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Command {
  /// 破坏：把 `pos` 上的方块挖成空气。
  Break { pos: [i32; 3] },
  /// 放置：在 `pos` 上放一块 `block`。
  Place { pos: [i32; 3], block: BlockId },
}

impl fmt::Display for Command {
  /// 一行一条的紧凑形式：命令序列打印出来就是这个样子（M2 验收第 4 条）。
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    match self {
      Self::Break { pos } => write!(f, "break ({}, {}, {})", pos[0], pos[1], pos[2]),
      Self::Place { pos, block } => {
        write!(f, "place ({}, {}, {}) {block}", pos[0], pos[1], pos[2])
      }
    }
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn axis_and_action_intents_are_separated() {
    assert!(Intent::MoveForward.is_axis());
    assert!(Intent::Boost.is_axis());
    for action in [Intent::Break, Intent::Place, Intent::Quit] {
      assert!(!action.is_axis(), "{action:?} 是动作意图");
    }
  }

  #[test]
  fn commands_print_as_one_line_each() {
    assert_eq!(
      Command::Break { pos: [1, -2, 3] }.to_string(),
      "break (1, -2, 3)"
    );
    assert_eq!(
      Command::Place {
        pos: [1, -2, 3],
        block: BlockId::AIR,
      }
      .to_string(),
      "place (1, -2, 3) block#0"
    );
  }
}
