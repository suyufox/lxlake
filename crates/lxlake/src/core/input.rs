//! 输入契约：**自有**的按键 / 鼠标枚举，与平台库解耦。
//!
//! 契约层不出现 winit 类型，按键自然也就自己定：`platform` 把原生键码翻译到这套，
//! 应用与相机只认这套。
//!
//! 枚举**故意只收常用键**——不必照着 winit 的 `KeyCode` 抄一遍；抄全了等于把平台的键位表
//! 变成框架的兼容负担。缺什么加什么。

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
  Escape,
}

/// 鼠标按键。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum MouseButton {
  Left,
  Right,
  Middle,
}
