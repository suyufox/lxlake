//! 文档模型：编辑器的**唯一真相源**。
//!
//! 范式是**文档驱动保留模式**：磁盘上的文档（`.lxml`，见 [`from_lxml`]）装成 [`Document`]，
//! [`instantiate`] 把它投影成一棵 [`UiTree`] 交给渲染与命中测试。选中 / 悬停 / 焦点**不在文档里**
//! ——它们是编辑器的临时状态，存在文档之外的独立状态表里（见 `apps/lxlake-editor`）。
//!
//! **节点有父子**：子的容器是**父的矩形**（见 [`instantiate`]），于是嵌套面板这类结构能表达出来。
//! 没有流式布局、没有裁剪——那些是另一刀。
//!
//! 三条边界写在这里，因为后面每一片都要靠它们：
//!
//! - **文档不认识窗口尺寸。** 节点只描述锚点与尺寸，摆位由 [`instantiate`] 的 `area` 参数决定；
//!   否则存盘会把「某台机器的分辨率」存进去。
//! - **文档不认识项目根。** 它是一份内存模型，读写文件是别处的事（见 [`crate::project`]）。
//! - **节点只写作者写过的属性。** 缺项一律取属性表里的默认值（[`NODE_PROPS`]），所以默认值
//!   只有一份；将来的写盘也只需要写出被改过的那些属性。
//!
//! 只有**读**这一半：`.lxml` → [`Document`]。反向的写盘、增删节点、撤销都还没有。
//!
//! **纯 CPU**：不含任何 GPU 类型，也不加 feature 门（见 `docs/architecture.md` 分层）。

use crate::core::geometry::{LogicalRect, LogicalSize};
use crate::core::widget::{Anchor, UiId, Widget};
use crate::ui::lxml::{DiagnosticSeverity, LxmlDiagnostic, StaticNode, parse_lxml};
use crate::ui::{UiTree, place_in};

/// 节点身份：**跨帧稳定**的派生值。选中态跨帧比对的就是它。
///
/// 派生规则（此处定死，**改规则等于选中错位**）：节点写了 `key` 就用 `key`，否则用
/// 「类型 + 兄弟索引」的路径；路径经 FNV-1a 64 成 `u64`。于是「给节点起个 key」是作者表达
/// 「这个节点的身份与位置无关」的唯一手段——中间插一个兄弟不会让选中跳到别的节点上。
///
/// 有父节点时**父的身份并进路径**（见 [`derive_id`]），所以两个不同的父下写同一个 `key` 是两个
/// 节点；反过来，一个节点的身份由「根到此的那一串段」唯一确定，与它在文档里的绝对位置无关。
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

/// 文档里的一个节点。**有父子**：子节点锚进**父节点的矩形**（见 [`instantiate`]），于是嵌套
/// 面板、面板角落的标签这类结构能表达出来。没有流式布局、没有裁剪。
#[derive(Debug, Clone, PartialEq)]
pub struct DocNode {
  id: NodeId,
  kind: String,
  props: Vec<Prop>,
  /// 子节点（顺序 = 绘制顺序，后画的在上）。
  children: Vec<DocNode>,
}

impl DocNode {
  fn new(id: NodeId, kind: String) -> Self {
    Self {
      id,
      kind,
      props: Vec::new(),
      children: Vec::new(),
    }
  }

  /// 稳定身份（见 [`NodeId`]）。
  pub fn id(&self) -> NodeId {
    self.id
  }

  /// 子节点。
  pub fn children(&self) -> &[DocNode] {
    &self.children
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

  /// 按身份找自己或后代（[`Document::node_by_id`] 的递归部分）。
  fn find(&self, id: NodeId) -> Option<&DocNode> {
    if self.id == id {
      return Some(self);
    }
    self.children.iter().find_map(|child| child.find(id))
  }

  /// [`DocNode::find`] 的可变版。
  fn find_mut(&mut self, id: NodeId) -> Option<&mut DocNode> {
    if self.id == id {
      return Some(self);
    }
    self
      .children
      .iter_mut()
      .find_map(|child| child.find_mut(id))
  }
}

/// 一份文档：**若干棵节点树**（根级的那些），顺序就是绘制顺序（后加的在上面）。
#[derive(Debug, Default, Clone, PartialEq)]
pub struct Document {
  roots: Vec<DocNode>,
}

impl Document {
  pub fn new() -> Self {
    Self::default()
  }

