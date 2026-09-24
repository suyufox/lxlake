//! 日志：**数据**（[`LogConfig`] / [`FileSink`] / [`Rotation`]）与**安装**
//! （[`LogConfig::install`]）分开。
//!
//! 分开的理由与装配期同一口径：数据是应用声明的一部分，随 [`Builder`](super::Builder) 一直
//! 走到运行期起点；安装则必须在**路径装好之后**发生（默认落点 `paths().logs` 要先算得出来）。
//! 因此 [`Application::log_config`](super::Application::log_config) 把数据交给平台入口，
//! 由入口在 `path::install` 之后立刻安装——建窗与建渲染后端阶段的日志也能被捕获。
//!
//! 应用侧**不写日志初始化**：要么用框架内封装（[`LogConfig`] 的默认值就已经落盘 + 上控制台），
//! 要么把 `LogConfig` 的公开字段改成自己要的。
//!
//! 文件写入不经非阻塞缓冲（`Mutex<File>` / `Mutex<RollingFileAppender>` 自身就带缓冲），
//! 于是**不需要 `WorkerGuard`**，也就没有「守卫必须活到进程退出」这个坑。

use crate::path::{self, BaseDirectory};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use tracing::subscriber::Subscriber;
use tracing_appender::rolling::{RollingFileAppender, Rotation as AppenderRotation};
use tracing_subscriber::EnvFilter;
use tracing_subscriber::fmt::layer;
use tracing_subscriber::prelude::*;
use tracing_subscriber::util::SubscriberInitExt;

/// `FileSink::Fixed { path: None }` 时落在 `paths().logs` 下的文件名。
pub const DEFAULT_FILE_NAME: &str = "lxlake.log";

/// 轮转周期。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Rotation {
  /// 不轮转：一直写同一个文件。
  Never,
  Minutely,
  Hourly,
  #[default]
  Daily,
}

impl Rotation {
  /// 契约侧周期 → `tracing-appender` 的周期。
  fn to_appender(self) -> AppenderRotation {
    match self {
      Rotation::Never => AppenderRotation::NEVER,
      Rotation::Minutely => AppenderRotation::MINUTELY,
      Rotation::Hourly => AppenderRotation::HOURLY,
      Rotation::Daily => AppenderRotation::DAILY,
    }
  }
}

/// 日志文件去哪。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FileSink {
  /// 不落盘，只上控制台。
  Disabled,
  /// 固定单文件，append 模式（多次运行累加，崩溃前的那一段留着）。
  ///
  /// `path = None` → `paths().logs` 下的 [`DEFAULT_FILE_NAME`]。
  Fixed { path: Option<PathBuf> },
  /// 在 `dir` 下按 `前缀_时间戳.后缀` 生成，按 `rotation` 轮转；`max_files` 限制保留份数。
  Rolling {
    dir: PathBuf,
    prefix: String,
    suffix: String,
    rotation: Rotation,
    max_files: Option<usize>,
  },
}

/// 日志配置。字段公开——外部可以直接构造或改字段；链式方法只是常用改法的语法糖。
#[derive(Debug, Clone)]
pub struct LogConfig {
  /// 过滤器指令（`EnvFilter` 语法）。`None` → 读 `RUST_LOG` → 都没有就用 `info`。
  pub directives: Option<String>,
  /// 是否上控制台（stdout）。
  pub to_stdout: bool,
  /// 文件去向。
  pub file: FileSink,
}

impl Default for LogConfig {
  /// 上控制台 + 落 `paths().logs/lxlake.log`——GUI 应用常常没有控制台，两边都给上。
  fn default() -> Self {
    Self {
      directives: None,
      to_stdout: true,
      file: FileSink::Fixed { path: None },
    }
  }
}

impl LogConfig {
  pub fn new() -> Self {
    Self::default()
  }

  /// 过滤器指令的语法糖（`directives = lvl`）。
  pub fn level(self, lvl: &str) -> Self {
    self.directives(lvl)
  }

