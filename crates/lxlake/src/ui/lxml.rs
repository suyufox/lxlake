//! `.lxml` 文本 IR：解析器与它的产物。
//!
//! 第一份**来自磁盘**的界面表达。它只产出一棵与几何 / 颜色无关的 [`StaticNode`] 树：`rect` /
//! `color` 只做**格式校验**并产出诊断，原始字符串原样留存，留给文档层去解释。于是这个模块
//! 不依赖 `lxlake` 的任何类型，将来 CLI 的静态校验也能单独复用它（见 `docs/architecture.md`）。
//!
//! **纯 CPU**：不含任何 GPU 类型，也不加 feature 门（与 `ui::doc` 同）。
//!
//! 钉住的行为（前四条是踩过坑的）：
//!
//! - `<!-- ... -->` 注释**跳过**，不是节点（历史上曾把 `!--` 误当成标签名）。
//! - 标签之间的文本存成 `tag == "#text"` 的节点，文本本体放在 `attrs` 的 `text` 键里。
//! - 有错也**尽力返回一棵树**（可能不完整），问题本身写进诊断而不是 `Err`。
//! - 诊断的行列是 **1-based**，按字符计；`length` 是覆盖的字符数。
//! - `rect` 认 4 个数字、`color` 认 3 或 4 个数字；不合法只出 `Warning`——结构仍可解析，
//!   节点照常生成。
//!
//! **还没打通的那一头**：`Document` ↔ `.lxml` 的读写需要父子层级（`StaticNode::children` 要
//! 映射到 `DocNode` 的父子关系），与切片 3 的层级容器一起做才不返工（见计划文件）。

/// 解析产出的 UI 树节点（与渲染无关）。
///
/// 既是编译期宏的产物形态，也是 [`parse_lxml`] 运行期的产物形态——CLI 与编辑器后端共用它做
/// 静态校验。
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct StaticNode {
  /// 标签名（`panel` / `card` / `label` / `#text` 等）。
  pub tag: String,
  /// 稳定身份：`key` 属性结构化提取出来的一份，同时**仍原样留在 [`StaticNode::attrs`] 里**。
  pub key: Option<String>,
  /// 原始属性（键 → 原始字符串值，`rect` / `color` 尚未解析成具体类型）。
  pub attrs: Vec<(String, String)>,
  /// 子节点（元素或 `#text` 文本节点）。
  pub children: Vec<StaticNode>,
}

impl StaticNode {
  /// 取某个属性名的原始值；没写就是 `None`。
  pub fn attr(&self, key: &str) -> Option<&str> {
    self
      .attrs
      .iter()
      .find(|(k, _)| k == key)
      .map(|(_, v)| v.as_str())
  }
}

/// 诊断严重级别。三个变体与 LSP 的数值一一对应（见 [`DiagnosticSeverity::lsp_code`]）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DiagnosticSeverity {
  /// 错误：无法继续解析 / 结构非法。
  Error,
  /// 警告：结构可解析，但语义可疑（如属性值格式不对）。
  Warning,
  /// 提示：可选改进建议。
  Hint,
}

impl DiagnosticSeverity {
  /// 对应的 LSP 数值（1 = Error，2 = Warning，4 = Hint；3 是 LSP 的 Information，本层不用）。
  pub fn lsp_code(self) -> u8 {
    match self {
      Self::Error => 1,
      Self::Warning => 2,
      Self::Hint => 4,
    }
  }
}

/// 一条解析诊断。**1-based 行列**（按字符计），编辑器可以直接拿去定位。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LxmlDiagnostic {
  /// 起始行（1-based）。**0 表示没有源位置**——语义阶段的诊断（属性映射、文本处理）不落在
  /// 某个字符上，见 [`LxmlDiagnostic::without_position`]。
  pub line: usize,
  /// 起始列（1-based，按字符计）。
  pub column: usize,
  /// 诊断覆盖的字符长度（0 表示单点）。
  pub length: usize,
  /// 严重级别。
  pub severity: DiagnosticSeverity,
  /// 面向用户的文案（中文，可直接展示在编辑器或 CLI 里）。
  pub message: String,
}

