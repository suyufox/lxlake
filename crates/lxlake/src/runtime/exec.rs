//! 异步运行时与帧边界邮箱——**I/O 与等待**的归属地。
//!
//! 分工（见 `docs/architecture.md` 任务与异步）：
//!
//! - CPU 密集、数据并行的活归作业池（[`JobPool`](super::JobPool)）：poll 式句柄，主线程在帧边界收
//! - 等待型（将来还有网络 / 资源下载）的活归这里：future 跑在 tokio 上，结果经 [`Mailbox`] 推回来
//! - **等待权不在这层**：tokio 跑在专用宿主线程上，主线程只在帧边界排空邮箱。任何异步机制都
//!   不得与帧循环争抢等待——`winit` 的 `ControlFlow` 仍由帧时钟与事件源决定。
//!
//! 宿主线程而不是主线程持有 tokio：`Runtime` 的收尾（关停 worker、等 blocking 任务）不该落在
//! 事件循环线程上——主线程要的是「立刻回到帧循环」。

use crate::core::Error;
use crate::runtime::Wakeup;
use std::collections::VecDeque;
use std::future::Future;
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::Duration;

/// 收尾时等 blocking 任务的上限：等不到就放掉，不能把进程退出拖住。
const SHUTDOWN_TIMEOUT: Duration = Duration::from_millis(500);

/// 异步运行时的配置。
#[derive(Debug, Clone)]
pub struct AsyncConfig {
  /// 宿主线程名（也是 tokio worker 线程名的前缀，便于在调试器里分辨）。
  pub thread_name: String,
  /// tokio worker 线程数。
  pub worker_threads: usize,
}

impl Default for AsyncConfig {
  fn default() -> Self {
    Self {
      thread_name: "lxlake-async".to_owned(),
      worker_threads: 2,
    }
  }
}

impl AsyncConfig {
  pub fn new() -> Self {
    Self::default()
  }

  pub fn thread_name(mut self, name: impl Into<String>) -> Self {
    self.thread_name = name.into();
    self
  }

  pub fn threads(mut self, workers: usize) -> Self {
    self.worker_threads = workers.max(1);
    self
  }
}

/// 异步运行时：一条专用宿主线程持有 tokio，主线程只留一个 `Handle`。
///
/// 用 [`AsyncRuntime::spawn`] 提交 future；不想要了就 [`AsyncRuntime::shutdown`]（`Drop` 也会关）。
pub struct AsyncRuntime {
  handle: tokio::runtime::Handle,
  /// 停止信号：宿主线程收下它就关停 runtime 退出。`shutdown` 幂等。
  stop: std::sync::mpsc::Sender<()>,
  /// 宿主线程句柄；`Drop` 时 join，确保线程不悬着。
  host: Mutex<Option<JoinHandle<()>>>,
}

impl AsyncRuntime {
  /// 起一条宿主线程并在其上建 tokio multi-thread runtime。
  ///
  /// 建不起来（线程创建失败 / runtime 构建失败）返回 `Err`——调用方报一声即可，不该挡住启动。
  pub fn new(config: AsyncConfig) -> Result<Self, Error> {
    let workers = config.worker_threads.max(1);
    let (handle_tx, handle_rx) =
      std::sync::mpsc::channel::<Result<tokio::runtime::Handle, String>>();
    let (stop_tx, stop_rx) = std::sync::mpsc::channel::<()>();
    let worker_name = format!("{}-worker", config.thread_name);

    let host = std::thread::Builder::new()
      .name(config.thread_name.clone())
      .spawn(move || {
        let runtime = tokio::runtime::Builder::new_multi_thread()
          .worker_threads(workers)
          .thread_name(worker_name)
          // 只开 time 驱动：net / io 特性没进（见 Cargo.toml），要用的 capability 自己加。
          .enable_time()
          .build();
        let runtime = match runtime {
          Ok(runtime) => runtime,
          Err(error) => {
            let _ = handle_tx.send(Err(error.to_string()));
            return;
          }
        };
        if handle_tx.send(Ok(runtime.handle().clone())).is_err() {
          // 主线程已经不想要了（`new` 中途失败），别留着 runtime 空跑。
          return;
        }
        // 阻塞在这里替主线程持有 runtime：`recv` 一返回就关停并放掉它。
        let _ = stop_rx.recv();
        runtime.shutdown_timeout(SHUTDOWN_TIMEOUT);
      })
      .map_err(|error| Error::Platform(format!("创建异步宿主线程失败：{error}")))?;

    let handle = match handle_rx.recv() {
      Ok(Ok(handle)) => handle,
      Ok(Err(reason)) => {
        let _ = stop_tx.send(());
        let _ = host.join();
        return Err(Error::Platform(format!("创建异步运行时失败：{reason}")));
      }
      Err(_) => {
        let _ = host.join();
        return Err(Error::Platform("异步宿主线程未能启动".to_owned()));
      }
    };

    Ok(Self {
      handle,
      stop: stop_tx,
      host: Mutex::new(Some(host)),
    })
  }

