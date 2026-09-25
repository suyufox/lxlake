//! 项目：一个目录 + 一份清单，以及它下面的文档。
//!
//! 编辑器要打开的不是「一份文件」而是「一个项目」——这正是它与 demo 的分野。形状定死三条：
//!
//! - **项目 = 一个目录**：清单 [`MANIFEST_FILE`] 所在的目录**就是**项目根。先有清单后有根，
//!   于是不存在「清单说根在哪、根在哪说清单在哪」的循环。
//! - **文档放 `ui/`**（[`DOCUMENT_DIR`]）：递归找 `*.lxml`，列表**按相对路径排序**——目录枚举
//!   的顺序由文件系统定，而结构树与将来的项目管理器都要一个稳定顺序。
//! - **清单手工取字段**：不引 serde 派生。诊断要中文、要能指到行列，而 `toml::de::Error::span`
//!   给的正是字节偏移：自己转 1-based 行列，比让 serde 报一串英文路径更贴编辑器。
//!
//! **只有读这一半**：打开项目、列文档、装一份文档。写盘、新建项目、构建打包都还没有。
//!
//! **纯 CPU**：读文件而已，不含任何 GPU 类型，也不加 feature 门（见 `docs/architecture.md` 分层）。

use std::fmt;
use std::path::{Path, PathBuf};

use crate::ui::{Document, LxmlDiagnostic, from_lxml};

/// 清单文件名。**它所在的目录就是项目根**（见模块文档）。
pub const MANIFEST_FILE: &str = "lxlake.toml";

/// 文档目录名：`*.lxml` 放这里，递归找。
pub const DOCUMENT_DIR: &str = "ui";

/// 清单没写 `version` 时用的值。
const DEFAULT_VERSION: &str = "0.1.0";

/// 清单里认得的字段。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Manifest {
  /// 项目名（显示用）。
  pub name: String,
  /// 项目版本。**还没有人读它**，所以缺省 [`DEFAULT_VERSION`]，缺了不算错。
  pub version: String,
  /// 打开项目时先装的那份文档，**相对项目根**的路径（如 `ui/main.lxml`）。
  pub entry: String,
}

/// 一个打开的项目：根、清单、发现到的文档。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Project {
  root: PathBuf,
  manifest: Manifest,
  documents: Vec<PathBuf>,
}

impl Project {
  /// 打开一个项目：读清单 → 列文档 → 校验 `entry` 在不在。
  ///
  /// 三步都可能出错，**每一步都说清是哪一步**（[`ProjectError`]），不做「打不开」这种糊涂账。
  pub fn open(root: impl Into<PathBuf>) -> Result<Project, ProjectError> {
    let root = root.into();

    let manifest_path = root.join(MANIFEST_FILE);
    // 先看清单在不在：不在就说明这一层不是项目根，比读失败再猜原因直接。
    if !manifest_path.is_file() {
      return Err(ProjectError::ManifestMissing {
        path: manifest_path,
      });
    }
    let manifest = parse_manifest(&read_text(&manifest_path)?)?;

    let mut documents = Vec::new();
    let ui = root.join(DOCUMENT_DIR);
    if ui.is_dir() {
      collect_documents(&ui, Path::new(DOCUMENT_DIR), &mut documents)?;
    }
    documents.sort_by_key(|path| sort_key(path));

    // `entry` 必须是**发现到的那一份**：指到一个没被收进来的文件，说明清单与目录已经对不上。
    let entry = Path::new(&manifest.entry);
    if !documents.iter().any(|document| document == entry) {
      let path = root.join(entry);
      return Err(ProjectError::EntryMissing {
        entry: manifest.entry,
        path,
      });
    }

    Ok(Project {
      root,
      manifest,
      documents,
    })
  }

  /// 项目根（清单所在的那个目录）。
  pub fn root(&self) -> &Path {
    &self.root
  }

  /// 清单。
  pub fn manifest(&self) -> &Manifest {
    &self.manifest
  }

  /// 发现到的文档，**相对项目根**，已排序（见模块文档）。
  pub fn documents(&self) -> &[PathBuf] {
    &self.documents
  }

  /// 先装的那份文档（相对项目根）。[`Project::open`] 已保证它在 [`Project::documents`] 里。
  pub fn entry(&self) -> &str {
    &self.manifest.entry
  }

