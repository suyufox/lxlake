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
///
/// 两条契约，实现前先看清楚：
///
/// - **泵是「至少」这么勤**：任何一个源到期，运行时会泵掉**全部**源（见
///   `AppContext::pump_sources`）。于是 `pump` 必须廉价、且能被超频调用——同一帧里被多泵几次
///   也不能出错。
/// - **原生窗口不在 [`PumpContext`] 里**：wry / CEF 要的父窗口句柄由源在**构造时**自己拿着
///   （`AppContext::main_window` 给的是 `Arc<dyn WindowHandle>`），而不是由运行时每泵一次塞进来。
///   于是这个接口始终不认识窗口，也就不必为它加窗口类型的门。
pub trait EventSource: 'static {
  /// 下次必须被泵的时间点；`None` 表示只依赖帧边界被动泵。
  ///
  /// 返回的是**绝对**时间点。运行时取所有源的**最早**者与下一帧时间取小，据此设
  /// `ControlFlow::WaitUntil`——wry 与 CEF 约 ~10ms 的泵频率就落在这一条上。
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
