//! 文档模型：编辑器的**唯一真相源**。
//!
//! 范式是**文档驱动保留模式**：磁盘上的文档（`.lxml`，见片 5）解析成 [`Document`]，
//! [`instantiate`] 把它投影成一棵 [`UiTree`] 交给渲染与命中测试。选中 / 悬停 / 焦点**不在文档里**
//! ——它们是编辑器的临时状态，存在文档之外的独立状态表里（见 `apps/lxlake-editor`）。
//!
//! 三条边界写在这里，因为后面每一片都要靠它们：
//!
//! - **文档不认识窗口尺寸。** 节点只描述锚点与尺寸，摆位由 [`instantiate`] 的 `area` 参数决定；
//!   否则存盘会把「某台机器的分辨率」存进去。
//! - **文档不认识项目根。** 它是一份内存模型，读写文件是别处的事。
//! - **节点只写作者写过的属性。** 缺项一律取属性表里的默认值（[`NODE_PROPS`]），所以默认值
//!   只有一份，`.lxml` 也只需要写出被改过的那些属性。
//!
//! **纯 CPU**：不含任何 GPU 类型，也不加 feature 门（见 `docs/architecture.md` 分层）。

use crate::core::geometry::{LogicalRect, LogicalSize};
use crate::core::widget::{Anchor, UiId, Widget};
use crate::ui::UiTree;

/// 节点身份：**跨帧稳定**的派生值。选中态跨帧比对的就是它。
///
/// 派生规则（此处定死，**改规则等于选中错位**）：节点写了 `key` 就用 `key`，否则用
/// 「类型 + 兄弟索引」的路径；路径经 FNV-1a 64 成 `u64`。于是「给节点起个 key」是作者表达
/// 「这个节点的身份与位置无关」的唯一手段——中间插一个兄弟不会让选中跳到别的节点上。
///
/// 它**不入文档**（文档里存的是 `key`），所以只是运行期身份，不同文档之间不必可比。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct NodeId(u64);

impl NodeId {
  /// 给 [`UiTree`] 用的 id：取低位 32 位。
  ///
  /// 两个节点撞上同一个 `UiId` 的概率约 2⁻³²，代价是命中测试取最上者、`rect_of` 取最先者。
  /// 第一刀的文档是程序构造的、节点数在百量级，不值当为此加一层校验。
  pub fn ui_id(self) -> UiId {
    UiId(self.0 as u32)
  }
}

/// 属性值。三个变体与 [`PropKind`] 一一对应。
#[derive(Debug, Clone, PartialEq)]
pub enum PropValue {
  Number(f64),
  /// 枚举属性的**候选下标**（候选表在 [`PropKind::Enum`] 里）。
  Variant(usize),
  Text(String),
}

/// 属性描述：这个属性叫什么、能取什么、没写时是什么。
#[derive(Debug, Clone, PartialEq)]
pub struct PropDesc {
  pub key: &'static str,
  pub label: &'static str,
  pub kind: PropKind,
  /// 节点没写这条属性时用的值。**默认值只写这一份**——`instantiate` 与（片 4 的）Inspector
  /// 读的是同一个数，不会出现「面板显示 0 但实际摆了 120」。
  pub default: PropValue,
}

/// 属性的取值形状。Inspector（片 4）据此决定给哪一行配哪种控件。
#[derive(Debug, Clone, PartialEq)]
pub enum PropKind {
  /// 数字：`min` / `max` 是取值边界，`step` 是面板上点一下的步长。
  Number { min: f64, max: f64, step: f64 },
  /// 枚举：候选表的下标就是 [`PropValue::Variant`] 的值。
  Enum(&'static [&'static str]),
  /// 文本：第一刀只读（不碰文本输入与 IME）。
  Text,
}

