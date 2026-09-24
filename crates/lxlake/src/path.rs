//! 路径层：系统目录与应用目录的**唯一判断处**。
//!
//! 框架里所有「目录该在哪」的判断都收在这里，别的模块不自行拼目录。形状分两层：
//!
//! - [`Paths`] 是**值**：一处算出全部目录，字段可以直接拉；
//! - [`BaseDirectory`] 是**键**：把「要哪个目录」变成数据（对齐 tauri 的 `path::BaseDirectory`），
//!   于是取值可以走 [`Paths::dir`] / [`Paths::resolve`] 这一条统一入口，而不是一串各自为政的函数。
//!
//! **自行解析、不引第三方目录库**（`dirs` / `directories` 一类都不用）：那些库在 Android 上返回的
//! 并不一定好用，而平台约定自己写反而更可控。代价是两处**明确的有损简化**——Linux 的用户目录
//! 不调 `xdg-user-dir`、不处理本地化目录名（只读 `user-dirs.dirs` 的键值）；Windows 的用户目录
//! 按 `%USERPROFILE%\<英文名>` 拼，被 OneDrive 重定向过的配置会不准（真要用准的已知文件夹
//! `SHGetKnownFolderPath`，留到「拉平台层依赖」那一步，届时只改本文件一个函数）。
//!
//! # 各平台约定
//!
//! | 基准 | Windows | Linux (XDG) | macOS | Android |
//! | ---- | ------- | ----------- | ----- | ------- |
//! | AppRoot | `%APPDATA%\<app_id>` | `$XDG_DATA_HOME` 或 `~/.local/share` 下 `<app_id>` | `~/Library/Application Support/<app_id>` | 内部数据目录 |
//! | AppData | `<root>\data` | 同 AppRoot | 同 AppRoot | `<root>/files` |
//! | AppConfig | `<root>\config` | `$XDG_CONFIG_HOME` 或 `~/.config` 下 `<app_id>` | 同 AppRoot | `<root>/files/config` |
//! | AppCache | `%LOCALAPPDATA%\<app_id>\cache` | `$XDG_CACHE_HOME` 或 `~/.cache` 下 `<app_id>` | `~/Library/Caches/<app_id>` | `<root>/cache` |
//! | AppLog | `%LOCALAPPDATA%\<app_id>\logs` | `$XDG_STATE_HOME` 或 `~/.local/state` 下 `<app_id>/logs` | `~/Library/Logs/<app_id>` | `<root>/files/logs` |
//! | Temp | `%TEMP%` | `$TMPDIR` 或 `/tmp` | `$TMPDIR` 或 `/tmp` | `<root>/cache` |
//! | Home | `%USERPROFILE%` | `$HOME` | `$HOME` | 无 |
//! | Desktop / Document / Download / Picture / Video / Audio | `%USERPROFILE%\<英文名>` | XDG 用户目录 | `~/<英文名>` | 无 |
//! | Executable | 当前 exe 的父目录 | 同 | 同 | 无（APK 无此概念） |
//!
//! 环境变量缺失时逐级退回：`%APPDATA%` → `%USERPROFILE%\AppData\Roaming`；`$XDG_*` → 家目录下的
//! 规范默认值；家目录也拿不到 → `std::env::temp_dir()` 兜底并 `tracing::warn!` 一声。
//!
//! # 安装时机
//!
//! 全局一份由平台入口在**进事件循环之前**装（桌面 [`install`] 的是 `Paths::for_app(app_id)`，
//! Android 是 `Paths::for_android(internal_data_path)`——离开 activity 就没有内部数据目录这个
//! 信息，所以只能由入口传进来）。安装早于日志安装，故日志的默认落点（[`BaseDirectory::AppLog`]）
//! 解析得到。因 `OnceLock` 语义**先装后用**：[`get`] 未安装时构造兜底实例，故永不 panic，
//! 单测也不必起事件循环。

#[cfg(target_os = "linux")]
use std::collections::BTreeMap;
use std::path::{Component, Path, PathBuf};
use std::sync::OnceLock;

/// 家目录与平台环境变量都拿不到时的兜底应用名。
const FALLBACK_APP_ID: &str = "lxlake";