  /// 装一份文档（`relative` 相对项目根），连带上解析与映射两阶段的诊断。
  ///
  /// 不校验它是不是发现到的那一份：这里只是「把一份文本装成 [`Document`]」，将来要打开项目外的
  /// 单文件也走这条。
  pub fn load(
    &self,
    relative: impl AsRef<Path>,
  ) -> Result<(Document, Vec<LxmlDiagnostic>), ProjectError> {
    let source = read_text(&self.root.join(relative))?;
    Ok(from_lxml(&source))
  }
}

/// 打开项目时的失败方式。**一个变体 = 一步**（读清单 / 解析清单 / 校验 entry）。
#[derive(Debug)]
pub enum ProjectError {
  /// 该目录里没有清单——它不是项目根。
  ManifestMissing {
    /// 期望的清单路径。
    path: PathBuf,
  },
  /// 读文件失败（清单或文档）。
  Read {
    /// 读的是哪个文件。
    path: PathBuf,
    /// 底层原因。
    source: std::io::Error,
  },
  /// 清单本身有问题：语法错，或必填字段缺了 / 类型不对。
  Manifest {
    /// 1-based 起始行；**0 表示指不到位置**（值不带坐标，见 [`parse_manifest`]）。
    line: usize,
    /// 1-based 起始列（按字符计）。
    column: usize,
    /// 面向用户的文案（中文，可直接展示）。
    message: String,
  },
  /// 清单里的 `entry` 找不到对应文件。
  EntryMissing {
    /// 清单里写的那条相对路径。
    entry: String,
    /// 解析出来的绝对路径。
    path: PathBuf,
  },
}

impl fmt::Display for ProjectError {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    match self {
      Self::ManifestMissing { path } => {
        write!(f, "`{}` 不是项目根：找不到 {MANIFEST_FILE}", path.display())
      }
      Self::Manifest {
        line,
        column,
        message,
      } if *line > 0 => write!(f, "清单第 {line} 行第 {column} 列：{message}"),
      Self::Manifest { message, .. } => write!(f, "清单有误：{message}"),
      Self::EntryMissing { entry, path } => write!(
        f,
        "清单里的 `entry = \"{entry}\"` 找不到：{} 不存在",
        path.display()
      ),
      Self::Read { path, source } => write!(f, "读 {} 失败：{source}", path.display()),
    }
  }
}

impl std::error::Error for ProjectError {
  fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
    match self {
      Self::Read { source, .. } => Some(source),
      _ => None,
    }
  }
}

/// 读文件为文本；失败一律 [`ProjectError::Read`]。
fn read_text(path: &Path) -> Result<String, ProjectError> {
  std::fs::read_to_string(path).map_err(|source| ProjectError::Read {
    path: path.to_path_buf(),
    source,
  })
}

/// 递归找 `*.lxml`。`prefix` 是该目录**在项目根下**的相对路径——列出来的就是它。
fn collect_documents(
  dir: &Path,
  prefix: &Path,
  out: &mut Vec<PathBuf>,
) -> Result<(), ProjectError> {
  let entries = std::fs::read_dir(dir).map_err(|source| ProjectError::Read {
    path: dir.to_path_buf(),
    source,
  })?;

  for entry in entries {
    let entry = entry.map_err(|source| ProjectError::Read {
      path: dir.to_path_buf(),
      source,
    })?;
    let relative = prefix.join(entry.file_name());
    let file_type = entry.file_type().map_err(|source| ProjectError::Read {
      path: entry.path(),
      source,
    })?;

    if file_type.is_dir() {
      collect_documents(&entry.path(), &relative, out)?;
    } else if entry.path().extension().is_some_and(|ext| ext == "lxml") {
      out.push(relative);
    }
  }
  Ok(())
}

/// 排序键：相对路径的字符串形式，分隔符统一成 `/`——同一批文件在任何平台上给出的顺序一致。
fn sort_key(path: &Path) -> String {
  path.to_string_lossy().replace('\\', "/")
}

/// 清单文本 → [`Manifest`]。
///
/// **手工取字段**（不引 serde 派生）：于是缺字段、写错类型都能给出中文文案。代价是**值不带位置**
/// ——`toml::Value` 只带值，所以这类诊断的 `line = 0`（与 `LxmlDiagnostic::without_position` 同口径）。
fn parse_manifest(source: &str) -> Result<Manifest, ProjectError> {
  let value: toml::Value = source.parse().map_err(|error: toml::de::Error| {
    // 语法错**带位置**：`span` 给的是字节偏移，转成 1-based 行列交给编辑器。
    let (line, column) = error
      .span()
      .map_or((0, 0), |span| line_column(source, span.start));
    ProjectError::Manifest {
      line,
      column,
      message: error.message().to_owned(),
    }
  })?;

  let table = value
    .as_table()
    .ok_or_else(|| manifest_error("清单的最外层该是一张表（`键 = 值` 那些行）".to_owned()))?;

  Ok(Manifest {
    name: field(table, "name")?,
    version: table
      .get("version")
      .and_then(toml::Value::as_str)
      .unwrap_or(DEFAULT_VERSION)
      .to_owned(),
    entry: field(table, "entry")?,
  })
}

