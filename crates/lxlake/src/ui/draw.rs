//! 自绘方片：UI 层的**唯一出图形式**。
//!
//! 自绘 UI 的画法只有一种——往屏幕上贴方片。面板底色是实心方片，文字是字形覆盖度方片，
//! 将来的边框、图标、进度条同理。形式收窄到一种，渲染侧就只需一条管线、一份顶点布局
//! （见 `render::ui`），不必为每种控件开一条路。

use crate::core::geometry::LogicalRect;
use crate::ui::text::GlyphKey;

/// 方片从哪儿取色。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QuadSource {
  /// 实心：不采纹理，直接用 [`Quad::color`]。
  Solid,
  /// 字形覆盖度：从字形图集的这一格取 alpha，RGB 仍用 [`Quad::color`]。
  Glyph(GlyphKey),
}

/// 一条自绘方片，坐标是**逻辑像素**。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Quad {
  pub rect: LogicalRect,
  /// sRGB 0..255。`a` 是整体不透明度，字形还要再乘覆盖度。
  pub color: [u8; 4],
  pub source: QuadSource,
}

impl Quad {
  /// 实心方片：HUD 的面板底色、准星、分隔线都是它。
  pub fn solid(rect: LogicalRect, color: [u8; 4]) -> Self {
    Self {
      rect,
      color,
      source: QuadSource::Solid,
    }
  }

  /// 字形方片：`key` 指向字形图集里的一格。
  pub fn glyph(key: GlyphKey, rect: LogicalRect, color: [u8; 4]) -> Self {
    Self {
      rect,
      color,
      source: QuadSource::Glyph(key),
    }
  }
}