/// 基准目录：把「要哪个目录」变成**数据**，而不是一串函数或一个字符串。
///
/// 有意不取 tauri 的这几项：`LocalData`（Windows 的 roaming / local 之别已由 [`AppData`] 与
/// [`AppCache`] 编码）、`Runtime` / `Template` / `Font` / `Resource` / `Public`（都属打包期概念，
/// 等打包与 Android 里程碑按需再加）。加变体是纯增量，没有兼容负担。
///
/// [`AppData`]: BaseDirectory::AppData
/// [`AppCache`]: BaseDirectory::AppCache
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum BaseDirectory {
  /// 应用私有目录的根。
  AppRoot,
  /// 应用数据（存档一类要留的东西）。
  AppData,
  /// 应用配置。
  AppConfig,
  /// 应用缓存（随时可被系统清掉）。
  AppCache,
  /// 日志落点。
  AppLog,
  /// 用户家目录。
  Home,
  /// 临时目录。
  Temp,
  /// 用户桌面。
  Desktop,
  /// 用户文档。
  Document,
  /// 用户下载。
  Download,
  /// 用户图片。
  Picture,
  /// 用户视频。
  Video,
  /// 用户音频。
  Audio,
  /// 可执行体所在目录。
  Executable,
}

impl BaseDirectory {
  /// 全部变体。长度写死是**故意的**：加变体时这里编译不过，逼着约定表与测试一起更新。
  pub const ALL: [BaseDirectory; 14] = [
    BaseDirectory::AppRoot,
    BaseDirectory::AppData,
    BaseDirectory::AppConfig,
    BaseDirectory::AppCache,
    BaseDirectory::AppLog,
    BaseDirectory::Home,
    BaseDirectory::Temp,
    BaseDirectory::Desktop,
    BaseDirectory::Document,
    BaseDirectory::Download,
    BaseDirectory::Picture,
    BaseDirectory::Video,
    BaseDirectory::Audio,
    BaseDirectory::Executable,
  ];
}

/// 应用与系统的全部路径，一处定稿（入口算一次，之后只读）。
pub struct Paths {
  /// 应用私有目录的根。
  pub root: PathBuf,
  /// 应用数据。
  pub data: PathBuf,
  /// 应用配置。
  pub config: PathBuf,
  /// 应用缓存。
  pub cache: PathBuf,
  /// 日志落点。
  pub logs: PathBuf,
  /// 临时目录（任何平台都有）。
  pub temp: PathBuf,
  /// 家目录；受限环境（Android）为 `None`。
  pub home: Option<PathBuf>,
  /// 用户桌面；取不到为 `None`。
  pub desktop: Option<PathBuf>,
  /// 用户文档；取不到为 `None`。
  pub document: Option<PathBuf>,
  /// 用户下载；取不到为 `None`。
  pub download: Option<PathBuf>,
  /// 用户图片；取不到为 `None`。
  pub picture: Option<PathBuf>,
  /// 用户视频；取不到为 `None`。
  pub video: Option<PathBuf>,
  /// 用户音频；取不到为 `None`。
  pub audio: Option<PathBuf>,
  /// 可执行体所在目录；取不到为 `None`。
  pub exe: Option<PathBuf>,
}

impl Paths {
  /// 按当前平台约定算出全部目录（读环境变量 + 平台规范，不引第三方库）。
  pub fn for_app(app_id: &str) -> Paths {
    let home = home_dir();
    let temp = temp_dir();
    let app = app_dirs(app_id, home.as_deref(), &temp);
    let user = user_dirs(home.as_deref());

    Paths {
      root: app.root,
      data: app.data,
      config: app.config,
      cache: app.cache,
      logs: app.logs,
      temp,
      home,
      desktop: user.desktop,
      document: user.document,
      download: user.download,
      picture: user.picture,
      video: user.video,
      audio: user.audio,
      exe: exe_dir(),
    }
  }