/// 一条**指不到位置**的清单诊断（见 [`parse_manifest`]）。
fn manifest_error(message: String) -> ProjectError {
  ProjectError::Manifest {
    line: 0,
    column: 0,
    message,
  }
}

/// 取一个**必填**的字符串字段：不写、或写的不是字符串都报出来。
fn field(table: &toml::Table, key: &str) -> Result<String, ProjectError> {
  match table.get(key) {
    Some(toml::Value::String(value)) => Ok(value.clone()),
    Some(_) => Err(manifest_error(format!("`{key}` 该是一个字符串"))),
    None => Err(manifest_error(format!("清单缺 `{key}`"))),
  }
}

/// 字节偏移 → **1-based** `(行, 列)`；列按**字符**计（与 `LxmlDiagnostic` 同口径）。
fn line_column(source: &str, offset: usize) -> (usize, usize) {
  let offset = offset.min(source.len());
  let (mut line, mut column) = (1, 1);
  for (index, ch) in source.char_indices() {
    if index >= offset {
      break;
    }
    if ch == '\n' {
      line += 1;
      column = 1;
    } else {
      column += 1;
    }
  }
  (line, column)
}

#[cfg(test)]
mod tests {
  use super::*;

  /// 测试用的项目目录：一个临时目录，测试结束自己删（同 `path.rs` 的 `Sandbox`）。
  struct Sandbox(PathBuf);

  impl Sandbox {
    fn new(name: &str) -> Sandbox {
      let dir =
        std::env::temp_dir().join(format!("lxlake-project-test-{name}-{}", std::process::id()));
      let _ = std::fs::remove_dir_all(&dir);
      std::fs::create_dir_all(&dir).expect("沙盒该建得出来");
      Sandbox(dir)
    }