impl LxmlDiagnostic {
  /// 一条**没有源位置**的诊断（`line = column = 0`）。
  ///
  /// 给语义阶段用：属性映射、文本处理这些判断落在**节点或属性**上，而 [`StaticNode`] 只带属性值
  /// 与子节点，不带各自的坐标。将来要把它们指到某个字符上，得先让解析器把位置一起留下来。
  pub fn without_position(severity: DiagnosticSeverity, message: String) -> Self {
    Self {
      line: 0,
      column: 0,
      length: 0,
      severity,
      message,
    }
  }
}

/// 把 `.lxml` 源文本解析成一棵树，附带诊断。
///
/// 有错也**尽力返回**（可能不完整的）树——诊断描述问题在哪儿，而不是用一个 `Err` 把整份文档
/// 作废：编辑器要一边打字一边看结果，半份文档比没有文档有用。
pub fn parse_lxml(src: &str) -> (StaticNode, Vec<LxmlDiagnostic>) {
  let mut parser = Parser {
    chars: src.chars().peekable(),
    line: 1,
    column: 1,
  };
  let mut diagnostics = Vec::new();
  // 根级的纯文本与空白一律忽略（一份文档只认第一个标签作为根）。
  parser.skip_to_first_tag();
  let tree = parser.parse_element(&mut diagnostics).unwrap_or_default();
  (tree, diagnostics)
}

/// 递归下降解析器的状态：字符流 + 当前 1-based 行列。
struct Parser<'a> {
  chars: std::iter::Peekable<std::str::Chars<'a>>,
  line: usize,
  column: usize,
}

impl<'a> Parser<'a> {
  fn peek(&mut self) -> Option<char> {
    self.chars.peek().copied()
  }

  /// 消费一个字符并推进行列（`\n` 换行，列回到 1）。
  fn bump(&mut self) -> Option<char> {
    let c = self.chars.next();
    if let Some(ch) = c {
      if ch == '\n' {
        self.line += 1;
        self.column = 1;
      } else {
        self.column += 1;
      }
    }
    c
  }

  /// 当前 1-based 行列。
  fn here(&self) -> (usize, usize) {
    (self.line, self.column)
  }

  fn skip_ws(&mut self) {
    while matches!(self.peek(), Some(c) if c.is_whitespace()) {
      self.bump();
    }
  }

  /// 当前是否在 `<!--` 上（`<` 后紧跟 `!` 与 `-`）。
  fn peek_is_comment(&mut self) -> bool {
    match self.peek() {
      Some('<') => {
        let mut rest = self.chars.clone();
        rest.next(); // '<'
        matches!(rest.next(), Some('!')) && matches!(rest.peek().copied(), Some('-'))
      }
      _ => false,
    }
  }

  /// 消费一个 `<!-- ... -->`（调用前须已确认 [`Parser::peek_is_comment`]）。
  fn consume_comment(&mut self) {
    self.bump(); // '<'
    let mut prev = ['\0', '\0'];
    while let Some(c) = self.peek() {
      // `-->` 的判定要看前两个字符，所以要留一格历史。
      if prev[0] == '-' && prev[1] == '-' && c == '>' {
        self.bump(); // '>'
        return;
      }
      prev[0] = prev[1];
      prev[1] = c;
      self.bump();
    }
  }

  /// 跳到第一个 `<`（根级纯文本与空白忽略）。
  fn skip_to_first_tag(&mut self) {
    loop {
      match self.peek() {
        None | Some('<') => return,
        Some(_) => {
          self.bump();
        }
      }
    }
  }