  /// Android：以 activity 给的内部数据目录为根。
  ///
  /// 该信息只有入口有（离开 activity 就没了），所以由入口传进来；拿不到就退临时目录。
  #[cfg(target_os = "android")]
  pub fn for_android(internal_data: Option<PathBuf>) -> Paths {
    let root = internal_data.unwrap_or_else(|| {
      tracing::warn!("lxlake: activity 未给出内部数据目录，应用目录退回临时目录");
      std::env::temp_dir().join(FALLBACK_APP_ID)
    });
    let files = root.join("files");
    let cache = root.join("cache");

    Paths {
      data: files.clone(),
      config: files.join("config"),
      logs: files.join("logs"),
      temp: cache.clone(),
      cache,
      root,
      home: None,
      desktop: None,
      document: None,
      download: None,
      picture: None,
      video: None,
      audio: None,
      exe: None,
    }
  }

  /// 建齐 data / config / cache / logs 四处目录。
  pub fn ensure(&self) -> std::io::Result<()> {
    for dir in [&self.data, &self.config, &self.cache, &self.logs] {
      std::fs::create_dir_all(dir)?;
    }
    Ok(())
  }

  /// 按枚举取目录（字段拉取的统一入口）。系统级目录取不到时为 `None`。
  pub fn dir(&self, base: BaseDirectory) -> Option<&Path> {
    match base {
      BaseDirectory::AppRoot => Some(&self.root),
      BaseDirectory::AppData => Some(&self.data),
      BaseDirectory::AppConfig => Some(&self.config),
      BaseDirectory::AppCache => Some(&self.cache),
      BaseDirectory::AppLog => Some(&self.logs),
      BaseDirectory::Temp => Some(&self.temp),
      BaseDirectory::Home => self.home.as_deref(),
      BaseDirectory::Desktop => self.desktop.as_deref(),
      BaseDirectory::Document => self.document.as_deref(),
      BaseDirectory::Download => self.download.as_deref(),
      BaseDirectory::Picture => self.picture.as_deref(),
      BaseDirectory::Video => self.video.as_deref(),
      BaseDirectory::Audio => self.audio.as_deref(),
      BaseDirectory::Executable => self.exe.as_deref(),
    }
  }

  /// 把相对路径解析到基准目录下；绝对路径原样返回。
  pub fn resolve(&self, path: impl AsRef<Path>, base: BaseDirectory) -> Option<PathBuf> {
    let path = path.as_ref();
    if path.is_absolute() {
      return Some(path.to_path_buf());
    }
    self.dir(base).map(|dir| dir.join(path))
  }

  /// [`Paths::resolve`] 的字符串形式，省去调用点各自 `to_string_lossy`。
  pub fn resolve_string(&self, path: &str, base: BaseDirectory) -> Option<String> {
    self
      .resolve(path, base)
      .map(|path| path.to_string_lossy().into_owned())
  }
}

/// 全局一份：[`install`] 装，[`get`] 取。
static PATHS: OnceLock<Paths> = OnceLock::new();

/// 安装全局路径。由平台入口在进事件循环之前调用一次；重复安装静默忽略。
pub(crate) fn install(paths: Paths) {
  let _ = PATHS.set(paths);
}

/// 全局路径。未安装时构造**兜底实例**（临时目录 + 默认应用名），因此永不 panic——
/// 单测与「还没进运行时就想读个目录」都能直接用。
pub fn get() -> &'static Paths {
  PATHS.get_or_init(fallback)
}

/// 模块级语法糖：按枚举取全局路径下的目录。
pub fn dir(base: BaseDirectory) -> Option<&'static Path> {
  get().dir(base)
}

/// 模块级语法糖：把相对路径解析到全局路径的基准目录下。
pub fn resolve(path: impl AsRef<Path>, base: BaseDirectory) -> Option<PathBuf> {
  get().resolve(path, base)
}

/// 兜底实例：全部挂在临时目录下（日志落点这类字段仍可用，只是位置是临时的）。
fn fallback() -> Paths {
  tracing::warn!("lxlake: 全局路径尚未安装，退回临时目录");
  let temp = std::env::temp_dir();
  let app = AppDirs::under_root(temp.join(FALLBACK_APP_ID));

  Paths {
    root: app.root,
    data: app.data,
    config: app.config,
    cache: app.cache,
    logs: app.logs,
    temp,
    home: None,
    desktop: None,
    document: None,
    download: None,
    picture: None,
    video: None,
    audio: None,
    exe: None,
  }
}

// ── 与目录约定无关的路径工具（纯字符串 / 平台属性） ──────────────────────────────