/// `anchor` 属性的候选表：名字的顺序即候选顺序，与 [`anchor_at`] 一一对应。
pub static ANCHOR_NAMES: [&str; 9] = [
  "top-left",
  "top-center",
  "top-right",
  "center-left",
  "center",
  "center-right",
  "bottom-left",
  "bottom-center",
  "bottom-right",
];

/// 第一刀认的**全部节点属性**。做成一张表是因为三处要读同一份：文档层（类型化描述）、
/// [`instantiate`]（取值与默认值）、Inspector（生成行）——属性名与默认值因此各只写一份。
pub static NODE_PROPS: [PropDesc; 6] = [
  PropDesc {
    key: "name",
    label: "名称",
    kind: PropKind::Text,
    default: PropValue::Text(String::new()),
  },
  PropDesc {
    key: "anchor",
    label: "锚点",
    kind: PropKind::Enum(&ANCHOR_NAMES),
    default: PropValue::Variant(0),
  },
  PropDesc {
    key: "x",
    label: "偏移 X",
    kind: PropKind::Number {
      min: -4096.0,
      max: 4096.0,
      step: 8.0,
    },
    default: PropValue::Number(0.0),
  },
  PropDesc {
    key: "y",
    label: "偏移 Y",
    kind: PropKind::Number {
      min: -4096.0,
      max: 4096.0,
      step: 8.0,
    },
    default: PropValue::Number(0.0),
  },
  PropDesc {
    key: "width",
    label: "宽",
    kind: PropKind::Number {
      min: 0.0,
      max: 4096.0,
      step: 8.0,
    },
    default: PropValue::Number(120.0),
  },
  PropDesc {
    key: "height",
    label: "高",
    kind: PropKind::Number {
      min: 0.0,
      max: 4096.0,
      step: 8.0,
    },
    default: PropValue::Number(32.0),
  },
];

/// 一条属性：键 + 值。键是 `String` 而不是 `&'static str`——`.lxml` 里的键来自源文本。
#[derive(Debug, Clone, PartialEq)]
pub struct Prop {
  pub key: String,
  pub value: PropValue,
}

/// 文档里的一个节点。第一刀是**扁平**的：没有父子，也没有容器（层级留给切片 3）。
#[derive(Debug, Clone, PartialEq)]
pub struct DocNode {
  id: NodeId,
  kind: String,
  props: Vec<Prop>,
}

impl DocNode {
  /// 稳定身份（见 [`NodeId`]）。
  pub fn id(&self) -> NodeId {
    self.id
  }

  /// 节点类型（`panel` / `label` 这类）。第一刀只做展示，不影响摆位。
  pub fn kind(&self) -> &str {
    &self.kind
  }

  /// 取属性；节点没写过就是 `None`（默认值在 [`NODE_PROPS`] 里，不在这里补齐）。
  pub fn prop(&self, key: &str) -> Option<&PropValue> {
    self
      .props
      .iter()
      .find(|prop| prop.key == key)
      .map(|prop| &prop.value)
  }

  /// 数字属性。没写过、或值的类型不对（手写文档可能出现）都退回表里的默认值——**不 panic**：
  /// 文档是外部输入，`instantiate` 不是校验器。
  pub fn number(&self, key: &str) -> f64 {
    match self.prop(key).or_else(|| default_of(key)) {
      Some(PropValue::Number(value)) => *value,
      _ => 0.0,
    }
  }

  /// 枚举属性的候选下标。越界或类型不对一律退回 0。
  pub fn variant(&self, key: &str) -> usize {
    match self.prop(key).or_else(|| default_of(key)) {
      Some(PropValue::Variant(index)) => *index,
      _ => 0,
    }
  }

  /// 设一个属性：有就覆盖，没有就追加。片 4 的 Inspector 从这条路径改文档。
  pub fn set(&mut self, key: &str, value: PropValue) {
    match self.props.iter_mut().find(|prop| prop.key == key) {
      Some(prop) => prop.value = value,
      None => self.props.push(Prop {
        key: key.to_owned(),
        value,
      }),
    }
  }
}

