//! 运行时：应用装配（[`Builder`]）、平台驱动契约（[`Application`]）、帧循环、外部事件源泵、
//! 作业调度。
//!
//! 两层应用对象，别混：
//!
//! - [`App`] —— **用户面向**的状态容器：托管状态 + [`AppContext`]。生命周期闭包拿到的是它。
//! - [`Application`] —— **平台面向**的内部契约，由 [`Builder`] 满足，应用侧不实现它。
//!
//! **等待权在本层与平台手里**：帧节奏由 [`FrameClock`] 定，外部事件源按自己声明的截止
//! 时间被泵。任何异步机制都不得与之争抢等待（见 `docs/architecture.md` 任务与异步）。

mod app;
mod builder;
mod clock;
mod command;
mod exec;
mod jobs;
mod log;
mod pump;
mod window;

pub use app::{App, AppContext, Capabilities};
pub use builder::{Builder, Plugin};
pub use clock::{Frame, FrameClock};
pub use command::{CommandBus, CommandExecutor};
pub use exec::{AsyncConfig, AsyncRuntime, Mailbox, MailboxSender};
pub use jobs::{JobContext, JobHandle, JobPool};
pub use log::{FileSink, LogConfig, LogError, Rotation};
pub use pump::{EventSource, PumpContext, Wakeup};
pub use window::{Window, WindowRegistry};

use crate::core::Error;
use crate::core::event::Event;
use crate::core::window::{WindowId, WindowLabel, WindowSpec};
use crate::platform;
use std::sync::Arc;
use std::time::Duration;

/// 默认目标帧率。
pub const DEFAULT_FPS: u32 = 60;

/// 平台驱动的**内部**契约：由 [`Builder`] 满足，应用侧不实现它。
///
/// 钩子全部在**主线程**上调用，顺序为：`attach_wakeup` → `windows` → `on_startup` →
///（`on_event` / `on_frame`）反复 → `on_shutdown`。窗口相关的两个钩子在建窗 / 摘窗的当口调用。
#[doc(hidden)]
pub trait Application: 'static {
  /// 注入跨线程唤醒句柄。
  ///
  /// 应用对象在事件循环**之前**就已构造，而唤醒句柄要等循环建好才有，所以必须补这一步。
  fn attach_wakeup(&mut self, wakeup: Arc<dyn Wakeup>);

  /// 运行时 / 平台要用的上下文。
  fn context_mut(&mut self) -> &mut AppContext;

  /// 应用标识。平台入口据此算应用目录（见 [`crate::path::Paths::for_app`]），
  /// 日志的默认落点也由它定。
  fn app_id(&self) -> &str;

  /// 日志配置。`None` = 不装全局订阅器（测试友好）。
  ///
  /// 平台入口在**装完路径之后**立刻拿它安装——默认落点在应用目录下，先装路径才解析得到
  /// （见 [`LogConfig`]）。取的是数据，安装动作在入口那一侧。
  fn log_config(&self) -> Option<&LogConfig>;

  /// 启动时要创建的窗口。标签为 `main` 的那个即主窗口。
  fn windows(&self) -> Vec<WindowSpec>;

  /// 目标帧间隔；`None` = 不限速。
  fn frame_interval(&self) -> Option<Duration>;

  /// 某个窗口刚建好。
  fn on_window_ready(&mut self, id: WindowId);

  /// 某个窗口被摘掉。
  fn on_window_destroyed(&mut self, id: WindowId);

  /// 窗口就绪、进入帧循环前调用一次。
  fn on_startup(&mut self);

  /// 运行时事件。
  fn on_event(&mut self, event: &Event);

  /// 帧边界。
  fn on_frame(&mut self, frame: Frame);

  /// 事件循环退出前调用一次。
  fn on_shutdown(&mut self);
}

/// 启动应用：先校验装配期配置（窗口标签），再把控制权交给平台后端的事件循环，返回时应用已退出。
pub fn run(app: impl Application) -> Result<(), Error> {
  validate_windows(&app.windows())?;
  platform::run(app)
}

/// 校验窗口声明：标签非空、互不重复，且保留标签 `main` 只归主窗口（列表首位那个）。
///
/// 放在这里而不是 `Builder` 里，是为了让**所有**入口（`Builder::run` 与直接
/// `runtime::run`）走同一道闸。
fn validate_windows(specs: &[WindowSpec]) -> Result<(), Error> {
  let mut seen: Vec<&str> = Vec::with_capacity(specs.len());
  for (index, spec) in specs.iter().enumerate() {
    let label = spec.label.as_str();
    if label.is_empty() {
      return Err(Error::Config("窗口标签不能为空".to_owned()));
    }
    if seen.contains(&label) {
      return Err(Error::Config(format!("窗口标签重复：{label}")));
    }
    if index > 0 && spec.label.is_main() {
      return Err(Error::Config(format!(
        "`{}` 是主窗口的保留标签（主窗口由 Builder::main_window 注入）",
        WindowLabel::MAIN
      )));
    }
    seen.push(label);
  }
  Ok(())
}

#[cfg(test)]
mod tests {
  use super::*;
  use crate::core::window::WindowDesc;

  fn label(name: &str) -> WindowSpec {
    WindowSpec {
      label: WindowLabel::new(name),
      desc: WindowDesc::default(),
    }
  }

  #[test]
  fn a_plain_main_window_passes() {
    assert!(validate_windows(&[label(WindowLabel::MAIN)]).is_ok());
    assert!(validate_windows(&[]).is_ok(), "无窗口应用也是合法的");
  }

  #[test]
  fn extra_windows_with_distinct_labels_pass() {
    let specs = [
      label(WindowLabel::MAIN),
      label("inspector"),
      label("assets"),
    ];

    assert!(validate_windows(&specs).is_ok());
  }

  /// 标签是取窗的钥匙，重复了就有一把钥匙打不开门。
  #[test]
  fn duplicate_labels_are_refused() {
    let specs = [
      label(WindowLabel::MAIN),
      label("inspector"),
      label("inspector"),
    ];

    let err = validate_windows(&specs).expect_err("重复标签要拒");
    assert!(matches!(err, Error::Config(_)));
    assert!(
      err.to_string().contains("inspector"),
      "报错要点名是哪个标签"
    );
  }

  /// `main` 是主窗口的保留标签：`create_window("main", ..)` 造出来的窗不是主窗口，
  /// 再让它冒充就会让「主窗口」有两个答案。
  #[test]
  fn the_reserved_label_is_refused_for_extra_windows() {
    let specs = [label("first"), label(WindowLabel::MAIN)];

    assert!(matches!(validate_windows(&specs), Err(Error::Config(_)),));
  }

  #[test]
  fn an_empty_label_is_refused() {
    assert!(matches!(
      validate_windows(&[label("")]),
      Err(Error::Config(_))
    ));
  }
}