/// 词法归一化：去掉 `.`、就地消掉能安全回退的 `..`、统一分隔符。**不碰文件系统**，
/// 因此不解析符号链接，也不要求路径存在。
pub fn normalize(path: &Path) -> PathBuf {
  let mut out = PathBuf::new();
  for component in path.components() {
    match component {
      Component::CurDir => {}
      Component::ParentDir => match out.components().next_back() {
        // 上一层是普通名字才弹得掉；根 `/` 或 Windows 的前缀 `C:` 弹不掉，原样留着。
        Some(Component::Normal(_)) => {
          out.pop();
        }
        _ => out.push(component.as_os_str()),
      },
      other => out.push(other.as_os_str()),
    }
  }
  if out.as_os_str().is_empty() {
    out.push(".");
  }
  out
}

/// 展开开头的 `~`（`~` 与 `~/…` 两种形态）。`~user/…` 不展开（我们不认识别人的家目录）；
/// 全局路径没有家目录时返回 `None`。
pub fn expand_user(path: &str) -> Option<String> {
  let rest = path.strip_prefix('~')?;
  if !rest.is_empty() && !rest.starts_with(['/', '\\']) {
    return None;
  }
  let home = get().home.as_ref()?;
  Some(format!("{}{rest}", home.display()))
}

/// 是否是隐藏项：名字以 `.` 开头。
///
/// 只看名字，不看 Windows 的隐藏属性（std 不暴露它，要读属性就得拉平台层依赖）。
pub fn is_hidden(path: &Path) -> bool {
  path
    .file_name()
    .is_some_and(|name| name.to_string_lossy().starts_with('.'))
}

/// 洗一个能当文件名用的字符串：非法字符与不可见字符换成 `_`，去掉结尾的 `.` 与空格
/// （Windows 上这类名字建不出来）。
pub fn sanitize_file_name(name: &str) -> String {
  let mut out: String = name
    .chars()
    .map(|ch| {
      if ch.is_control() || matches!(ch, '<' | '>' | ':' | '"' | '/' | '\\' | '|' | '?' | '*') {
        '_'
      } else {
        ch
      }
    })
    .collect();
  while out.ends_with(['.', ' ']) {
    out.pop();
  }
  out
}

/// Windows 的 MAX_PATH 突破：给绝对路径加 `\\?\` 前缀（UNC 走 `\\?\UNC\`），其余平台原样返回。
///
/// 前缀形式要求路径**已经规范化**（不含 `.` / `..`），所以调用点通常先过一遍 [`normalize`]。
#[cfg(target_os = "windows")]
pub fn long_path(path: &Path) -> PathBuf {
  let text = path.to_string_lossy();
  if text.starts_with(r"\\?\") {
    return path.to_path_buf();
  }
  match text.strip_prefix(r"\\") {
    Some(rest) => PathBuf::from(format!(r"\\?\UNC\{rest}")),
    None if path.is_absolute() => PathBuf::from(format!(r"\\?\{text}")),
    None => path.to_path_buf(),
  }
}

/// 非 Windows 平台没有 MAX_PATH 这回事。
#[cfg(not(target_os = "windows"))]
pub fn long_path(path: &Path) -> PathBuf {
  path.to_path_buf()
}

// ── 平台约定 ──────────────────────────────────────────────────────────────

/// 应用私有目录（各平台约定不同，见文件头的表）。
struct AppDirs {
  root: PathBuf,
  data: PathBuf,
  config: PathBuf,
  cache: PathBuf,
  logs: PathBuf,
}

impl AppDirs {
  /// 兜底形态：全部挂在给定根下。
  fn under_root(root: PathBuf) -> AppDirs {
    let data = root.join("data");
    let config = root.join("config");
    let cache = root.join("cache");
    let logs = root.join("logs");
    AppDirs {
      root,
      data,
      config,
      cache,
      logs,
    }
  }

  /// macOS 的形态：`root` 与 `data` / `config` 同处，缓存在别处。
  #[cfg(target_os = "macos")]
  fn split(root: PathBuf, cache: PathBuf, logs: PathBuf) -> AppDirs {
    AppDirs {
      data: root.clone(),
      config: root.clone(),
      root,
      cache,
      logs,
    }
  }
}

