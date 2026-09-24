//! 窗口身份与注册表。
//!
//! 窗口在运行时里是**一等对象**：一个 [`Window`] 带着自己的标签与平台句柄，注册表按 id 与标签
//! 双表反查。应用侧一律用标签找窗（`app.window("inspector")`），id 只在事件与运行时内部流转。
//!
//! 本文件不认识 winit：句柄是 [`WindowHandle`]（见 `core::window`），平台在 `platform` 里实现它。
//!
//! 命名提醒：这里的 [`Window`] 是**框架的一等窗口对象**，与 `winit::window::Window` 同名但完全
//! 不同层——后者只在 `platform` 后端里以内层字段的形式出现。

use crate::core::window::{WindowHandle, WindowId, WindowLabel};
use std::collections::BTreeMap;
use std::sync::Arc;

/// 框架的窗口对象：身份（id + 标签）+ 平台句柄。
pub struct Window {
  id: WindowId,
  label: WindowLabel,
  handle: Arc<dyn WindowHandle>,
}

impl Window {
  pub fn id(&self) -> WindowId {
    self.id
  }

  /// 应用写的名字（主窗口恒为 `main`）。
  pub fn label(&self) -> &WindowLabel {
    &self.label
  }

  /// 平台句柄：改标题、抓光标、建渲染表面都从它走。
  pub fn handle(&self) -> &Arc<dyn WindowHandle> {
    &self.handle
  }
}

/// 窗口注册表：按 id 与标签双表反查。
///
/// id 由运行时分配（建窗顺序），标签由应用声明（装配期已校验唯一、且不占用保留标签）。
pub struct WindowRegistry {
  /// 主窗口的 id：标签为 `main` 的那一个。
  main: Option<WindowId>,
  by_id: BTreeMap<WindowId, Window>,
  by_label: BTreeMap<String, WindowId>,
}

impl WindowRegistry {
  pub(crate) fn new() -> Self {
    Self {
      main: None,
      by_id: BTreeMap::new(),
      by_label: BTreeMap::new(),
    }
  }

  /// 登记一个窗口；标签为 `main` 的即主窗口。
  ///
  /// 标签唯一性由装配期校验保证（见 `runtime::validate_windows`），这里不再判、也不 panic。
  pub(crate) fn insert(
    &mut self,
    id: WindowId,
    label: &WindowLabel,
    handle: Arc<dyn WindowHandle>,
  ) {
    if label.is_main() {
      self.main = Some(id);
    }
    self.by_label.insert(label.0.clone(), id);
    self.by_id.insert(
      id,
      Window {
        id,
        label: label.clone(),
        handle,
      },
    );
  }

  /// 摘掉一个窗口，返回它。
  pub(crate) fn remove(&mut self, id: WindowId) -> Option<Window> {
    let window = self.by_id.remove(&id)?;
    self.by_label.remove(window.label.as_str());
    if self.main == Some(id) {
      self.main = None;
    }
    Some(window)
  }

  pub(crate) fn clear(&mut self) {
    self.main = None;
    self.by_id.clear();
    self.by_label.clear();
  }

  /// 主窗口。
  pub fn main(&self) -> Option<&Window> {
    self.main.and_then(|id| self.by_id.get(&id))
  }

  /// 按标签取窗。
  pub fn by_label(&self, label: &str) -> Option<&Window> {
    self.by_label.get(label).and_then(|id| self.by_id.get(id))
  }

  /// 按 id 取窗。
  pub fn by_id(&self, id: WindowId) -> Option<&Window> {
    self.by_id.get(&id)
  }

  /// 全部窗口，按 id 升序（即建窗顺序）。
  pub fn iter(&self) -> impl Iterator<Item = &Window> {
    self.by_id.values()
  }

  /// 窗口数量。
  pub fn len(&self) -> usize {
    self.by_id.len()
  }

  /// 是否一个窗口都没有。
  pub fn is_empty(&self) -> bool {
    self.by_id.is_empty()
  }
}