  /// 在**根级**加一个节点，返回它的稳定身份。
  ///
  /// `key` 是作者给的身份（`None` = 没有，身份按位置派生）；`kind` 只影响无 key 节点的身份与显示。
  pub fn push(&mut self, kind: impl Into<String>, key: Option<&str>) -> NodeId {
    let kind = kind.into();
    let id = derive_id(&kind, key, self.roots.len(), None);
    self.roots.push(DocNode::new(id, kind));
    id
  }

  /// 在 `parent` 下加一个子节点。
  ///
  /// 父节点找不到就给 `None`——**不 panic**：身份可能来自上一份文档（同 `DocNode::number` 的口径）。
  pub fn push_child(
    &mut self,
    parent: NodeId,
    kind: impl Into<String>,
    key: Option<&str>,
  ) -> Option<NodeId> {
    let kind = kind.into();
    let parent_node = self.node_mut(parent)?;
    // 序号要在**同一层**里数：身份由「根到此的那一串段」定（见 [`NodeId`]）。
    let id = derive_id(&kind, key, parent_node.children.len(), Some(parent));
    parent_node.children.push(DocNode::new(id, kind));
    Some(id)
  }

  /// 根级节点（顺序 = 文档顺序）。
  pub fn roots(&self) -> &[DocNode] {
    &self.roots
  }

  /// 前序展开：`(深度, 节点)`，深度从 0 起。结构树按它出行（缩进 = 深度）。
  ///
  /// 显式栈而不是递归闭包：`impl Iterator` 与递归闭包不好凑一起，而这里的顺序就是绘制顺序，
  /// 必须与 [`instantiate`] 一致。
  pub fn walk(&self) -> impl Iterator<Item = (usize, &DocNode)> {
    let mut stack: Vec<(usize, &DocNode)> = self.roots.iter().rev().map(|node| (0, node)).collect();
    std::iter::from_fn(move || {
      let (depth, node) = stack.pop()?;
      stack.extend(node.children.iter().rev().map(|child| (depth + 1, child)));
      Some((depth, node))
    })
  }

  /// 节点总数（含后代）。
  pub fn len(&self) -> usize {
    self.walk().count()
  }

  /// 空文档：项目里一份文档都没有时就是这样。
  pub fn is_empty(&self) -> bool {
    self.roots.is_empty()
  }

  /// 按身份取节点（结构树、Inspector 生成行时读它）。
  pub fn node_by_id(&self, id: NodeId) -> Option<&DocNode> {
    self.roots.iter().find_map(|node| node.find(id))
  }