/// 一份文档：节点的有序列表。第一刀没有父子，顺序就是绘制顺序（后加的在上面）。
#[derive(Debug, Default, Clone, PartialEq)]
pub struct Document {
  nodes: Vec<DocNode>,
}

impl Document {
  pub fn new() -> Self {
    Self::default()
  }

  /// 加一个节点，返回它的稳定身份。
  ///
  /// `key` 是作者给的身份（`None` = 没有，身份按位置派生）；`kind` 只影响无 key 节点的身份与显示。
  pub fn push(&mut self, kind: impl Into<String>, key: Option<&str>) -> NodeId {
    let kind = kind.into();
    let id = derive_id(&kind, key, self.nodes.len());
    self.nodes.push(DocNode {
      id,
      kind,
      props: Vec::new(),
    });
    id
  }

  /// 全部节点（加入顺序）。
  pub fn nodes(&self) -> &[DocNode] {
    &self.nodes
  }

  /// 按身份取节点（片 3 的结构树、片 4 的 Inspector 生成行时读它）。
  pub fn node_by_id(&self, id: NodeId) -> Option<&DocNode> {
    self.nodes.iter().find(|node| node.id == id)
  }

  /// 按身份取可变节点（改属性走这条）。
  pub fn node_mut(&mut self, id: NodeId) -> Option<&mut DocNode> {
    self.nodes.iter_mut().find(|node| node.id == id)
  }
}

/// 投影：文档 → 树。
///
/// 摆位按 `area` 算（编辑器将来传**算出来的预览区**，而不是整个窗口）——文档里因此永远不含
/// 窗口尺寸。返回的树与文档等长，节点顺序一致。
pub fn instantiate(document: &Document, area: LogicalRect) -> UiTree {
  let mut tree = UiTree::new();
  for node in document.nodes() {
    tree.add(widget_of(node));
  }
  tree.layout_in(area);
  tree
}

/// 问题：属性名写错时给 `None`，而不是 panic（见 [`DocNode::number`]）。
pub fn default_of(key: &str) -> Option<&'static PropValue> {
  NODE_PROPS
    .iter()
    .find(|desc| desc.key == key)
    .map(|desc| &desc.default)
}

/// 候选下标 → 锚点。顺序与 [`ANCHOR_NAMES`] 一致（左上 → 右下，行优先）。
fn anchor_at(index: usize) -> Anchor {
  match index {
    1 => Anchor::TopCenter,
    2 => Anchor::TopRight,
    3 => Anchor::CenterLeft,
    4 => Anchor::Center,
    5 => Anchor::CenterRight,
    6 => Anchor::BottomLeft,
    7 => Anchor::BottomCenter,
    8 => Anchor::BottomRight,
    _ => Anchor::TopLeft,
  }
}

/// 一个节点 → 一块摆位数据。缺项与类型不对都由 [`DocNode::number`] / [`DocNode::variant`] 兜住。
fn widget_of(node: &DocNode) -> Widget {
  Widget::new(
    node.id().ui_id(),
    anchor_at(node.variant("anchor")),
    LogicalSize::new(node.number("width"), node.number("height")),
  )
  .offset([node.number("x"), node.number("y")])
}

/// 身份派生（规则见 [`NodeId`]）。
fn derive_id(kind: &str, key: Option<&str>, index: usize) -> NodeId {
  let path = match key {
    Some(key) => format!("key:{key}"),
    None => format!("{kind}#{index}"),
  };
  NodeId(hash_path(&path))
}

/// FNV-1a 64。
///
/// 手写而不是用 `DefaultHasher`：后者只承诺**同一进程内**一致，而 `NodeId` 要在选中态里长期
/// 比对，换个 Rust 版本哈希值变了就会选中错位。这一段是常量运算，不引入依赖也永远不变。
fn hash_path(path: &str) -> u64 {
  const OFFSET: u64 = 0xcbf2_9ce4_8422_2325;
  const PRIME: u64 = 0x0000_0100_0000_01b3;

  let mut hash = OFFSET;
  for byte in path.as_bytes() {
    hash ^= u64::from(*byte);
    hash = hash.wrapping_mul(PRIME);
  }
  hash
}