  fn parse_element(&mut self, diagnostics: &mut Vec<LxmlDiagnostic>) -> Option<StaticNode> {
    self.skip_ws();
    // 注释不是节点：吃掉它，接着找下一个元素。
    if self.peek_is_comment() {
      self.consume_comment();
      return self.parse_element(diagnostics);
    }
    if self.peek() != Some('<') {
      return None;
    }
    self.bump(); // '<'

    // 元素入口不该遇到闭合标签；遇到就记一笔再跳过（于是兄弟元素仍能解析出来）。
    if self.peek() == Some('/') {
      let (line, column) = self.here();
      self.bump();
      self.consume_until('>');
      diagnostics.push(LxmlDiagnostic {
        line,
        column,
        length: 2,
        severity: DiagnosticSeverity::Error,
        message: "意外的闭合标签（缺少对应的开标签）".to_owned(),
      });
      return None;
    }

    let (open_line, open_column) = self.here();
    let name = self.parse_tag_name();
    let attrs = self.parse_attributes(diagnostics);
    let self_closing = if self.peek() == Some('/') {
      self.bump();
      true
    } else {
      false
    };
    // 消费 '>'。没有 '>' 也接着往下走——**尽力解析**比整棵树作废有用。
    if self.peek() == Some('>') {
      self.bump();
    } else {
      let (line, column) = self.here();
      self.consume_until('>');
      diagnostics.push(LxmlDiagnostic {
        line,
        column,
        length: 1,
        severity: DiagnosticSeverity::Error,
        message: format!("标签 `<{name}` 缺少结束的 `>`"),
      });
    }

    if self_closing {
      return Some(build_node(&name, &attrs, Vec::new(), diagnostics));
    }

    // 子节点，直到匹配的 `</name>`。
    let mut children = Vec::new();
    loop {
      self.skip_ws();
      match self.peek() {
        None => {
          diagnostics.push(LxmlDiagnostic {
            line: open_line,
            column: open_column,
            length: name.chars().count().max(1),
            severity: DiagnosticSeverity::Error,
            message: format!("标签 `<{name}>` 未闭合（到达文件结尾）"),
          });
          break;
        }
        Some('<') => {
          if self.peek_after_lt() == Some('/') {
            // 闭合标签：名字对不上也要把开标签收掉（否则错误会成串地冒出来）。
            let (close_line, close_column) = self.here();
            self.bump(); // '<'
            self.bump(); // '/'
            let close_name = self.parse_tag_name();
            if close_name != name {
              diagnostics.push(LxmlDiagnostic {
                line: close_line,
                column: close_column,
                length: close_name.chars().count().max(1) + 2,
                severity: DiagnosticSeverity::Error,
                message: format!("闭合标签 `</{close_name}>` 与开标签 `<{name}>` 不匹配"),
              });
            }
            self.consume_until('>');
            break;
          }
          // 嵌套元素。
          if let Some(child) = self.parse_element(diagnostics) {
            children.push(child);
          } else {
            break;
          }
        }
        Some(_) => {
          let text = self.consume_text_until_tag();
          let trimmed = text.trim();
          if !trimmed.is_empty() {
            children.push(StaticNode {
              tag: "#text".to_owned(),
              key: None,
              attrs: vec![("text".to_owned(), trimmed.to_owned())],
              children: Vec::new(),
            });
          }
        }
      }
    }

    Some(build_node(&name, &attrs, children, diagnostics))
  }

  /// `<' 之后的那一个字符，用来分辨嵌套元素与闭合标签（只看，不动位置）。
  fn peek_after_lt(&mut self) -> Option<char> {
    let mut rest = self.chars.clone();
    rest.next(); // '<'
    rest.peek().copied()
  }

  /// 标签名 / 闭合标签名：读到空白、`>`、`/`、`=` 为止。
  fn parse_tag_name(&mut self) -> String {
    let mut name = String::new();
    while let Some(c) = self.peek() {
      if c.is_whitespace() || c == '>' || c == '/' || c == '=' {
        break;
      }
      name.push(c);
      self.bump();
    }
    name
  }

  fn parse_attributes(&mut self, diagnostics: &mut Vec<LxmlDiagnostic>) -> Vec<Attr> {
    let mut attrs = Vec::new();
    loop {
      self.skip_ws();
      match self.peek() {
        None | Some('>') | Some('/') => break,
        _ => {}
      }
      // 属性名。
      let mut key = String::new();
      while let Some(c) = self.peek() {
        if c.is_whitespace() || c == '=' || c == '>' || c == '/' {
          break;
        }
        key.push(c);
        self.bump();
      }
      self.skip_ws();

      let mut value = String::new();
      let mut value_pos = self.here();
      if self.peek() == Some('=') {
        self.bump(); // '='
        self.skip_ws();
        if let Some(quote @ ('"' | '\'')) = self.peek() {
          let (quote_line, quote_column) = self.here();
          self.bump(); // 开引号
          value_pos = (quote_line, quote_column + 1);
          while let Some(c) = self.peek() {
            if c == quote {
              break;
            }
            if c == '>' || c == '\n' {
              // 引号在标签结束前没闭上：报一处，值就到此为止。
              diagnostics.push(LxmlDiagnostic {
                line: quote_line,
                column: quote_column,
                length: 1,
                severity: DiagnosticSeverity::Error,
                message: format!("属性 `{key}` 的引号未闭合"),
              });
              break;
            }
            value.push(c);
            self.bump();
          }
          if self.peek() == Some(quote) {
            self.bump(); // 闭引号
          }
        } else {
          // 无引号值：读到空白 / `>` / `/` 为止。
          while let Some(c) = self.peek() {
            if c.is_whitespace() || c == '>' || c == '/' {
              break;
            }
            value.push(c);
            self.bump();
          }
        }
      }

      if !key.is_empty() {
        attrs.push(Attr {
          key,
          value,
          value_pos,
        });
      }
    }
    attrs
  }