    /// 写一个文件（必要的父目录一起建）。
    fn write(&self, relative: &str, contents: &str) {
      let path = self.0.join(relative);
      if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).expect("父目录该建得出来");
      }
      std::fs::write(&path, contents).expect("文件该写得进去");
    }
  }

  impl Drop for Sandbox {
    fn drop(&mut self) {
      let _ = std::fs::remove_dir_all(&self.0);
    }
  }

  /// 一份像样的清单。
  const MANIFEST: &str = "name = \"样例\"\nversion = \"0.2.0\"\nentry = \"ui/main.lxml\"\n";

  /// 打开项目：清单读出来、`ui/` 下的文档递归列出并**按路径排序**，非 `.lxml` 不进列表。
  #[test]
  fn open_reads_the_manifest_and_lists_the_documents() {
    let sandbox = Sandbox::new("open");
    sandbox.write(MANIFEST_FILE, MANIFEST);
    sandbox.write("ui/panels/inner.lxml", "<panel />");
    sandbox.write("ui/main.lxml", "<panel />");
    sandbox.write("ui/readme.md", "不是文档");

    let project = Project::open(sandbox.0.clone()).expect("这是一份像样的项目");

    assert_eq!(project.manifest().name, "样例");
    assert_eq!(project.manifest().version, "0.2.0");
    assert_eq!(project.entry(), "ui/main.lxml");
    assert_eq!(
      project.documents(),
      [
        PathBuf::from("ui/main.lxml"),
        PathBuf::from("ui/panels/inner.lxml"),
      ],
      "文档记的是**项目根下**的相对路径（所以带 `ui/`），且按路径排序"
    );
    assert_eq!(project.root(), sandbox.0);
  }

  /// 清单没写 `version` 时按缺省填——它还没有人读，缺了不算错。
  #[test]
  fn version_defaults_when_the_manifest_omits_it() {
    let sandbox = Sandbox::new("version");
    sandbox.write(MANIFEST_FILE, "name = \"样例\"\nentry = \"ui/main.lxml\"\n");
    sandbox.write("ui/main.lxml", "<panel />");

    let project = Project::open(sandbox.0.clone()).expect("省掉 version 不该拦人");

    assert_eq!(project.manifest().version, DEFAULT_VERSION);
  }

  /// 没有清单的目录**不是项目根**：报的是「找不到清单」，不是「读失败」。
  #[test]
  fn a_directory_without_a_manifest_is_not_a_project_root() {
    let sandbox = Sandbox::new("no-manifest");
    sandbox.write("ui/main.lxml", "<panel />");

    let error = Project::open(sandbox.0.clone()).expect_err("没有清单就不是项目根");

    assert!(
      matches!(error, ProjectError::ManifestMissing { .. }),
      "{error:?}"
    );
    assert!(
      error.to_string().contains(MANIFEST_FILE),
      "文案里该有清单文件名：{error}"
    );
  }

  /// 清单语法错要**指到行列**，而不是「解析失败」四个字。
  #[test]
  fn a_broken_manifest_points_at_the_line() {
    let sandbox = Sandbox::new("broken");
    sandbox.write(MANIFEST_FILE, "name = \"样例\"\nversion =\n");

    let error = Project::open(sandbox.0.clone()).expect_err("这一行写坏了");

    match error {
      ProjectError::Manifest { line, column, .. } => {
        assert_eq!(line, 2, "坏的是第二行");
        assert!(column > 0, "列号该指到 `=` 之后");
      }
      other => panic!("该报清单错误，实际 {other:?}"),
    }
  }

  /// 必填字段缺了就说是缺哪个；`toml::Value` 不带位置，所以这一条指不到行列。
  #[test]
  fn a_manifest_without_an_entry_is_reported() {
    let sandbox = Sandbox::new("no-entry");
    sandbox.write(MANIFEST_FILE, "name = \"样例\"\n");

    let error = Project::open(sandbox.0.clone()).expect_err("缺 entry");

    match error {
      ProjectError::Manifest { line, message, .. } => {
        assert_eq!(line, 0, "值不带位置，这一条指不到行列");
        assert!(message.contains("entry"), "{message}");
      }
      other => panic!("该报缺字段，实际 {other:?}"),
    }
  }

  /// `entry` 指到一个没被收进文档列表的文件（写错名字，或根本没那个文件）都要报。
  #[test]
  fn an_entry_that_is_not_there_is_reported() {
    let sandbox = Sandbox::new("bad-entry");
    sandbox.write(MANIFEST_FILE, "name = \"样例\"\nentry = \"ui/nope.lxml\"\n");
    sandbox.write("ui/main.lxml", "<panel />");

    let error = Project::open(sandbox.0.clone()).expect_err("entry 指空了");

    assert!(
      matches!(error, ProjectError::EntryMissing { .. }),
      "{error:?}"
    );
    assert!(
      error.to_string().contains("ui/nope.lxml"),
      "文案里该有那条路径：{error}"
    );
  }

  /// 装一份文档：相对项目根读文件，交给 [`from_lxml`]。
  #[test]
  fn load_reads_a_document_from_the_project() {
    let sandbox = Sandbox::new("load");
    sandbox.write(MANIFEST_FILE, MANIFEST);
    sandbox.write(
      "ui/main.lxml",
      "<panel name=\"根\" width=\"320\"><label /></panel>",
    );

    let project = Project::open(sandbox.0.clone()).expect("像样的项目");
    let (document, diagnostics) = project.load(project.entry()).expect("entry 在项目里");

    assert!(diagnostics.is_empty(), "{diagnostics:?}");
    assert_eq!(document.len(), 2);
    assert_eq!(document.roots()[0].number("width"), 320.0);
  }

  /// 读不到的文件报 [`ProjectError::Read`]（带上路径），不 panic。
  #[test]
  fn load_reports_a_document_it_cannot_read() {
    let sandbox = Sandbox::new("load-missing");
    sandbox.write(MANIFEST_FILE, MANIFEST);
    sandbox.write("ui/main.lxml", "<panel />");

    let project = Project::open(sandbox.0.clone()).expect("像样的项目");
    let error = project.load("ui/nope.lxml").expect_err("文件不在");

    assert!(matches!(error, ProjectError::Read { .. }), "{error:?}");
    assert!(
      error.to_string().contains("nope.lxml"),
      "文案里该有那个文件：{error}"
    );
  }

  /// 字节偏移 → 1-based 行列；列按**字符**计，所以一行中文后面的列号不吃亏。
  #[test]
  fn line_column_counts_characters_not_bytes() {
    let source = "a\n中b";

    assert_eq!(line_column(source, 0), (1, 1));
    assert_eq!(line_column(source, 2), (2, 1), "第二行行首");
    assert_eq!(line_column(source, 5), (2, 2), "`中` 占 3 字节但只算一列");
    assert_eq!(line_column(source, 999), (2, 3), "越界不 panic，落在末尾");
  }
}
