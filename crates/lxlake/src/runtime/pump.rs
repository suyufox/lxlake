//! 外部事件源与跨线程唤醒——**运行时在帧边界之外被唤醒的唯一入口**。
//!
//! webview 双线（wry 覆盖层 / CEF 纹理层）与将来任何异步能力都从这里接：它们要求在帧
//! 节奏之外被泵（wry 与 CEF 都约每 ~10ms 一次）。M0 只交接口，不含任何具体实现
//! （见 `docs/roadmap.md` M0）。

use std::time::Instant;

/// 跨线程唤醒句柄：任何线程拿它请求运行时「立刻醒一次」。
///
/// 由平台提供——winit 的 `EventLoopProxy::send_event`。平台若没有这个能力就返回 `None`,
/// 调用方降级为「等帧边界」（见 `docs/architecture.md` 任务与异步）。
pub trait Wakeup: Send + Sync + 'static {
  fn wake(&self);
}

/// 外部事件源：需要比帧节奏更细的泵频率的东西。
///
/// 运行时在帧边界、以及源自己声明的截止时间上调用 [`EventSource::pump`]。实现必须跑在
/// **主线程**上且**不得阻塞**——它花的正是帧边界的时间。
pub trait EventSource: 'static {
  /// 下次必须被泵的时间点；`None` 表示只依赖帧边界被动泵。
  fn next_deadline(&self) -> Option<Instant> {
    None
  }

  /// 泵一次；返回 `true` 表示产生了需要应用处理的东西（运行时会因此补一帧）。
  fn pump(&mut self, cx: &mut PumpContext<'_>) -> bool;
}

/// 泵上下文：交给事件源的可调用面。
pub struct PumpContext<'a> {
  /// 本次泵的时间点。
  pub now: Instant,
  wakeup: &'a dyn Wakeup,
}

impl<'a> PumpContext<'a> {
  pub(crate) fn new(now: Instant, wakeup: &'a dyn Wakeup) -> Self {
    Self { now, wakeup }
  }

  /// 请求运行时再醒一次——源在别的线程拿到结果时用它把结果推回主循环。
  pub fn wake(&self) {
    self.wakeup.wake();
  }
}