  /// 按身份取可变节点（改属性走这条）。
  pub fn node_mut(&mut self, id: NodeId) -> Option<&mut DocNode> {
    self.roots.iter_mut().find_map(|node| node.find_mut(id))
  }
}

/// 从 `.lxml` 源文本装一份文档，连带上解析与映射两阶段的诊断。
///
/// **只有读这一半**（写盘、增删节点、撤销都还没有）。规则三条：
///
/// - 标签名 → 节点类型；**根元素也入文档**——它就是最外层的容器，锚进 [`instantiate`] 的 `area`。
/// - 清单里认得的属性名原样映射（见 [`NODE_PROPS`]），另外认老仓的 `rect="x,y,w,h"` 写法；
///   认不出的属性**不静默丢弃**，出一条 `Warning`（否则 `color` 这类写了没用的东西会一直没人管）。
/// - 标签间的文本（`tag == "#text"`）暂不入文档——属性表里还没有文本这一位，出一条 `Hint`。
///
/// 映射阶段产生的诊断**没有源位置**（`line = 0`）：`StaticNode` 只带属性值，不带各自的坐标。
pub fn from_lxml(src: &str) -> (Document, Vec<LxmlDiagnostic>) {
  let (root, mut diagnostics) = parse_lxml(src);
  let mut document = Document::new();
  add_node(&mut document, None, &root, &mut diagnostics);
  (document, diagnostics)
}

/// 递归把一棵静态树装进文档。`parent` 为 `None` 就是根级。
fn add_node(
  document: &mut Document,
  parent: Option<NodeId>,
  node: &StaticNode,
  diagnostics: &mut Vec<LxmlDiagnostic>,
) {
  let id = match parent {
    Some(parent) => document.push_child(parent, node.tag.clone(), node.key.as_deref()),
    None => Some(document.push(node.tag.clone(), node.key.as_deref())),
  };
  // 父节点找不到：这一支只有「按身份取「父」失败」才走得到，`parent` 就是刚加进去的那个，取不到
  // 说明树已经坏了——那就不再往下装。
  let Some(id) = id else {
    return;
  };

  apply_attrs(document, id, node, diagnostics);

  for child in &node.children {
    if child.tag == "#text" {
      diagnostics.push(LxmlDiagnostic::without_position(
        DiagnosticSeverity::Hint,
        "文本节点暂不入文档（属性表里还没有文本这一位）".to_owned(),
      ));
      continue;
    }
    add_node(document, Some(id), child, diagnostics);
  }
}

/// 一条静态节点的属性 → 文档节点上的属性。
fn apply_attrs(
  document: &mut Document,
  id: NodeId,
  node: &StaticNode,
  diagnostics: &mut Vec<LxmlDiagnostic>,
) {
  let mut pending: Vec<(&'static str, PropValue)> = Vec::new();

  for (key, value) in &node.attrs {
    match key.as_str() {
      // `key` 是身份（`NodeId` 的派生段），不是可编辑属性。
      "key" => {}
      // 老仓的写法：一个 `rect` 顶 x / y / width / height 四条。
      "rect" => match numbers4(value) {
        Some([x, y, width, height]) => {
          pending.push(("x", PropValue::Number(x)));
          pending.push(("y", PropValue::Number(y)));
          pending.push(("width", PropValue::Number(width)));
          pending.push(("height", PropValue::Number(height)));
        }
        None => diagnostics.push(LxmlDiagnostic::without_position(
          DiagnosticSeverity::Warning,
          format!("`rect` 该是 `x,y,宽,高` 四个数字，`{value}` 认不出，已忽略"),
        )),
      },
      "name" => pending.push(("name", PropValue::Text(value.clone()))),
      "anchor" => match ANCHOR_NAMES.iter().position(|name| name == value) {
        Some(index) => pending.push(("anchor", PropValue::Variant(index))),
        None => diagnostics.push(LxmlDiagnostic::without_position(
          DiagnosticSeverity::Warning,
          format!("`anchor` 不认识候选 `{value}`，已忽略"),
        )),
      },
      "x" | "y" | "width" | "height" => {
        // 属性名就是属性键（`NODE_PROPS` 的键与这四条同名），所以这里只是把 `&String` 收成静态的。
        let prop_key = match key.as_str() {
          "x" => "x",
          "y" => "y",
          "width" => "width",
          _ => "height",
        };
        match value.parse::<f64>() {
          Ok(number) => pending.push((prop_key, PropValue::Number(number))),
          Err(_) => diagnostics.push(LxmlDiagnostic::without_position(
            DiagnosticSeverity::Warning,
            format!("`{key}` 该是一个数字，`{value}` 认不出，已忽略"),
          )),
        }
      }
      other => diagnostics.push(LxmlDiagnostic::without_position(
        DiagnosticSeverity::Warning,
        format!("未知属性 `{other}`，已忽略"),
      )),
    }
  }

  if let Some(target) = document.node_mut(id) {
    for (key, value) in pending {
      target.set(key, value);
    }
  }
}

/// 逗号分隔的四个数字（`rect` 的写法）。个数不对、有认不出的数都给 `None`——由调用方出诊断。
fn numbers4(value: &str) -> Option<[f64; 4]> {
  let mut parts = value.split(',').map(|part| part.trim().parse::<f64>().ok());
  let mut out = [0.0; 4];
  for slot in &mut out {
    *slot = parts.next()??;
  }
  // 多出来的第五个数也算认不出：`rect` 就是四条。
  if parts.next().is_some() {
    return None;
  }
  Some(out)
}

/// 投影：文档 → 树。
///
/// 摆位按 `area` 算（编辑器传**算出来的预览区**，而不是整个窗口）——文档里因此永远不含窗口尺寸。
/// **子节点的容器是父节点的矩形**，于是嵌套能表达出来；代价是每个节点各有一个容器，而
/// [`UiTree::layout_in`] 一次只认一个，所以这里自己算完再入树（见 [`UiTree::add_placed`]）——
/// 也正因如此，这棵树**不该再调 `layout_in`**（会把层级压平）。
pub fn instantiate(document: &Document, area: LogicalRect) -> UiTree {
  let mut tree = UiTree::new();
  for root in document.roots() {
    place_subtree(root, area, &mut tree);
  }
  tree
}

/// 前序摆一棵子树：父先入树、子在父之上（与 [`Document::walk`] 同序——绘制顺序就是文档顺序）。
fn place_subtree(node: &DocNode, area: LogicalRect, tree: &mut UiTree) {
  let widget = widget_of(node);
  let rect = place_in(&widget, area);
  tree.add_placed(widget, rect);
  for child in node.children() {
    place_subtree(child, rect, tree);
  }
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
fn derive_id(kind: &str, key: Option<&str>, index: usize, parent: Option<NodeId>) -> NodeId {
  let segment = match key {
    Some(key) => format!("key:{key}"),
    None => format!("{kind}#{index}"),
  };
  // 父的身份数字并进路径：整条链因此由「根到此的那一串段」唯一确定，不必另存一份路径字符串。
  let path = match parent {
    Some(parent) => format!("{}/{segment}", parent.0),
    None => segment,
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

  /// 测试里的改属性捷径：`Editor` 走的是同一条 `node_mut(...).set(...)` 路径。
  fn set(document: &mut Document, id: NodeId, key: &str, value: PropValue) {
    document
      .node_mut(id)
      .expect("按身份取得到节点")
      .set(key, value);
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

  /// 子的容器是**父的矩形**：子贴父的右下角，父挪一步子跟着挪。
  #[test]
  fn a_child_is_placed_inside_its_parent() {
    let mut document = Document::new();
    let parent = document.push("panel", Some("outer"));
    let child = document
      .push_child(parent, "label", Some("inner"))
      .expect("父刚加进去");
    set(&mut document, parent, "width", PropValue::Number(400.0));
    set(&mut document, parent, "height", PropValue::Number(200.0));
    set(&mut document, child, "anchor", PropValue::Variant(8)); // bottom-right

    let tree = instantiate(&document, AREA);
    let parent_rect = tree.rect_of(parent.ui_id()).expect("父在树里");
    let child_rect = tree.rect_of(child.ui_id()).expect("子在树里");

    // 子的右下角与父的右下角重合（尺寸取默认值），而不是与**预览区**的右下角重合。
    assert_eq!(
      child_rect.x + child_rect.width,
      parent_rect.x + parent_rect.width
    );
    assert_eq!(
      child_rect.y + child_rect.height,
      parent_rect.y + parent_rect.height
    );

    set(&mut document, parent, "x", PropValue::Number(40.0));
    let moved = instantiate(&document, AREA);
    assert_eq!(
      moved.rect_of(child.ui_id()).expect("子还在树里").x,
      child_rect.x + 40.0,
      "父挪一步，子跟着挪"
    );
  }

  /// 前序展开：父在子前、深度从 0 起、根级排在后代之后。
  #[test]
  fn walk_is_depth_first_and_reports_depth() {
    let mut document = Document::new();
    let parent = document.push("panel", Some("outer"));
    let child = document
      .push_child(parent, "label", None)
      .expect("父刚加进去");
    let grandchild = document
      .push_child(child, "label", None)
      .expect("子刚加进去");
    document.push("panel", None);

    let walked: Vec<(usize, &str)> = document
      .walk()
      .map(|(depth, node)| (depth, node.kind()))
      .collect();
    assert_eq!(
      walked,
      vec![(0, "panel"), (1, "label"), (2, "label"), (0, "panel")]
    );
    assert_eq!(document.len(), 4, "含后代");

    // 身份挂在整条链上：同一个父下再加一个同类型的兄弟，与血缘上那个孙子不是同一个节点。
    let sibling = document.push_child(parent, "label", None).expect("父还在");
    assert_ne!(grandchild, sibling);
  }

  /// 父不在（身份来自上一份文档）时加子节点给 `None`，不 panic。
  #[test]
  fn push_child_needs_a_live_parent() {
    let mut document = Document::new();
    let ghost = Document::new().push("panel", Some("gone"));

    assert_eq!(document.push_child(ghost, "label", None), None);
    assert!(document.is_empty());
  }

  /// `.lxml` → 文档：标签成节点、嵌套成父子、**根元素也入文档**。
  #[test]
  fn from_lxml_builds_the_hierarchy() {
    let source = "<panel key=\"outer\" name=\"外框\" anchor=\"center\" width=\"400\" height=\"200\">\
                  <label key=\"inner\" anchor=\"bottom-right\" /></panel>";
    let (document, diagnostics) = from_lxml(source);

    assert!(diagnostics.is_empty(), "这份文档没有问题：{diagnostics:?}");
    assert_eq!(document.len(), 2);

    let (depth, root) = document.walk().next().expect("根在文档里");
    assert_eq!(depth, 0);
    assert_eq!(root.kind(), "panel");
    assert_eq!(root.number("width"), 400.0);
    assert_eq!(root.variant("anchor"), 4); // center
    assert_eq!(root.prop("name"), Some(&PropValue::Text("外框".to_owned())));

    let child = &root.children()[0];
    assert_eq!(child.kind(), "label");
    assert_eq!(child.variant("anchor"), 8); // bottom-right
  }

  /// 老仓的 `rect="x,y,宽,高"` 映射成四条数字属性。
  #[test]
  fn from_lxml_maps_the_rect_writing() {
    let (document, diagnostics) = from_lxml("<card rect=\"8,16,320,200\" />");

    assert!(diagnostics.is_empty(), "{diagnostics:?}");
    let node = &document.roots()[0];
    assert_eq!(node.number("x"), 8.0);
    assert_eq!(node.number("y"), 16.0);
    assert_eq!(node.number("width"), 320.0);
    assert_eq!(node.number("height"), 200.0);
  }

  /// 认不出的东西**不静默丢弃**：未知属性出 `Warning`，文本节点出 `Hint`；能装的照样装上。
  #[test]
  fn from_lxml_reports_what_it_could_not_map() {
    let (document, diagnostics) = from_lxml("<panel color=\"1,0,0\">正文</panel>");

    assert!(
      diagnostics.iter().any(|diagnostic| {
        diagnostic.severity == DiagnosticSeverity::Warning && diagnostic.message.contains("color")
      }),
      "未知属性该报 Warning：{diagnostics:?}"
    );
    assert!(
      diagnostics
        .iter()
        .any(|diagnostic| diagnostic.severity == DiagnosticSeverity::Hint),
      "文本节点该报 Hint：{diagnostics:?}"
    );
    assert_eq!(document.len(), 1, "报归报，节点还在");
    assert!(
      document.roots()[0].children().is_empty(),
      "文本不进文档的父子结构"
    );
  }
}
