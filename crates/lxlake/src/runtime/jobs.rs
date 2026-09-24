//! 作业系统：工作窃取线程池 + poll 式句柄——**CPU 密集、数据并行**作业的归属地。
//!
//! 口径（见 `docs/architecture.md` 任务与异步）：
//!
//! - 主线程只在**帧边界**收结果：`JobPool` 不回调应用，应用在 `on_frame` 里 poll 句柄
//! - 作业完成时经 [`Wakeup`] 把主循环的等待打断；结果本身仍由主线程取
//! - 工作线程不碰世界状态：世界以不可变 `Arc<Chunk>` 快照交进作业（见「世界与区块」）
//! - 线程数由本层**统一持有**（不引 rayon 一类全局池），渲染线程 / 作业线程 / 主线程的
//!   核数分配都从这里出
//!
//! 池的形状是**全局注入队列 + 每 worker 一条本地队列**：`spawn` 一律推到注入队列，空闲
//! worker 先看本地、再看注入、最后去别的 worker 那里**成批**偷。本层不做作业内再提交，
//! 所以本地队列只由「偷」填充——批量偷正好把一次提交摊到多个核上。

use crate::runtime::Wakeup;
use crossbeam_deque::{Injector, Steal, Stealer, Worker};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::Duration;

/// 空闲等待的超时兜底：正常路径靠 [`Condvar`] 即时唤醒，超时只在极端竞态下兜底。
const IDLE_TIMEOUT: Duration = Duration::from_millis(100);

/// 作业池句柄。
///
/// 池本体在 `Arc` 里，克隆只是多一个句柄——池可以被应用、流式层共用，线程数仍只有一份。
#[derive(Clone)]
pub struct JobPool {
  inner: Arc<PoolInner>,
}

struct PoolInner {
  /// worker 线程数。
  workers: usize,
  /// 池的共享状态（队列、计数器、唤醒）。
  shared: Arc<Shared>,
  /// worker 线程句柄：`Drop` 时停线程并 join。
  threads: Mutex<Vec<std::thread::JoinHandle<()>>>,
}

/// 池内所有 worker 共用的状态。**不含任何作业数据**——只有队列与计数。
struct Shared {
  /// 全局注入队列：`spawn` 的唯一入口。
  injector: Injector<JobTask>,
  /// 每个 worker 的本地队列出口，供互相偷。
  stealers: Vec<Stealer<JobTask>>,
  /// 未完成作业数。归零 = 这一批收工，见 [`Shared::complete`]。
  pending: AtomicUsize,
  /// 停池标志：置位后 worker 取不到活就退出。
  shutdown: AtomicBool,
  /// 完成时唤醒主循环。**合并唤醒**：只在 `pending` 归零那一次调用，worker 打不爆主循环。
  wakeup: Arc<dyn Wakeup>,
  /// 空闲等待的临界区：`spawn` 持它入队，worker 持它检查空 → 入睡，杜绝丢唤醒。
  idle_lock: Mutex<()>,
  idle_cv: Condvar,
}

/// 一个待跑的作业：**类型已擦除**，队列只认「一个闭包 + 取消标志」。
struct JobTask {
  /// 与 [`JobHandle`] 共享的取消标志。
  cancelled: Arc<AtomicBool>,
  /// 作业本体；[`JobContext`] 由 worker 在跑之前现造。
  run: Box<dyn FnOnce(&JobContext<'_>) + Send + 'static>,
}

impl Shared {
  /// 取一个待跑的作业：本地 → 注入队列 → 别的 worker。
  ///
  /// 偷是**成批**的（`steal_batch_and_pop`）：一次提交往往是一串同形作业，成批搬走能立刻
  /// 摊到多个核上，省掉逐条偷的往返。
  fn find_task(&self, index: usize, worker: &Worker<JobTask>) -> Option<JobTask> {
    if let Some(task) = worker.pop() {
      return Some(task);
    }

    loop {
      match self.injector.steal_batch_and_pop(worker) {
        Steal::Success(task) => return Some(task),
        Steal::Retry => continue,
        Steal::Empty => break,
      }
    }

    for (i, stealer) in self.stealers.iter().enumerate() {
      if i == index {
        continue;
      }
      loop {
        match stealer.steal_batch_and_pop(worker) {
          Steal::Success(task) => return Some(task),
          Steal::Retry => continue,
          Steal::Empty => break,
        }
      }
    }

    None
  }