/// 系统级用户目录。受限环境（Android）全为 `None`。
struct UserDirs {
  desktop: Option<PathBuf>,
  document: Option<PathBuf>,
  download: Option<PathBuf>,
  picture: Option<PathBuf>,
  video: Option<PathBuf>,
  audio: Option<PathBuf>,
}

impl UserDirs {
  fn none() -> UserDirs {
    UserDirs {
      desktop: None,
      document: None,
      download: None,
      picture: None,
      video: None,
      audio: None,
    }
  }
}

/// 家目录：`$HOME`，或 Windows 的 `%USERPROFILE%`。
fn home_dir() -> Option<PathBuf> {
  ["HOME", "USERPROFILE"]
    .iter()
    .find_map(std::env::var_os)
    .map(PathBuf::from)
    .filter(|home| !home.as_os_str().is_empty())
}

/// 临时目录：`%TEMP%` / `$TMPDIR`，都没有就用 std 的兜底。
fn temp_dir() -> PathBuf {
  ["TEMP", "TMP", "TMPDIR"]
    .iter()
    .find_map(std::env::var_os)
    .map(PathBuf::from)
    .filter(|temp| !temp.as_os_str().is_empty())
    .unwrap_or_else(std::env::temp_dir)
}

/// 兜底根：家目录与平台环境变量都拿不到时，一律落临时目录并提示一声。
fn fallback_root(temp: &Path) -> PathBuf {
  tracing::warn!("lxlake: 拿不到家目录 / 平台环境变量，应用目录退回临时目录");
  temp.join(FALLBACK_APP_ID)
}

/// 可执行体所在目录。Android 的「可执行体」是 APK，没有对应概念。
#[cfg(not(target_os = "android"))]
fn exe_dir() -> Option<PathBuf> {
  std::env::current_exe()
    .ok()
    .and_then(|exe| exe.parent().map(Path::to_path_buf))
}

#[cfg(target_os = "android")]
fn exe_dir() -> Option<PathBuf> {
  None
}

/// 应用私有目录：Windows 的 roaming / local 之别就在这里。
#[cfg(target_os = "windows")]
fn app_dirs(app_id: &str, home: Option<&Path>, temp: &Path) -> AppDirs {
  let roaming = std::env::var_os("APPDATA")
    .map(PathBuf::from)
    .or_else(|| home.map(|home| home.join("AppData").join("Roaming")));
  let local = std::env::var_os("LOCALAPPDATA")
    .map(PathBuf::from)
    .or_else(|| home.map(|home| home.join("AppData").join("Local")));

  match (roaming, local) {
    (Some(roaming), Some(local)) => {
      // 数据与配置留在漫游这份根下，缓存与日志归本地那份（见文件头的表）。
      let root = roaming.join(app_id);
      let local_app = local.join(app_id);
      AppDirs {
        data: root.join("data"),
        config: root.join("config"),
        root,
        cache: local_app.join("cache"),
        logs: local_app.join("logs"),
      }
    }
    _ => AppDirs::under_root(fallback_root(temp).join(app_id)),
  }
}

/// 应用私有目录：XDG。数据 / 配置 / 缓存 / 状态四处各归各的 `$XDG_*`。
#[cfg(target_os = "linux")]
fn app_dirs(app_id: &str, home: Option<&Path>, temp: &Path) -> AppDirs {
  let Some(home) = home else {
    return AppDirs::under_root(fallback_root(temp).join(app_id));
  };
  let xdg = |key: &str, default: &str| {
    std::env::var_os(key)
      .map(PathBuf::from)
      .unwrap_or_else(|| home.join(default))
  };

  let root = xdg("XDG_DATA_HOME", ".local/share").join(app_id);
  AppDirs {
    data: root.clone(),
    config: xdg("XDG_CONFIG_HOME", ".config").join(app_id),
    cache: xdg("XDG_CACHE_HOME", ".cache").join(app_id),
    logs: xdg("XDG_STATE_HOME", ".local/state")
      .join(app_id)
      .join("logs"),
    root,
  }
}

