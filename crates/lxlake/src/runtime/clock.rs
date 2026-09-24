//! 帧节奏器：把「目标帧间隔」变成「下一帧的绝对时间点」，带漂移补偿。

use std::time::{Duration, Instant};

/// 一帧的时间信息。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Frame {
  /// 帧序号，从 0 开始。
  pub index: u64,
  /// 距上一帧的实际间隔。
  pub delta: Duration,
  /// 自启动以来的时长。
  pub elapsed: Duration,
}

/// 帧节奏器。
///
/// winit 没有内建 vsync，所以节流必须自己拿。渲染落地后 vsync 由 wgpu 的 present mode
/// （`Fifo`）负责，本节奏器负责的是**上限**——不让循环跑到几百帧空转，两者不冲突。
pub struct FrameClock {
  /// 目标帧间隔；`None` = 不限速（交给 present mode 节流）。
  target: Option<Duration>,
  start: Instant,
  last: Instant,
  index: u64,
  /// 下一帧应当发生的绝对时间点。
  next: Instant,
}

impl FrameClock {
  pub fn new(target: Option<Duration>, now: Instant) -> Self {
    Self {
      target,
      start: now,
      last: now,
      index: 0,
      next: now,
    }
  }

  /// 帧是否到期。
  pub fn is_due(&self, now: Instant) -> bool {
    self.target.is_none() || now >= self.next
  }

  /// 下一帧到期的绝对时间点；`None` = 不限速。
  pub fn next_due(&self) -> Option<Instant> {
    self.target.map(|_| self.next)
  }

  /// 推进一帧，返回本帧的时间信息。
  pub fn advance(&mut self, now: Instant) -> Frame {
    let frame = Frame {
      index: self.index,
      delta: now.saturating_duration_since(self.last),
      elapsed: now.saturating_duration_since(self.start),
    };

    self.index += 1;
    self.last = now;

    if let Some(target) = self.target {
      // 累加而非「now + target」：单帧耗时不会被吃进下一帧，长时间跑不会累积漂移。
      self.next += target;
      // 但落后超过一整帧（被外部事件拖住）就重置——补帧会把一次卡顿放大成连发。
      if self.next <= now {
        self.next = now + target;
      }
    }

    frame
  }
}