  pub fn directives(mut self, directives: impl Into<String>) -> Self {
    self.directives = Some(directives.into());
    self
  }

  pub fn stdout(mut self, on: bool) -> Self {
    self.to_stdout = on;
    self
  }

  /// 落盘开关。开 = [`FileSink::Fixed`] 的默认位置。
  pub fn file(mut self, on: bool) -> Self {
    self.file = if on {
      FileSink::Fixed { path: None }
    } else {
      FileSink::Disabled
    };
    self
  }

  /// 固定单文件、指定路径。
  pub fn file_path(mut self, path: impl Into<PathBuf>) -> Self {
    self.file = FileSink::Fixed {
      path: Some(path.into()),
    };
    self
  }

  /// 轮转日志；后缀默认 `log`，周期默认 [`Rotation::Daily`]。
  pub fn rolling(mut self, dir: impl Into<PathBuf>, prefix: impl Into<String>) -> Self {
    self.file = FileSink::Rolling {
      dir: dir.into(),
      prefix: prefix.into(),
      suffix: "log".to_owned(),
      rotation: Rotation::default(),
      max_files: None,
    };
    self
  }

  /// 轮转周期；当前不是轮转模式则什么都不做。
  pub fn rotation(mut self, rotation: Rotation) -> Self {
    if let FileSink::Rolling {
      rotation: current, ..
    } = &mut self.file
    {
      *current = rotation;
    }
    self
  }

  /// 保留份数上限；当前不是轮转模式则什么都不做。
  pub fn max_files(mut self, n: usize) -> Self {
    if let FileSink::Rolling { max_files, .. } = &mut self.file {
      *max_files = Some(n);
    }
    self
  }

  /// 安装全局订阅器：控制台层（无 target 前缀）+ 文件层（无 ANSI）。
  ///
  /// **重复安装静默容忍**：`try_init` 只在「已经装过全局订阅器」时报错——同一进程里多次
  /// `run`（测试、重启式热载）属于正常路径，不该炸。
  pub fn install(&self) -> Result<(), LogError> {
    let filter = match &self.directives {
      Some(directives) => EnvFilter::new(directives.clone()),
      None => EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
    };

    // 各文件模式构造出的 `Layer` 具体类型各不相同（`FmtLayer` / `Registry` / appender），
    // 没法统一进同一个 `Option<Layer>`；故在 **subscriber 层级**装箱——每个分支都产出
    // `Box<dyn Subscriber + Send + Sync>`，类型统一、互不冲突。
    let subscriber: Box<dyn Subscriber + Send + Sync> = match &self.file {
      FileSink::Disabled => {
        let registry = tracing_subscriber::registry().with(filter);
        if self.to_stdout {
          boxed(registry.with(layer().with_writer(std::io::stdout)))
        } else {
          boxed(registry)
        }
      }
      FileSink::Fixed { path } => {
        let target = path.clone().unwrap_or_else(default_log_path);
        let file = open_log_file(&target)?;
        let registry = tracing_subscriber::registry()
          .with(filter)
          .with(layer().with_ansi(false).with_writer(Mutex::new(file)));
        tracing::debug!(path = %target.display(), "日志落点");
        if self.to_stdout {
          boxed(registry.with(layer().with_writer(std::io::stdout)))
        } else {
          boxed(registry)
        }
      }
      FileSink::Rolling {
        dir,
        prefix,
        suffix,
        rotation,
        max_files,
      } => {
        let mut builder = RollingFileAppender::builder()
          .rotation(rotation.to_appender())
          .filename_prefix(prefix)
          .filename_suffix(suffix);
        if let Some(max) = max_files {
          builder = builder.max_log_files(*max);
        }
        let appender = builder.build(dir).map_err(|error| LogError::Appender {
          dir: dir.clone(),
          reason: error.to_string(),
        })?;
        let registry = tracing_subscriber::registry()
          .with(filter)
          .with(layer().with_ansi(false).with_writer(Mutex::new(appender)));
        tracing::debug!(dir = %dir.display(), %prefix, ?rotation, "日志落点（轮转）");
        if self.to_stdout {
          boxed(registry.with(layer().with_writer(std::io::stdout)))
        } else {
          boxed(registry)
        }
      }
    };

    // 错误只有「已经装过」这一种，属正常路径（见方法文档）。
    let _ = subscriber.try_init();
    Ok(())
  }
}

