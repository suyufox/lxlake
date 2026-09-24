//! 输入契约：**自有**的按键 / 鼠标枚举、键位表、意图状态，与平台库解耦。
//!
//! 契约层不出现 winit 类型，按键自然也就自己定：`platform` 把原生键码翻译到这套，
//! 应用与相机只认这套。
//!
//! 枚举**故意只收常用键**——不必照着 winit 的 `KeyCode` 抄一遍；抄全了等于把平台的键位表
//! 变成框架的兼容负担。缺什么加什么。
//!
//! 从设备事件到意图的链路（见 `docs/roadmap.md` 的「输入与模拟的分界」）：
//!
//! ```text
//!   Event::KeyboardInput / MouseButton → 输入源 → 键位表 → 意图（本文件的 IntentState）
//! ```
//!
//! 这一层**只认意图、不认世界**：知道世界坐标的是模拟层，产物是 [`crate::core::command`]。

use crate::core::command::Intent;

/// 键盘按键（按**物理位置**识别，不看当前键盘布局）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Key {
  W,
  A,
  S,
  D,
  Q,
  E,
  Space,
  /// 左右 Shift 合为一个——本层不区分左右手。
  Shift,
  /// 左右 Ctrl 合为一个。
  Control,
  /// Tab：M3 起用于开关调试面板（自绘 UI 的第一个「抢输入」场景）。
  Tab,
  Escape,
}

/// 鼠标按键。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum MouseButton {
  Left,
  Right,
  Middle,
}

/// 输入源：一个按键或一个鼠标键。
///
/// 键位表按它建索引——「左键破坏、右键放置」与「W 前进」因此是同一张表里的两行。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum InputSource {
  Key(Key),
  Mouse(MouseButton),
}

/// 键位表：一张 `输入源 → 意图` 的**数据表**。
///
/// 两层结构：**应用声明的默认键位 + 用户配置的覆盖**，覆盖优先。M2 只落默认这一层与「可覆盖」
/// 的形状——改键界面要等 M3 的自绘 HUD 才有地方放，键位的持久化与 M4 存档同源。
///
/// 做成数据而不是散在各处 `match` 里的硬编码，第三方应用因此能声明自己的默认键位。
#[derive(Debug, Clone, Default)]
pub struct Keymap {
  defaults: Vec<(InputSource, Intent)>,
  overrides: Vec<(InputSource, Intent)>,
}

impl Keymap {
  /// 空表。
  pub fn new() -> Self {
    Self::default()
  }

  /// 声明一条默认键位。链式调用，读起来就是一张表。
  pub fn bind(mut self, source: InputSource, intent: Intent) -> Self {
    self.defaults.push((source, intent));
    self
  }

  /// 应用一条用户覆盖；同一输入源反复覆盖时后来者胜。
  pub fn set_override(&mut self, source: InputSource, intent: Intent) {
    self.overrides.push((source, intent));
  }

  /// 查一个输入源对应的意图：**覆盖优先**，其次默认；表里没有就是 `None`。
  ///
  /// 同一层内后者胜（倒着找），这样一张表从上往下读就是「后面的声明可以改前面那条」。
  pub fn intent_for(&self, source: InputSource) -> Option<Intent> {
    self
      .overrides
      .iter()
      .rev()
      .chain(self.defaults.iter().rev())
      .find(|(bound, _)| *bound == source)
      .map(|&(_, intent)| intent)
  }
}

/// 意图状态：键位表把设备事件翻译成意图之后的落点。
///
/// - **轴意图**按「按住」记录（去重），每帧读一次，叠成轴值
/// - **动作意图**按「本帧新按下」排队，每帧排空一次——排空的时机是帧边界
#[derive(Debug, Default)]
pub struct IntentState {
  held: Vec<Intent>,
  actions: Vec<Intent>,
}

impl IntentState {
  /// 空状态。
  pub fn new() -> Self {
    Self::default()
  }

  /// 一个输入源的状态变化（按下 / 抬起）：查表 → 更新状态。表里没有的输入安静忽略。
  pub fn handle(&mut self, keymap: &Keymap, source: InputSource, pressed: bool) {
    let Some(intent) = keymap.intent_for(source) else {
      return;
    };

    if intent.is_axis() {
      if pressed {
        if !self.held.contains(&intent) {
          self.held.push(intent);
        }
      } else {
        self.held.retain(|held| *held != intent);
      }
    } else if pressed {
      self.actions.push(intent);
    }
  }