  /// 消费到 `delim` 为止（含 `delim`）；没遇到就一路吃到头。
  fn consume_until(&mut self, delim: char) {
    while let Some(c) = self.peek() {
      if c == delim {
        self.bump();
        return;
      }
      self.bump();
    }
  }

  /// 消费到下一个 `<` 为止（不含它）。
  fn consume_text_until_tag(&mut self) -> String {
    let mut text = String::new();
    while let Some(c) = self.peek() {
      if c == '<' {
        break;
      }
      text.push(c);
      self.bump();
    }
    text
  }
}

/// 解析出的一个属性。带值起点的位置，是为了坏值能精确指到那一格。
#[derive(Clone, Debug)]
struct Attr {
  key: String,
  value: String,
  /// 值起点（开引号之后）的 1-based 行列。
  value_pos: (usize, usize),
}

/// 属性列表 + 子节点 → 一个节点。
///
/// `key` 顺手结构化出来（仍原样留在 `attrs` 里）；`rect` / `color` 只做格式校验——原始字符串
/// 一律原样留存，解释它们（是像素还是比例、是 RGBA 还是 RGB）是文档层的事。
fn build_node(
  name: &str,
  attrs: &[Attr],
  children: Vec<StaticNode>,
  diagnostics: &mut Vec<LxmlDiagnostic>,
) -> StaticNode {
  let mut node = StaticNode {
    tag: name.to_owned(),
    key: None,
    attrs: Vec::with_capacity(attrs.len()),
    children,
  };
  for attr in attrs {
    match attr.key.as_str() {
      "rect" => validate_numbers(&attr.value, 4, attr.value_pos, diagnostics),
      "color" => validate_numbers(&attr.value, 3, attr.value_pos, diagnostics),
      "key" => node.key = Some(attr.value.clone()),
      _ => {}
    }
    node.attrs.push((attr.key.clone(), attr.value.clone()));
  }
  node
}