#[cfg(test)]
mod tests {
  use super::*;
  use crate::core::geometry::PhysicalSize;
  use raw_window_handle::{DisplayHandle, HandleError, HasDisplayHandle, HasWindowHandle};

  /// 只记 id 的假句柄：注册表与句柄内容无关。
  struct FakeWindow(WindowId);

  impl HasWindowHandle for FakeWindow {
    fn window_handle(&self) -> Result<raw_window_handle::WindowHandle<'_>, HandleError> {
      Err(HandleError::Unavailable)
    }
  }

  impl HasDisplayHandle for FakeWindow {
    fn display_handle(&self) -> Result<DisplayHandle<'_>, HandleError> {
      Err(HandleError::Unavailable)
    }
  }

  impl WindowHandle for FakeWindow {
    fn id(&self) -> WindowId {
      self.0
    }

    fn size(&self) -> PhysicalSize {
      PhysicalSize::new(0, 0)
    }

    fn scale_factor(&self) -> f64 {
      1.0
    }

    fn set_title(&self, _title: &str) {}

    fn set_cursor_grab(&self, _grab: bool) {}

    fn set_cursor_visible(&self, _visible: bool) {}
  }

  fn handle(id: u64) -> Arc<dyn WindowHandle> {
    Arc::new(FakeWindow(WindowId(id)))
  }

  fn registry_with_two() -> WindowRegistry {
    let mut registry = WindowRegistry::new();
    registry.insert(WindowId(0), &WindowLabel::new(WindowLabel::MAIN), handle(0));
    registry.insert(WindowId(1), &WindowLabel::new("inspector"), handle(1));
    registry
  }

  #[test]
  fn both_tables_point_at_the_same_window() {
    let registry = registry_with_two();

    let by_label = registry.by_label("inspector").expect("按标签找得到");
    let by_id = registry.by_id(WindowId(1)).expect("按 id 找得到");

    assert_eq!(by_label.id(), by_id.id());
    assert_eq!(by_label.label().as_str(), "inspector");
    assert_eq!(registry.len(), 2);
  }

  #[test]
  fn the_main_window_is_the_one_labelled_main() {
    let registry = registry_with_two();

    let main = registry.main().expect("有主窗口");
    assert_eq!(main.id(), WindowId(0));
    assert!(main.label().is_main(), "主窗口的标签就是保留标签");
    assert_eq!(
      registry.by_label(WindowLabel::MAIN).map(Window::id),
      Some(WindowId(0))
    );
  }

  #[test]
  fn iterating_is_ordered_by_creation() {
    let registry = registry_with_two();
    let ids: Vec<u64> = registry.iter().map(|window| window.id().0).collect();

    assert_eq!(ids, vec![0, 1]);
  }

  /// 摘窗要**两张表一起摘**，否则会出现「按标签找得到、按 id 找不到」的鬼影。
  #[test]
  fn removing_drops_both_keys() {
    let mut registry = registry_with_two();

    let removed = registry.remove(WindowId(1)).expect("摘得掉");
    assert_eq!(removed.label().as_str(), "inspector");
    assert!(registry.by_label("inspector").is_none());
    assert!(registry.by_id(WindowId(1)).is_none());
    assert_eq!(registry.len(), 1);

    assert!(registry.remove(WindowId(1)).is_none(), "摘两次是空手");
  }

  #[test]
  fn removing_the_main_window_clears_the_main_slot() {
    let mut registry = registry_with_two();

    registry.remove(WindowId(0));

    assert!(registry.main().is_none());
    assert!(registry.by_label(WindowLabel::MAIN).is_none());
    assert!(!registry.is_empty(), "另一个窗口还在");
  }

  #[test]
  fn clearing_leaves_an_empty_registry() {
    let mut registry = registry_with_two();

    registry.clear();

    assert!(registry.is_empty());
    assert_eq!(registry.len(), 0);
    assert!(registry.main().is_none());
    assert!(registry.iter().next().is_none());
  }
}