/// 应用私有目录：Apple 那套 `Library` 三段。
#[cfg(target_os = "macos")]
fn app_dirs(app_id: &str, home: Option<&Path>, temp: &Path) -> AppDirs {
  let Some(home) = home else {
    return AppDirs::under_root(fallback_root(temp).join(app_id));
  };
  AppDirs::split(
    home.join("Library/Application Support").join(app_id),
    home.join("Library/Caches").join(app_id),
    home.join("Library/Logs").join(app_id),
  )
}

/// 应用私有目录：Android 的真实根来自 activity（见 [`Paths::for_android`]）。
/// 走这条只说明没走 `for_android`，退回临时目录。
#[cfg(target_os = "android")]
fn app_dirs(app_id: &str, _home: Option<&Path>, temp: &Path) -> AppDirs {
  AppDirs::under_root(fallback_root(temp).join(app_id))
}

/// 其余平台（headless 一类）：`platform` 后端会先报「尚未实现」，这里给个能编译的兜底。
#[cfg(not(any(
  target_os = "windows",
  target_os = "linux",
  target_os = "macos",
  target_os = "android"
)))]
fn app_dirs(app_id: &str, _home: Option<&Path>, temp: &Path) -> AppDirs {
  AppDirs::under_root(fallback_root(temp).join(app_id))
}

/// 用户目录：Windows 与 macOS 都按家目录下的英文名拼（简化，见文件头）。
#[cfg(any(target_os = "windows", target_os = "macos"))]
fn user_dirs(home: Option<&Path>) -> UserDirs {
  let Some(home) = home else {
    return UserDirs::none();
  };
  UserDirs {
    desktop: Some(home.join("Desktop")),
    document: Some(home.join("Documents")),
    download: Some(home.join("Downloads")),
    picture: Some(home.join("Pictures")),
    video: Some(home.join(VIDEO_DIR_NAME)),
    audio: Some(home.join("Music")),
  }
}

/// Windows 的视频目录叫 `Videos`，macOS 叫 `Movies`。
#[cfg(target_os = "windows")]
const VIDEO_DIR_NAME: &str = "Videos";

#[cfg(target_os = "macos")]
const VIDEO_DIR_NAME: &str = "Movies";

/// 用户目录：Linux 读 `user-dirs.dirs`，缺键退回家目录下的英文默认名。
#[cfg(target_os = "linux")]
fn user_dirs(home: Option<&Path>) -> UserDirs {
  let Some(home) = home else {
    return UserDirs::none();
  };
  let table = xdg_user_dirs(home);
  let pick = |key: &str, default: &str| {
    table
      .get(key)
      .cloned()
      .unwrap_or_else(|| home.join(default))
  };

  UserDirs {
    desktop: Some(pick("XDG_DESKTOP_DIR", "Desktop")),
    document: Some(pick("XDG_DOCUMENTS_DIR", "Documents")),
    download: Some(pick("XDG_DOWNLOAD_DIR", "Downloads")),
    picture: Some(pick("XDG_PICTURES_DIR", "Pictures")),
    video: Some(pick("XDG_VIDEOS_DIR", "Videos")),
    audio: Some(pick("XDG_MUSIC_DIR", "Music")),
  }
}

/// 读 `$XDG_CONFIG_HOME/user-dirs.dirs`（缺省 `~/.config/user-dirs.dirs`）里的 `KEY="值"` 行。
///
/// **有损简化**：不调 `xdg-user-dir`、不处理本地化目录名；文件缺失就是个空表，调用方退默认名。
#[cfg(target_os = "linux")]
fn xdg_user_dirs(home: &Path) -> BTreeMap<String, PathBuf> {
  let config_home = std::env::var_os("XDG_CONFIG_HOME")
    .map(PathBuf::from)
    .unwrap_or_else(|| home.join(".config"));
  let Ok(text) = std::fs::read_to_string(config_home.join("user-dirs.dirs")) else {
    return BTreeMap::new();
  };

  let mut table = BTreeMap::new();
  for line in text.lines() {
    let line = line.trim();
    if line.is_empty() || line.starts_with('#') {
      continue;
    }
    let Some((key, value)) = line.split_once('=') else {
      continue;
    };
    let value = value
      .trim()
      .trim_matches('"')
      .replace("$HOME", &home.display().to_string());
    table.insert(key.trim().to_owned(), PathBuf::from(value));
  }
  table
}