  /// 提交一个 future 到 tokio 上跑（`Output` 固定为 `()`）。
  ///
  /// 结果请经 [`Mailbox`] 推回主线程——主线程只在帧边界排空邮箱，future 自己不碰世界状态。
  pub fn spawn<F>(&self, future: F)
  where
    F: Future<Output = ()> + Send + 'static,
  {
    self.handle.spawn(async move {
      tracing::trace!("异步任务开始");
      future.await;
      tracing::trace!("异步任务结束");
    });
  }

  /// 请求关停（幂等）：宿主线程关掉 runtime 后退出。
  ///
  /// 也已经可以走 `Drop`；显式调它的场合是「想在 `on_shutdown` 里先把异步停干净」。
  pub fn shutdown(&self) {
    let _ = self.stop.send(());
  }
}

impl Drop for AsyncRuntime {
  fn drop(&mut self) {
    self.shutdown();
    let host = self.host.lock().unwrap_or_else(|e| e.into_inner()).take();
    if let Some(host) = host {
      let _ = host.join();
    }
  }
}

/// 帧边界邮箱：**别的线程灌、主线程在帧边界排空**——异步结果回主循环的通道。
///
/// 形状是「`Arc<Mutex<VecDeque>>` + 唤醒句柄」：灌的一方拿 [`MailboxSender`]（可克隆、可挪去
/// 任何线程），排空的一方拿 [`Mailbox`] 本体（托管在 [`App`](super::App) 里）。
pub struct Mailbox<T> {
  queue: Arc<Mutex<VecDeque<T>>>,
}

impl<T> Default for Mailbox<T> {
  fn default() -> Self {
    Self::new()
  }
}

impl<T> Mailbox<T> {
  pub fn new() -> Self {
    Self {
      queue: Arc::new(Mutex::new(VecDeque::new())),
    }
  }

  /// 造一个灌入口；`wakeup` 是打断主循环等待的句柄（`App::wakeup()` 那份）。
  pub fn sender(&self, wakeup: Arc<dyn Wakeup>) -> MailboxSender<T> {
    MailboxSender {
      queue: Arc::clone(&self.queue),
      wakeup,
    }
  }

  /// 排空积压，按灌入顺序交出**拥有所有权**的迭代器（队列同时被腾空）。
  ///
  /// 拿到的迭代器不借用邮箱——排空之后可以立刻接着用 `App` 的其它部分（改世界状态、出画）。
  pub fn drain(&mut self) -> std::collections::vec_deque::IntoIter<T> {
    let taken = std::mem::take(&mut *self.queue.lock().unwrap_or_else(|e| e.into_inner()));
    taken.into_iter()
  }

  /// 是否还有积压。
  pub fn is_empty(&self) -> bool {
    self
      .queue
      .lock()
      .unwrap_or_else(|e| e.into_inner())
      .is_empty()
  }

  /// 积压条数。
  pub fn len(&self) -> usize {
    self.queue.lock().unwrap_or_else(|e| e.into_inner()).len()
  }
}

/// 邮箱的灌入口：克隆只是多一个句柄，队列仍只有一条。
pub struct MailboxSender<T> {
  queue: Arc<Mutex<VecDeque<T>>>,
  wakeup: Arc<dyn Wakeup>,
}

impl<T> MailboxSender<T> {
  /// 灌一条结果并唤醒主循环。**先入队再唤醒**：被唤醒的一方一定能看到这条。
  pub fn send(&self, value: T) {
    self
      .queue
      .lock()
      .unwrap_or_else(|e| e.into_inner())
      .push_back(value);
    self.wakeup.wake();
  }
}

impl<T> Clone for MailboxSender<T> {
  fn clone(&self) -> Self {
    Self {
      queue: Arc::clone(&self.queue),
      wakeup: Arc::clone(&self.wakeup),
    }
  }
}

#[cfg(test)]
mod tests {
  use super::*;
  use std::sync::atomic::{AtomicUsize, Ordering};

  #[derive(Default)]
  struct CountingWakeup(AtomicUsize);

  impl Wakeup for CountingWakeup {
    fn wake(&self) {
      self.0.fetch_add(1, Ordering::SeqCst);
    }
  }