  /// 报一个作业完成。**合并唤醒**：`pending` 归零（这一批全部收工）才去打断主循环。
  fn complete(&self) {
    let previous = self.pending.fetch_sub(1, Ordering::AcqRel);
    tracing::trace!(pending = previous - 1, "作业完成");
    if previous == 1 {
      self.wakeup.wake();
    }
  }
}

impl JobPool {
  /// 按默认 worker 数建池。
  pub fn new(wakeup: Arc<dyn Wakeup>) -> Self {
    Self::with_workers(default_workers(), wakeup)
  }

  /// 指定 worker 数建池（测试与手工调参用）。建池即拉起线程。
  pub fn with_workers(workers: usize, wakeup: Arc<dyn Wakeup>) -> Self {
    let workers = workers.max(1);

    // 先把各 worker 的本地队列建出来，才能把它们互相的偷入口收进共享状态。
    let locals: Vec<Worker<JobTask>> = (0..workers).map(|_| Worker::new_fifo()).collect();
    let stealers = locals.iter().map(Worker::stealer).collect();

    let shared = Arc::new(Shared {
      injector: Injector::new(),
      stealers,
      pending: AtomicUsize::new(0),
      shutdown: AtomicBool::new(false),
      wakeup,
      idle_lock: Mutex::new(()),
      idle_cv: Condvar::new(),
    });

    let threads = locals
      .into_iter()
      .enumerate()
      .map(|(index, worker)| {
        let shared = Arc::clone(&shared);
        std::thread::Builder::new()
          .name(format!("lxlake-job-{index}"))
          .spawn(move || worker_loop(index, worker, shared))
          .expect("创建作业线程失败")
      })
      .collect();

    Self {
      inner: Arc::new(PoolInner {
        workers,
        shared,
        threads: Mutex::new(threads),
      }),
    }
  }

  /// worker 线程数。
  pub fn worker_count(&self) -> usize {
    self.inner.workers
  }