/// 用户目录：受限环境（Android 等）没有这一层。
#[cfg(not(any(target_os = "windows", target_os = "linux", target_os = "macos")))]
fn user_dirs(_home: Option<&Path>) -> UserDirs {
  UserDirs::none()
}

#[cfg(test)]
mod tests {
  use super::*;

  /// 测试用的沙盒：一个临时目录，测试结束自己删。
  struct Sandbox(PathBuf);

  impl Sandbox {
    fn new(name: &str) -> Sandbox {
      let dir =
        std::env::temp_dir().join(format!("lxlake-path-test-{name}-{}", std::process::id()));
      let _ = std::fs::remove_dir_all(&dir);
      Sandbox(dir)
    }
  }

  impl Drop for Sandbox {
    fn drop(&mut self) {
      let _ = std::fs::remove_dir_all(&self.0);
    }
  }

  /// 约定表要钉住：Windows 的 roaming / local 之别、app_id 的落点、exe 取父目录。
  #[cfg(target_os = "windows")]
  #[test]
  fn the_windows_conventions_are_what_the_table_says() {
    let (Ok(roaming), Ok(local)) = (std::env::var("APPDATA"), std::env::var("LOCALAPPDATA")) else {
      return; // 环境变量不在（非正常会话）就不测，避免误报。
    };
    let app_id = "com.lxlake.path-test";
    let paths = Paths::for_app(app_id);
    let root = PathBuf::from(&roaming).join(app_id);
    let local_app = PathBuf::from(&local).join(app_id);

    assert_eq!(paths.root, root);
    assert_eq!(paths.data, root.join("data"));
    assert_eq!(paths.config, root.join("config"));
    assert_eq!(paths.cache, local_app.join("cache"));
    assert_eq!(paths.logs, local_app.join("logs"));
    assert_eq!(paths.temp, temp_dir());
    assert_eq!(paths.home, home_dir());
    assert_eq!(paths.exe, exe_dir());
    assert!(paths.exe.is_some(), "桌面平台上 exe 目录该取得到");
  }

  /// `dir` 要覆盖**全部**变体：应用私有那几个与临时目录永远有值，其余看家目录取没取到。
  #[test]
  fn dir_answers_every_variant() {
    let paths = Paths::for_app("com.lxlake.dir-test");

    for base in BaseDirectory::ALL {
      let expected = match base {
        BaseDirectory::AppRoot
        | BaseDirectory::AppData
        | BaseDirectory::AppConfig
        | BaseDirectory::AppCache
        | BaseDirectory::AppLog
        | BaseDirectory::Temp => true,
        BaseDirectory::Home
        | BaseDirectory::Desktop
        | BaseDirectory::Document
        | BaseDirectory::Download
        | BaseDirectory::Picture
        | BaseDirectory::Video
        | BaseDirectory::Audio => paths.home.is_some(),
        BaseDirectory::Executable => paths.exe.is_some(),
      };
      assert_eq!(paths.dir(base).is_some(), expected, "{base:?} 与约定表不符");
    }
  }

  /// `resolve`：相对路径挂到基准目录下，绝对路径原样返回。
  #[test]
  fn resolve_joins_relative_and_keeps_absolute() {
    let paths = Paths::for_app("com.lxlake.resolve-test");

    assert_eq!(
      paths.resolve("config.toml", BaseDirectory::AppConfig),
      Some(paths.config.join("config.toml"))
    );
    assert_eq!(
      paths.resolve_string("config.toml", BaseDirectory::AppConfig),
      Some(
        paths
          .config
          .join("config.toml")
          .to_string_lossy()
          .into_owned()
      )
    );
    // 相对路径里的分隔符原样带着走（`Path` 的比较按分量来，混用分隔符不算不等）。
    assert_eq!(
      paths.resolve("save/world.dat", BaseDirectory::AppData),
      Some(paths.data.join("save/world.dat"))
    );

    let absolute = if cfg!(target_os = "windows") {
      PathBuf::from(r"C:\tmp\lxlake.txt")
    } else {
      PathBuf::from("/tmp/lxlake.txt")
    };
    assert_eq!(
      paths.resolve(&absolute, BaseDirectory::AppData),
      Some(absolute),
      "绝对路径不该被塞进基准目录"
    );
  }