  fn wakeup() -> Arc<CountingWakeup> {
    Arc::new(CountingWakeup::default())
  }

  /// 灌进去的在帧边界能按顺序排空；排空后队列是空的，第二次排空空手而归。
  #[test]
  fn a_mailbox_hands_out_what_was_sent_in_order() {
    let mut mailbox = Mailbox::<u32>::new();
    let counter = wakeup();
    let sender = mailbox.sender(Arc::clone(&counter) as Arc<dyn Wakeup>);

    sender.send(1);
    sender.send(2);
    assert_eq!(mailbox.len(), 2);
    assert!(!mailbox.is_empty());

    assert_eq!(mailbox.drain().collect::<Vec<_>>(), vec![1, 2]);
    assert!(mailbox.is_empty());
    assert_eq!(mailbox.drain().count(), 0);
    assert_eq!(counter.0.load(Ordering::SeqCst), 2, "每条都唤一次");
  }

  /// 排空交出的迭代器不借用邮箱：拿着它还能继续动邮箱（帧里接着取别的能力就靠这条）。
  #[test]
  fn a_drained_iterator_does_not_hold_the_mailbox() {
    let mut mailbox = Mailbox::<String>::new();
    let sender = mailbox.sender(wakeup() as Arc<dyn Wakeup>);
    sender.send("a".to_owned());

    let drained = mailbox.drain();
    sender.send("b".to_owned());
    assert_eq!(mailbox.len(), 1, "排空之后灌进来的留在队列里");

    assert_eq!(drained.collect::<Vec<_>>(), vec!["a".to_owned()]);
  }

  /// 克隆的灌入口共用同一条队列（异步任务常常一人一个克隆）。
  #[test]
  fn cloned_senders_share_the_queue() {
    let mut mailbox = Mailbox::<u32>::new();
    let sender = mailbox.sender(wakeup() as Arc<dyn Wakeup>);
    let clone = sender.clone();

    clone.send(7);
    assert_eq!(mailbox.drain().collect::<Vec<_>>(), vec![7]);
  }

  /// 配置的默认值：名字取自框架，线程数不为零。
  #[test]
  fn the_async_config_has_a_usable_default() {
    let config = AsyncConfig::new();

    assert_eq!(config.thread_name, "lxlake-async");
    assert_eq!(config.worker_threads, 2);
    assert_eq!(AsyncConfig::new().threads(0).worker_threads, 1, "至少一条");
  }

  /// 提交的 future 跑在**异步线程**上（不是主线程），并且真能 await 计时器——`time` 驱动在。
  #[test]
  fn a_task_runs_off_the_main_thread_with_time_enabled() {
    let runtime = AsyncRuntime::new(AsyncConfig::new().threads(1)).expect("宿主线程起得来");
    let (tx, rx) = std::sync::mpsc::channel();

    runtime.spawn(async move {
      let name = std::thread::current().name().unwrap_or_default().to_owned();
      tokio::time::sleep(Duration::from_millis(1)).await;
      let _ = tx.send(name);
    });

    let name = rx
      .recv_timeout(Duration::from_secs(5))
      .expect("任务该在 5 秒内跑完");
    assert!(
      name.starts_with("lxlake-async"),
      "该跑在异步线程上，实际是 {name:?}"
    );

    runtime.shutdown();
  }

  /// 关停是幂等的：重复调、以及紧随其后的 `Drop` 都不该炸。
  #[test]
  fn shutdown_is_idempotent() {
    let runtime = AsyncRuntime::new(AsyncConfig::new().threads(1)).expect("宿主线程起得来");

    runtime.shutdown();
    runtime.shutdown();
  }

  /// 邮箱与异步运行时合起来用——异步结果经邮箱回主线程，这就是帧边界的完整回路。
  #[test]
  fn a_task_can_push_a_result_through_a_mailbox() {
    let runtime = AsyncRuntime::new(AsyncConfig::new().threads(1)).expect("宿主线程起得来");
    let mut mailbox = Mailbox::<u32>::new();
    let counter = wakeup();
    let sender = mailbox.sender(Arc::clone(&counter) as Arc<dyn Wakeup>);

    runtime.spawn(async move {
      sender.send(42);
    });

    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    while mailbox.is_empty() && std::time::Instant::now() < deadline {
      std::thread::sleep(Duration::from_millis(1));
    }

    assert_eq!(mailbox.drain().collect::<Vec<_>>(), vec![42]);
    assert_eq!(counter.0.load(Ordering::SeqCst), 1, "灌一条唤一次");
  }
}