/// 校验分量个数与「每个分量都是数字」。不合法只出 [`DiagnosticSeverity::Warning`]——结构层面
/// 没问题，节点照常生成，由文档层决定要不要用这个值。
fn validate_numbers(
  value: &str,
  expect: usize,
  value_pos: (usize, usize),
  diagnostics: &mut Vec<LxmlDiagnostic>,
) {
  let parsed = value
    .split(',')
    .filter(|part| !part.trim().is_empty())
    .filter(|part| part.trim().parse::<f32>().is_ok())
    .count();
  // `color` 多认一个分量：RGB 与 RGBA 都算合法。
  let ok = match expect {
    4 => parsed == 4,
    3 => parsed == 3 || parsed == 4,
    _ => parsed == expect,
  };
  if !ok {
    diagnostics.push(LxmlDiagnostic {
      line: value_pos.0,
      column: value_pos.1,
      length: value.chars().count(),
      severity: DiagnosticSeverity::Warning,
      message: format!("属性值应为 {expect} 个以逗号分隔的数字，收到 `{value}`"),
    });
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  /// 注释跳过：既不进树，也不出诊断（历史上曾把 `!--` 误当成标签名）。
  #[test]
  fn comment_is_skipped() {
    let src = "<!-- 注释 --><panel rect=\"0,0,1,1\" color=\"0.1,0.2,0.3\"/>";
    let (tree, diagnostics) = parse_lxml(src);

    assert!(diagnostics.is_empty(), "注释不该产生诊断：{diagnostics:?}");
    assert_eq!(tree.tag, "panel");
    assert_eq!(tree.attr("rect"), Some("0,0,1,1"));
    assert_eq!(tree.attr("color"), Some("0.1,0.2,0.3"));
  }

  /// 嵌套 + 自闭合。
  #[test]
  fn nested_and_self_closing() {
    let src =
      "<panel rect=\"0,0,400,300\"><card rect=\"1,1,2,2\"/><card rect=\"3,3,4,4\"/></panel>";
    let (tree, diagnostics) = parse_lxml(src);

    assert!(
      diagnostics.is_empty(),
      "合法文档不该有诊断：{diagnostics:?}"
    );
    assert_eq!(tree.tag, "panel");
    assert_eq!(tree.children.len(), 2);
    assert_eq!(tree.children[0].tag, "card");
    assert_eq!(tree.children[1].attr("rect"), Some("3,3,4,4"));
  }

  /// 坏的 `rect` 只 Warning、不 Error：结构仍解析得出来。
  #[test]
  fn malformed_rect_is_warning_not_error() {
    let src = "<panel rect=\"not,a,rect,here\"/>";
    let (tree, diagnostics) = parse_lxml(src);

    assert!(!diagnostics.is_empty());
    assert_eq!(diagnostics[0].severity, DiagnosticSeverity::Warning);
    assert_eq!(tree.tag, "panel", "结构仍可解析");
  }

  /// 标签间的文本存成 `#text` 节点，本体在 `attrs` 的 `text` 键里。
  #[test]
  fn text_node_becomes_hash_text() {
    let src = "<label>Hello</label>";
    let (tree, _diagnostics) = parse_lxml(src);

    assert_eq!(tree.tag, "label");
    assert_eq!(tree.children.len(), 1);
    assert_eq!(tree.children[0].tag, "#text");
    assert_eq!(tree.children[0].attr("text"), Some("Hello"));
  }

  /// 未闭合的标签报 Error（诊断指回开标签那一格）。
  #[test]
  fn unclosed_tag_is_error() {
    let src = "<panel>";
    let (_tree, diagnostics) = parse_lxml(src);

    assert!(
      diagnostics
        .iter()
        .any(|diagnostic| diagnostic.severity == DiagnosticSeverity::Error)
    );
  }

  /// 有错也尽力返回一棵树：兄弟元素照旧解析出来。
  #[test]
  fn a_broken_node_does_not_take_down_its_siblings() {
    let src = "<panel><card rect=\"bad\"/><label>Hi</label></panel>";
    let (tree, diagnostics) = parse_lxml(src);

    assert_eq!(tree.children.len(), 2, "坏属性不该吃掉后面的兄弟");
    assert_eq!(tree.children[1].tag, "label");
    assert_eq!(diagnostics.len(), 1);
    assert_eq!(diagnostics[0].severity, DiagnosticSeverity::Warning);
  }

  /// 诊断的行列是 1-based：坏 `rect` 指在值起点那一格上。
  #[test]
  fn diagnostics_carry_one_based_positions() {
    // 第一行 `<panel`，`rect` 的值在第 2 行的第 9 列（开引号在第 8 列，值起点在它之后）。
    let src = "<panel\n  rect=\"x\"/>";
    let (_tree, diagnostics) = parse_lxml(src);

    assert_eq!(diagnostics.len(), 1);
    assert_eq!(diagnostics[0].line, 2);
    assert_eq!(diagnostics[0].column, 9);
    assert_eq!(diagnostics[0].length, 1);
  }

  /// `color` 认 3 个或 4 个分量，`rect` 只认 4 个。
  #[test]
  fn color_accepts_rgb_and_rgba() {
    for color in ["1,2,3", "1,2,3,4"] {
      let src = format!("<panel color=\"{color}\"/>");
      let (_tree, diagnostics) = parse_lxml(&src);
      assert!(diagnostics.is_empty(), "`{color}` 该认：{diagnostics:?}");
    }
  }

  /// 严重级别与 LSP 数值的对应关系（编辑器直接拿它决定提示形态）。
  #[test]
  fn severities_map_to_lsp_codes() {
    assert_eq!(DiagnosticSeverity::Error.lsp_code(), 1);
    assert_eq!(DiagnosticSeverity::Warning.lsp_code(), 2);
    assert_eq!(DiagnosticSeverity::Hint.lsp_code(), 4);
  }
}