/// 装箱成统一的 subscriber 类型（各分支的 `Layer` 具体类型不同，见 [`LogConfig::install`]）。
fn boxed<S: Subscriber + Send + Sync + 'static>(
  subscriber: S,
) -> Box<dyn Subscriber + Send + Sync> {
  Box::new(subscriber)
}

/// 默认落点：`paths().logs` 下的 [`DEFAULT_FILE_NAME`]。
///
/// 路径层未安装（全局 `get()` 走临时目录兜底）时也拿得到值，不会因此挡住启动。
fn default_log_path() -> PathBuf {
  match path::dir(BaseDirectory::AppLog) {
    Some(dir) => dir.join(DEFAULT_FILE_NAME),
    None => std::env::temp_dir().join(DEFAULT_FILE_NAME),
  }
}

/// 打开（或创建）日志文件：**append** 模式，父目录缺了先建。
fn open_log_file(path: &Path) -> Result<std::fs::File, LogError> {
  if let Some(parent) = path.parent() {
    std::fs::create_dir_all(parent).map_err(|source| LogError::Directory {
      path: parent.to_path_buf(),
      source,
    })?;
  }
  std::fs::OpenOptions::new()
    .create(true)
    .append(true)
    .open(path)
    .map_err(|source| LogError::File {
      path: path.to_path_buf(),
      source,
    })
}

/// 安装日志时出的事——只报「装不上」，不影响应用继续跑（调用方报一声即止）。
#[derive(Debug)]
pub enum LogError {
  /// 建日志目录失败。
  Directory {
    path: PathBuf,
    source: std::io::Error,
  },
  /// 打开 / 创建日志文件失败。
  File {
    path: PathBuf,
    source: std::io::Error,
  },
  /// 轮转 appender 建不起来。
  Appender { dir: PathBuf, reason: String },
}

impl std::fmt::Display for LogError {
  fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    match self {
      LogError::Directory { path, source } => {
        write!(f, "建日志目录失败：{}（{source}）", path.display())
      }
      LogError::File { path, source } => {
        write!(f, "打开日志文件失败：{}（{source}）", path.display())
      }
      LogError::Appender { dir, reason } => {
        write!(f, "建日志轮转 appender 失败：{}（{reason}）", dir.display())
      }
    }
  }
}

impl std::error::Error for LogError {
  fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
    match self {
      LogError::Directory { source, .. } | LogError::File { source, .. } => Some(source),
      LogError::Appender { .. } => None,
    }
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  /// 沙盒目录：测试自己建、自己清。
  struct Sandbox(PathBuf);

  impl Sandbox {
    fn new(name: &str) -> Self {
      let dir = std::env::temp_dir().join(format!("lxlake-log-test-{name}-{}", std::process::id()));
      let _ = std::fs::remove_dir_all(&dir);
      std::fs::create_dir_all(&dir).expect("建沙盒目录");
      Self(dir)
    }

    fn path(&self) -> &Path {
      &self.0
    }
  }

  impl Drop for Sandbox {
    fn drop(&mut self) {
      let _ = std::fs::remove_dir_all(&self.0);
    }
  }

  /// 默认：两边都要，落盘走路径层的应用日志目录。
  #[test]
  fn the_default_goes_to_stdout_and_the_app_log_dir() {
    let config = LogConfig::default();

    assert!(config.to_stdout);
    assert_eq!(config.file, FileSink::Fixed { path: None });
    assert_eq!(config.directives, None, "不指定就读 RUST_LOG，再退 info");

    let fallback = default_log_path();
    assert!(
      fallback.ends_with(DEFAULT_FILE_NAME),
      "默认落点要带默认文件名：{}",
      fallback.display()
    );
  }