  /// 轴意图是否被按住。
  pub fn is_held(&self, intent: Intent) -> bool {
    self.held.contains(&intent)
  }

  /// 排空本帧的动作意图（按发生顺序）。
  pub fn take_actions(&mut self) -> Vec<Intent> {
    std::mem::take(&mut self.actions)
  }

  /// 丢掉全部状态：失焦时用——切出去时按着 W，切回来不该继续飞。
  pub fn clear(&mut self) {
    self.held.clear();
    self.actions.clear();
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  /// 一张最小表：W 前进、Shift 加速、左键破坏、Esc 退出。
  fn keymap() -> Keymap {
    Keymap::new()
      .bind(InputSource::Key(Key::W), Intent::MoveForward)
      .bind(InputSource::Key(Key::Shift), Intent::Boost)
      .bind(InputSource::Mouse(MouseButton::Left), Intent::Break)
      .bind(InputSource::Key(Key::Escape), Intent::Quit)
  }

  #[test]
  fn lookup_reads_both_kinds_of_source() {
    let keymap = keymap();
    assert_eq!(
      keymap.intent_for(InputSource::Key(Key::W)),
      Some(Intent::MoveForward)
    );
    assert_eq!(
      keymap.intent_for(InputSource::Mouse(MouseButton::Left)),
      Some(Intent::Break)
    );
    assert_eq!(keymap.intent_for(InputSource::Key(Key::Q)), None);
  }

  #[test]
  fn override_wins_over_default_and_later_override_wins() {
    let mut keymap = keymap();
    keymap.set_override(InputSource::Key(Key::W), Intent::Ascend);
    keymap.set_override(InputSource::Key(Key::W), Intent::Descend);

    assert_eq!(
      keymap.intent_for(InputSource::Key(Key::W)),
      Some(Intent::Descend),
      "覆盖优先，且后来的覆盖改掉前一条"
    );
    assert_eq!(
      keymap.intent_for(InputSource::Key(Key::Shift)),
      Some(Intent::Boost),
      "没被覆盖的默认键位照旧"
    );
  }

  #[test]
  fn axis_intents_are_held_and_released_once() {
    let mut state = IntentState::new();
    let keymap = keymap();

    state.handle(&keymap, InputSource::Key(Key::W), true);
    // 长按的重复按下不该在状态里堆两份。
    state.handle(&keymap, InputSource::Key(Key::W), true);
    assert!(state.is_held(Intent::MoveForward));

    state.handle(&keymap, InputSource::Key(Key::W), false);
    assert!(!state.is_held(Intent::MoveForward));
  }

  #[test]
  fn action_intents_queue_on_press_only() {
    let mut state = IntentState::new();
    let keymap = keymap();

    state.handle(&keymap, InputSource::Mouse(MouseButton::Left), true);
    state.handle(&keymap, InputSource::Mouse(MouseButton::Left), false);
    // 抬起不是一次新动作。
    assert_eq!(state.take_actions(), vec![Intent::Break]);
    assert!(state.take_actions().is_empty(), "排空过就没了");
  }

  #[test]
  fn unmapped_input_is_ignored() {
    let mut state = IntentState::new();
    state.handle(&keymap(), InputSource::Key(Key::Q), true);
    // 表里没有 Q：既不排队，也不该被当成某个轴按住。
    assert!(state.take_actions().is_empty());
    assert!(!state.is_held(Intent::MoveForward) && !state.is_held(Intent::Boost));
  }

  #[test]
  fn clear_drops_held_state() {
    let mut state = IntentState::new();
    let keymap = keymap();
    state.handle(&keymap, InputSource::Key(Key::W), true);
    state.handle(&keymap, InputSource::Key(Key::Escape), true);

    state.clear();
    assert!(!state.is_held(Intent::MoveForward), "失焦后不该还按着 W");
    assert!(state.take_actions().is_empty(), "失焦时排队里的动作也丢掉");
  }
}