  /// 提交一个作业，**立刻**返回 poll 式句柄；作业在 worker 线程上跑。
  ///
  /// 作业拿到的是只读环境（[`JobContext`]）与它自己的输入副本——它没有任何途径摸到
  /// 世界本体，这是「工作线程不碰世界状态」的落实方式。
  pub fn spawn<J, T>(&self, job: J) -> JobHandle<T>
  where
    J: FnOnce(&JobContext<'_>) -> T + Send + 'static,
    T: Send + 'static,
  {
    let shared = &self.inner.shared;
    let cancelled = Arc::new(AtomicBool::new(false));
    let result = Arc::new(Mutex::new(None));

    let task_result = Arc::clone(&result);
    let run: Box<dyn FnOnce(&JobContext<'_>) + Send + 'static> =
      Box::new(move |cx: &JobContext<'_>| {
        let value = job(cx);
        *task_result.lock().unwrap_or_else(|e| e.into_inner()) = Some(value);
      });

    let task = JobTask {
      cancelled: Arc::clone(&cancelled),
      run,
    };

    // 入队与「worker 检查空 → 入睡」共用一把锁：入队先占锁，worker 睡下前也占锁检查，
    // 因此不会出现「worker 判定没活、睡下去、作业刚好在睡之前推入」的丢唤醒。
    shared.pending.fetch_add(1, Ordering::AcqRel);
    {
      let _guard = shared.idle_lock.lock().unwrap_or_else(|e| e.into_inner());
      shared.injector.push(task);
    }
    shared.idle_cv.notify_one();
    tracing::trace!(workers = self.inner.workers, "提交作业");

    JobHandle { cancelled, result }
  }
}

impl Drop for PoolInner {
  fn drop(&mut self) {
    self.shared.shutdown.store(true, Ordering::Release);
    self.shared.idle_cv.notify_all();

    let threads = self
      .threads
      .lock()
      .unwrap_or_else(|e| e.into_inner())
      .drain(..)
      .collect::<Vec<_>>();
    for handle in threads {
      let _ = handle.join();
    }
  }
}

/// worker 主循环：取活 → 跑 → 报完成；取不到就等在注入队列上。
fn worker_loop(index: usize, worker: Worker<JobTask>, shared: Arc<Shared>) {
  loop {
    // 快路：不上锁直接找活。
    if let Some(task) = shared.find_task(index, &worker) {
      run_task(&shared, task);
      continue;
    }

    if shared.shutdown.load(Ordering::Acquire) {
      break;
    }

    let guard = shared.idle_lock.lock().unwrap_or_else(|e| e.into_inner());
    // 双重检查：新作业可能刚好在「找到没活」与「拿到锁」之间入队。取到活就把锁放下再跑——
    // 这里取到的**必须原样跑掉**，丢掉它等于吞掉一个作业。
    if shared.shutdown.load(Ordering::Acquire) {
      break;
    }
    if let Some(task) = shared.find_task(index, &worker) {
      drop(guard);
      run_task(&shared, task);
      continue;
    }
    let _ = shared.idle_cv.wait_timeout(guard, IDLE_TIMEOUT);
  }
}

/// 跑一个作业并报完成。
fn run_task(shared: &Shared, task: JobTask) {
  let JobTask { cancelled, run } = task;
  let cx = JobContext {
    cancelled: &cancelled,
  };
  run(&cx);
  shared.complete();
}

/// poll 式作业句柄。
///
/// 结果**只能取一次**：取走即释放。句柄可以一直握着不 poll（表示「作业在跑，我还不想要
/// 结果」），丢弃句柄不会取消作业——要取消得显式 [`JobHandle::cancel`]。
pub struct JobHandle<T> {
  /// 协作式取消标志：主线程置位，作业在 [`JobContext::is_cancelled`] 里看到。
  cancelled: Arc<AtomicBool>,
  /// 结果槽：`None` = 未完成或已被取走。
  result: Arc<Mutex<Option<T>>>,
}

impl<T> JobHandle<T> {
  /// 取结果；未完成或已取过则返回 `None`。**非阻塞**。
  pub fn poll(&self) -> Option<T> {
    self.result.lock().unwrap_or_else(|e| e.into_inner()).take()
  }

  /// 结果是否已就绪。
  pub fn is_finished(&self) -> bool {
    self
      .result
      .lock()
      .unwrap_or_else(|e| e.into_inner())
      .is_some()
  }

  /// 请求取消。作业需自己在长循环里定期查 [`JobContext::is_cancelled`]——**协作式**，
  /// 池不会强杀线程（强杀会让 `Arc<Chunk>` 一类的共享状态处于半改状态）。
  pub fn cancel(&self) {
    self.cancelled.store(true, Ordering::Relaxed);
  }
}

/// 作业上下文：交给作业的只读环境。
pub struct JobContext<'a> {
  cancelled: &'a AtomicBool,
}

impl<'a> JobContext<'a> {
  /// 是否已被请求取消。
  pub fn is_cancelled(&self) -> bool {
    self.cancelled.load(Ordering::Relaxed)
  }
}

/// 默认 worker 数：逻辑核数 − 1（留一个给主线程），至少 1。
fn default_workers() -> usize {
  std::thread::available_parallelism()
    .map_or(1, |parallelism| parallelism.get().saturating_sub(1))
    .max(1)
}

#[cfg(test)]
mod tests {
  use super::*;
  use std::sync::atomic::AtomicUsize;

  /// 不做任何事的唤醒：单测只关心结果，不关心主循环是否被打断。
  struct NullWakeup;