  /// 语法糖只是改字段，改完的数据要能原样读出来。
  #[test]
  fn the_shorthands_write_the_fields() {
    let config = LogConfig::new()
      .level("debug,lxlake=trace")
      .stdout(false)
      .file(false);

    assert_eq!(config.directives.as_deref(), Some("debug,lxlake=trace"));
    assert!(!config.to_stdout);
    assert_eq!(config.file, FileSink::Disabled);

    // 再打开、再指定路径：都落回 `Fixed`。
    let config = config.file(true);
    assert_eq!(config.file, FileSink::Fixed { path: None });
    let config = config.file_path("logs/app.log");
    assert_eq!(
      config.file,
      FileSink::Fixed {
        path: Some(PathBuf::from("logs/app.log"))
      }
    );
  }

  /// 轮转模式的三个字段（外加默认后缀与默认周期）。
  #[test]
  fn rolling_carries_its_knobs() {
    let config = LogConfig::new()
      .rolling("logs", "lxlake")
      .rotation(Rotation::Hourly)
      .max_files(3);

    assert_eq!(
      config.file,
      FileSink::Rolling {
        dir: PathBuf::from("logs"),
        prefix: "lxlake".to_owned(),
        suffix: "log".to_owned(),
        rotation: Rotation::Hourly,
        max_files: Some(3),
      }
    );

    // 非轮转模式下这两个改法不该把 `Fixed` 变成别的什么。
    let config = LogConfig::new()
      .file_path("a.log")
      .rotation(Rotation::Never);
    assert_eq!(
      config.file,
      FileSink::Fixed {
        path: Some(PathBuf::from("a.log"))
      }
    );
    assert_eq!(Rotation::default(), Rotation::Daily);
  }

  /// 路径指的是「父目录其实是个文件」——建目录必然失败，此时要报错而不是 panic。
  #[test]
  fn an_unopenable_path_is_an_error_not_a_panic() {
    let sandbox = Sandbox::new("unopenable");
    let blocker = sandbox.path().join("blocker");
    std::fs::write(&blocker, b"x").expect("写占位文件");

    let err = LogConfig::new()
      .file_path(blocker.join("nested").join("app.log"))
      .install()
      .expect_err("父目录建不出来就该报错");

    assert!(matches!(err, LogError::Directory { .. }), "错误是 {err:?}");
    assert!(
      err.to_string().contains("建日志目录失败"),
      "报错要说明是哪一步：{err}"
    );
  }

  /// 重复安装是正常路径（同进程多次 `run`）：第二次也要 `Ok`，不 panic。
  #[test]
  fn installing_twice_is_tolerated() {
    let sandbox = Sandbox::new("twice");
    let config = LogConfig::new()
      .stdout(false)
      .file_path(sandbox.path().join("app.log"));

    config.install().expect("首次安装");
    config.install().expect("重复安装要静默容忍");

    assert!(
      sandbox.path().join("app.log").exists(),
      "日志文件该被建出来"
    );
  }

  /// 轮转模式能装起来（appender 建得出来、参数被接受）。
  ///
  /// 不断言「目录里有文件」：轮转文件是**首次写入**时才开的，而全局订阅器在同一进程里只能装
  /// 一次——并行测试谁先装谁生效，这里断言文件在不在就是不确定的。落点相关的断言走
  /// `Fixed` 那条（它的文件是立刻开的）。
  #[test]
  fn a_rolling_sink_installs() {
    let sandbox = Sandbox::new("rolling");
    LogConfig::new()
      .stdout(false)
      .rolling(sandbox.path(), "lxlake")
      .rotation(Rotation::Never)
      .max_files(2)
      .install()
      .expect("轮转模式该装得上");
  }
}