#[cfg(test)]
mod tests {
  use super::*;

  const AREA: LogicalRect = LogicalRect::new(0.0, 0.0, 800.0, 600.0);

  fn sample() -> Document {
    let mut document = Document::new();
    document.push("panel", Some("sidebar"));
    document.push("panel", None);
    document.push("label", None);
    document
  }

  /// 同一份文档摆两次出同一棵树——身份与摆位都不是「遍历的偶然产物」。
  #[test]
  fn instantiate_is_repeatable() {
    let document = sample();

    let first = instantiate(&document, AREA);
    let second = instantiate(&document, AREA);

    let ids = |tree: &UiTree| {
      tree
        .placed()
        .iter()
        .map(|placed| placed.id)
        .collect::<Vec<_>>()
    };
    assert!(!ids(&first).is_empty(), "样例文档该摆出东西来");
    assert_eq!(ids(&first), ids(&second));
  }

  /// 有 `key` 的节点身份**与位置无关**：前面插一个兄弟，它的 id 不变（选中态因此不跳到别处）；
  /// 无 key 的兄弟则各按索引区分。
  #[test]
  fn a_keyed_node_keeps_its_id_when_a_sibling_is_inserted() {
    let mut alone = Document::new();
    let keyed = alone.push("panel", Some("sidebar"));

    let mut with_sibling = Document::new();
    let sibling = with_sibling.push("panel", None);
    let keyed_after = with_sibling.push("panel", Some("sidebar"));

    assert_eq!(keyed, keyed_after, "key 是身份，与所在位置无关");
    assert_ne!(sibling, keyed_after, "无 key 的兄弟按「类型 + 索引」区分");
  }

  /// 节点没写过的属性取**表里的默认值**；`prop` 仍返回 `None`（没写过就是没有）。
  #[test]
  fn missing_props_fall_back_to_the_table_defaults() {
    let mut document = Document::new();
    let id = document.push("panel", None);
    let node = document.node_by_id(id).expect("刚加的节点");

    assert_eq!(node.number("width"), 120.0);
    assert_eq!(node.number("height"), 32.0);
    assert_eq!(node.number("x"), 0.0);
    assert_eq!(node.variant("anchor"), 0);
    assert_eq!(node.prop("name"), None);
  }

  /// 改一个数字属性 → 这个节点的矩形跟着变（实时预览的最小闭环）。
  #[test]
  fn changing_a_number_prop_moves_the_node() {
    let mut document = Document::new();
    let id = document.push("panel", Some("sidebar"));

    let before = instantiate(&document, AREA)
      .rect_of(id.ui_id())
      .expect("节点进了树");

    document
      .node_mut(id)
      .expect("按身份取得到节点")
      .set("width", PropValue::Number(300.0));
    let after = instantiate(&document, AREA)
      .rect_of(id.ui_id())
      .expect("节点还在树里");

    assert_eq!(before.width, 120.0, "默认宽来自属性表");
    assert_eq!(after.width, 300.0);
    assert_eq!(after.x, before.x, "只改了宽度，锚点没动");
  }

  /// 锚点候选表的顺序是行优先的九宫格——与 `Anchor` 的九个变体一一对应。
  #[test]
  fn the_anchor_table_covers_the_grid_in_row_major_order() {
    assert_eq!(ANCHOR_NAMES.len(), 9);
    assert_eq!(anchor_at(0).normalized(), [0.0, 0.0]);
    assert_eq!(anchor_at(4).normalized(), [0.5, 0.5]);
    assert_eq!(anchor_at(8).normalized(), [1.0, 1.0]);
  }
}