  impl Wakeup for NullWakeup {
    fn wake(&self) {}
  }

  /// 数唤醒次数。
  struct CountWakeup(AtomicUsize);

  impl Wakeup for CountWakeup {
    fn wake(&self) {
      self.0.fetch_add(1, Ordering::Relaxed);
    }
  }

  fn null_pool(workers: usize) -> JobPool {
    JobPool::with_workers(workers, Arc::new(NullWakeup))
  }

  /// 轮询等结果，超时即失败——作业系统本身是异步的，单测只做有界等待。
  fn wait_for<T>(handle: &JobHandle<T>) -> T {
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    loop {
      if let Some(value) = handle.poll() {
        return value;
      }
      assert!(
        std::time::Instant::now() < deadline,
        "作业超时未完成（10s）"
      );
      std::thread::sleep(Duration::from_millis(1));
    }
  }

  #[test]
  fn worker_count_is_at_least_one() {
    assert_eq!(
      JobPool::with_workers(0, Arc::new(NullWakeup)).worker_count(),
      1
    );
    assert_eq!(null_pool(4).worker_count(), 4);
  }

  #[test]
  fn collects_all_results() {
    let pool = null_pool(4);
    let handles = (0..64u64)
      .map(|n| pool.spawn(move |_cx: &JobContext<'_>| (n, (0..1000u64).sum::<u64>() + n)))
      .collect::<Vec<_>>();

    for (n, handle) in handles.iter().enumerate() {
      let (from_job, sum) = wait_for(handle);
      assert_eq!(from_job, n as u64);
      assert_eq!(sum, (0..1000u64).sum::<u64>() + n as u64);
    }
  }

  #[test]
  fn result_is_taken_once() {
    let pool = null_pool(2);
    let handle = pool.spawn(|_cx: &JobContext<'_>| 7u32);
    assert_eq!(wait_for(&handle), 7);
    assert_eq!(handle.poll(), None, "结果只能取一次");
    assert!(!handle.is_finished(), "取走后不再是「已就绪」");
  }

  #[test]
  fn cancel_is_seen_by_job() {
    let pool = null_pool(2);
    let handle = pool.spawn(|cx: &JobContext<'_>| {
      while !cx.is_cancelled() {
        std::hint::spin_loop();
      }
      42u32
    });

    handle.cancel();
    assert_eq!(wait_for(&handle), 42);
  }

  /// 回归：worker 入睡前的双重检查曾把「刚取到的作业」直接丢掉（`is_some()` 用过即弃），
  /// 表现为作业凭空消失、句柄永远等不到结果。分批反复提交就是为了撞上那个窗口。
  #[test]
  fn repeated_batches_all_complete() {
    let pool = null_pool(4);
    for round in 0..64u32 {
      let handles = (0..8u32)
        .map(|n| pool.spawn(move |_cx: &JobContext<'_>| n))
        .collect::<Vec<_>>();

      for (n, handle) in handles.iter().enumerate() {
        assert_eq!(wait_for(handle), n as u32, "第 {round} 批少了一个作业");
      }
    }
  }

  #[test]
  fn wakeup_is_coalesced_per_batch() {
    let wakeup = Arc::new(CountWakeup(AtomicUsize::new(0)));
    let pool = JobPool::with_workers(4, Arc::clone(&wakeup) as Arc<dyn Wakeup>);

    // 一批 32 个作业：全部收工才唤醒一次，不能一到就响。
    let handles = (0..32u32)
      .map(|n| pool.spawn(move |_cx: &JobContext<'_>| n * 2))
      .collect::<Vec<_>>();
    let sum = handles.iter().map(wait_for).sum::<u32>();
    assert_eq!(sum, (0..32u32).map(|n| n * 2).sum::<u32>());

    assert!(
      wakeup.0.load(Ordering::Relaxed) <= 2,
      "合并唤醒：一批作业不该把主循环打好几次"
    );
  }
}