  /// `ensure` 把四处目录建齐（沙盒里跑，不碰真实的应用目录）。
  #[test]
  fn ensure_creates_the_four_app_directories() {
    let sandbox = Sandbox::new("ensure");
    let root = sandbox.0.clone();
    let mut paths = Paths::for_app("com.lxlake.ensure-test");
    paths.root = root.clone();
    paths.data = root.join("data");
    paths.config = root.join("config");
    paths.cache = root.join("cache");
    paths.logs = root.join("logs");

    paths.ensure().expect("建目录不该失败");
    for dir in [&paths.data, &paths.config, &paths.cache, &paths.logs] {
      assert!(dir.is_dir(), "{} 该被建出来", dir.display());
    }
  }

  /// `~` 两种形态展开作家目录；`~user` 不展开。
  #[test]
  fn expand_user_only_handles_our_own_home() {
    assert_eq!(expand_user("~user/x"), None, "别人的家目录我们不知道");
    assert_eq!(expand_user("a/~/b"), None, "`~` 只在开头才是家目录");

    match &get().home {
      Some(home) => {
        assert_eq!(expand_user("~"), Some(home.display().to_string()));
        assert_eq!(
          expand_user("~/save.dat"),
          Some(format!("{}/save.dat", home.display())),
          "展开就是把前缀换成家目录，分隔符沿用原样"
        );
      }
      None => assert_eq!(expand_user("~"), None, "没有家目录就无从展开"),
    }
  }

  /// 归一化是纯词法的：`.` 去掉，能回退的 `..` 就地消掉，越界的 `..` 留着。
  #[test]
  fn normalize_is_lexical() {
    assert_eq!(normalize(Path::new("a/./b")), PathBuf::from("a/b"));
    assert_eq!(normalize(Path::new("a/b/../c")), PathBuf::from("a/c"));
    assert_eq!(normalize(Path::new("./a")), PathBuf::from("a"));
    assert_eq!(normalize(Path::new("")), PathBuf::from("."));

    let escaping = if cfg!(target_os = "windows") {
      PathBuf::from(r"..\a")
    } else {
      PathBuf::from("../a")
    };
    assert_eq!(
      normalize(&escaping),
      escaping,
      "回退不掉的 `..` 要原样留着，不能被吃掉"
    );
  }

  /// 洗文件名：非法字符换 `_`，结尾的 `.` 与空格去掉。
  #[test]
  fn sanitize_replaces_illegal_characters() {
    assert_eq!(
      sanitize_file_name(r#"a<b>c:d"e/f\g|h?i*j"#),
      "a_b_c_d_e_f_g_h_i_j"
    );
    assert_eq!(sanitize_file_name("name...  "), "name");
    assert_eq!(sanitize_file_name("正常名字.txt"), "正常名字.txt");
    assert_eq!(sanitize_file_name("带\n换行"), "带_换行");
  }

  /// 隐藏项只看名字开头。
  #[test]
  fn hidden_is_decided_by_a_leading_dot() {
    assert!(is_hidden(Path::new(".git")));
    assert!(is_hidden(Path::new("/tmp/.hidden")));
    assert!(!is_hidden(Path::new("visible")));
  }

  /// MAX_PATH 突破：Windows 上加前缀，UNC 走 `\\?\UNC\`；其余平台原样。
  #[test]
  fn long_path_prefixes_only_on_windows() {
    let plain = if cfg!(windows) {
      Path::new(r"C:\tmp\lxlake")
    } else {
      Path::new("/tmp/lxlake")
    };
    let converted = long_path(plain);

    if cfg!(windows) {
      assert_eq!(converted, PathBuf::from(r"\\?\C:\tmp\lxlake"));
      assert_eq!(long_path(&converted), converted, "已经带前缀就不要再加");
      assert_eq!(
        long_path(Path::new(r"\\server\share\dir")),
        PathBuf::from(r"\\?\UNC\server\share\dir")
      );
      assert_eq!(
        long_path(Path::new(r"relative\dir")),
        PathBuf::from(r"relative\dir"),
        "相对路径加不了前缀"
      );
    } else {
      assert_eq!(converted, plain);
    }
  }
}
