//! 文本：排版与字形光栅化。**纯 CPU**——不含任何 GPU 类型。
//!
//! 与 [`crate::ui`] 里布局的分工：布局管「Widget 摆在哪块矩形」，本模块管「矩形里那些字怎么
//! 变成方片」。产出是 [`Quad`]，与面板底色、准星走同一个出图形式（见 `ui::draw`），渲染侧因此
//! 只需一条管线。
//!
//! **清晰度的关键**：字形按 `字号 × DPI` 的**物理分辨率**光栅化，再除回逻辑像素交出矩形。
//! 若按逻辑分辨率光栅化，高 DPI 屏上的字会被系统放大糊成一片。代价是同一个字在不同 DPI 下
//! 是两份位图，所以缓存键 [`GlyphKey`] 里带的是**物理尺寸**而非逻辑字号。
//!
//! 本模块刻意不做的事：自动换行、对齐、字距调整、双向文本整形。M3 的 HUD 只需要按 `'\n'`
//! 硬分行、从左到右排——真要做排版是独立的一刀，不该先长在这里。

use crate::core::geometry::{LogicalPosition, LogicalRect, LogicalSize, sanitize_scale};
use crate::ui::draw::Quad;
use ab_glyph::{Font, FontVec, GlyphId, PxScale, ScaleFont, point};
use std::collections::HashMap;
use std::collections::hash_map::Entry;
use std::fmt;

/// 一段文本的样式。两个尺寸都是**逻辑像素**。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TextStyle {
  /// 字号，`ab_glyph` 的 `PxScale` 语义（大致等于 em 高度）。
  pub size: f32,
  /// 行高：**基线到基线**的距离，不是行盒高度。
  pub line_height: f32,
}

impl TextStyle {
  pub const fn new(size: f32, line_height: f32) -> Self {
    Self { size, line_height }
  }
}

/// 字形图集里的一格。
///
/// `size_px` 是**物理**像素尺寸（取整），故这个键天然按 DPI 分档：换显示器或改缩放后，
/// 同一个字会是新的键、重新光栅化一次，而不是拿旧尺寸的位图拉大凑合。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct GlyphKey {
  /// 字形 id（`ab_glyph::GlyphId` 的裸值）。
  pub glyph: u16,
  /// 光栅化时的物理像素尺寸。
  pub size_px: u16,
}

/// 一个光栅化好的字形位图：8 位覆盖度，`width × height` 行优先。
#[derive(Debug, Clone, PartialEq)]
pub struct GlyphBitmap {
  pub key: GlyphKey,
  /// 位图左上角相对**笔位**（基线原点）的偏移，物理像素，y 向下为正。
  /// 左边界可能是负的——斜体、`f` 这类字的轮廓会探到笔位左边。
  pub offset: [f32; 2],
  pub width: u32,
  pub height: u32,
  /// 逐像素覆盖度（0 = 完全没盖住，255 = 完全盖住），长度 = `width * height`。
  pub coverage: Vec<u8>,
}

/// 文本侧错误。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TextError {
  /// 字体数据无法解析：不是 ttf/otf，或字库本身损坏。
  Font(String),
}

impl fmt::Display for TextError {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    match self {
      TextError::Font(message) => write!(f, "字体错误：{message}"),
    }
  }
}

impl std::error::Error for TextError {}

/// 排版器：持有字体，缓存已光栅化的字形。
#[derive(Debug)]
pub struct TextShaper {
  font: FontVec,
  /// 已光栅化的字形。**只增不减**——字形的种类被 UI 文案限死，不会无限膨胀。
  cache: HashMap<GlyphKey, GlyphBitmap>,
  /// 本次新光栅化、还没交给渲染侧上传的字形。
  fresh: Vec<GlyphKey>,
}

impl TextShaper {
  /// 从字体文件字节建排版器。字体由应用提供（首个外部资产，见 `data/fonts/`）。
  pub fn from_bytes(bytes: Vec<u8>) -> Result<Self, TextError> {
    let font = FontVec::try_from_vec(bytes).map_err(|err| TextError::Font(err.to_string()))?;
    Ok(Self {
      font,
      cache: HashMap::new(),
      fresh: Vec::new(),
    })
  }

  /// 排版一段文本，产出**逻辑像素**的字形方片。
  ///
  /// `origin` 是文本块左上角（不是基线）。只按 `'\n'` 分行；不可见的字形（空格）占位但不产出
  /// 方片。`scale_factor` 参与字形光栅化，但产出的矩形仍旧是逻辑像素。
  pub fn layout(
    &mut self,
    text: &str,
    style: TextStyle,
    origin: LogicalPosition,
    color: [u8; 4],
    scale_factor: f64,
  ) -> Vec<Quad> {
    let scale_factor = sanitize_scale(scale_factor);
    let size_px = size_in_px(style.size, scale_factor);
    let scale = PxScale::from(f32::from(size_px));

    // 字段级拆分借用：`as_scaled` 不可变借走字体，缓存要可变借——拆开才不打架。
    let Self { font, cache, fresh } = self;
    let scaled = font.as_scaled(scale);
    let mut quads = Vec::new();
    let mut baseline = origin.y + f64::from(scaled.ascent()) / scale_factor;

    for (index, line) in text.split('\n').enumerate() {
      if index > 0 {
        baseline += f64::from(style.line_height);
      }
      let mut pen_x = origin.x;

      for ch in line.chars() {
        let id = font.glyph_id(ch);
        let key = GlyphKey {
          glyph: id.0,
          size_px,
        };
        let bitmap = match cache.entry(key) {
          Entry::Occupied(slot) => slot.into_mut(),
          Entry::Vacant(slot) => {
            let bitmap = rasterize(font, key);
            // 空位图（空格这类没有轮廓的字）不入上传队列，但留在缓存里——
            // 免得每帧都白试一遍「这个字有没有轮廓」。
            if bitmap.width > 0 && bitmap.height > 0 {
              fresh.push(key);
            }
            slot.insert(bitmap)
          }
        };

        if bitmap.width > 0 && bitmap.height > 0 {
          let rect = LogicalRect::new(
            pen_x + f64::from(bitmap.offset[0]) / scale_factor,
            baseline + f64::from(bitmap.offset[1]) / scale_factor,
            f64::from(bitmap.width) / scale_factor,
            f64::from(bitmap.height) / scale_factor,
          );
          quads.push(Quad::glyph(key, rect, color));
        }
        pen_x += f64::from(scaled.h_advance(id)) / scale_factor;
      }
    }
    quads
  }

  /// 量一段文本占多大（逻辑像素）。空串量出 0；只测不画，所以不用 `&mut self`。
  pub fn measure(&self, text: &str, style: TextStyle, scale_factor: f64) -> LogicalSize {
    if text.is_empty() {
      return LogicalSize::new(0.0, 0.0);
    }
    let scale_factor = sanitize_scale(scale_factor);
    let size_px = size_in_px(style.size, scale_factor);
    let scaled = self.font.as_scaled(PxScale::from(f32::from(size_px)));

    let mut lines = 0usize;
    let mut width = 0.0f64;
    for line in text.split('\n') {
      lines += 1;
      let mut pen_x = 0.0f64;
      for ch in line.chars() {
        pen_x += f64::from(scaled.h_advance(self.font.glyph_id(ch))) / scale_factor;
      }
      width = width.max(pen_x);
    }
    LogicalSize::new(width, lines as f64 * f64::from(style.line_height))
  }

  /// 交出本次新光栅化的字形，供渲染侧增量上传；交出后即清空。
  ///
  /// 增量而非全量：字形位图是这几刀里唯一的「大块 CPU 数据」，每帧重传整个图集纯属浪费。
  pub fn take_new_glyphs(&mut self) -> Vec<GlyphBitmap> {
    let keys = std::mem::take(&mut self.fresh);
    keys
      .into_iter()
      .filter_map(|key| self.cache.get(&key).cloned())
      .collect()
  }
}

/// 字号换算到**物理**像素并取整，下限 1。
///
/// 取整是为了让缓存键稳定：`16.0000001` 和 `15.9999999` 不该是两格。下限 1 是因为
/// `PxScale(0.0)` 会让 ab_glyph 光栅化出空位图，症状是「字突然全没了」。
fn size_in_px(size: f32, scale_factor: f64) -> u16 {
  let px = f64::from(size) * scale_factor;
  px.round().clamp(1.0, f64::from(u16::MAX)) as u16
}

/// 光栅化一格字形。笔位取原点，于是位图偏移就是相对笔位的偏移。
fn rasterize(font: &FontVec, key: GlyphKey) -> GlyphBitmap {
  let glyph = GlyphId(key.glyph).with_scale_and_position(f32::from(key.size_px), point(0.0, 0.0));
  let Some(outlined) = font.outline_glyph(glyph) else {
    return GlyphBitmap {
      key,
      offset: [0.0, 0.0],
      width: 0,
      height: 0,
      coverage: Vec::new(),
    };
  };

  let bounds = outlined.px_bounds();
  let (width, height) = (bounds.width() as usize, bounds.height() as usize);
  if width == 0 || height == 0 {
    return GlyphBitmap {
      key,
      offset: [0.0, 0.0],
      width: 0,
      height: 0,
      coverage: Vec::new(),
    };
  }

  // `draw` 的回调坐标以位图左上角为原点、行优先。越界保护留着是保险：ab_glyph 的
  // 「保守取整」边界在极端字号下未必与位图尺寸逐像素对齐。
  let mut coverage = vec![0u8; width * height];
  outlined.draw(|x, y, value| {
    let (x, y) = (x as usize, y as usize);
    if x < width && y < height {
      coverage[y * width + x] = (value.clamp(0.0, 1.0) * 255.0).round() as u8;
    }
  });

  GlyphBitmap {
    key,
    offset: [bounds.min.x, bounds.min.y],
    width: width as u32,
    height: height as u32,
    coverage,
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  /// 字体随仓库提供（OFL，见 `data/fonts/`）。单测因此能钉住真实的排版行为，而不是假字体。
  const FONT: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../data/fonts/NotoSansSC-Regular.otf"
  );
  const STYLE: TextStyle = TextStyle::new(16.0, 20.0);

  fn shaper() -> TextShaper {
    let bytes = std::fs::read(FONT).expect("字体应随仓库提供");
    TextShaper::from_bytes(bytes).expect("字体应能解析")
  }

  #[test]
  fn rejects_garbage_font_data() {
    let error = TextShaper::from_bytes(vec![0, 1, 2, 3]).unwrap_err();
    assert!(matches!(error, TextError::Font(_)), "应报字体错误：{error}");
  }

  #[test]
  fn empty_text_measures_nothing() {
    let shaper = shaper();
    assert_eq!(shaper.measure("", STYLE, 1.0), LogicalSize::new(0.0, 0.0));
  }

  /// 汉字是全角、字母是半角：同样字数，中文更宽。这条钉住「用的是真实字体度量」。
  #[test]
  fn cjk_is_wider_than_ascii_at_the_same_char_count() {
    let shaper = shaper();
    let cjk = shaper.measure("中文字", STYLE, 1.0);
    let ascii = shaper.measure("abc", STYLE, 1.0);

    assert!(cjk.width > ascii.width, "中文 {cjk:?} 应宽于 {ascii:?}");
    assert!(ascii.width > 0.0);
    // 单行的高度就是行高，与内容无关。
    assert_eq!(cjk.height, f64::from(STYLE.line_height));
  }

  #[test]
  fn two_lines_are_one_line_height_apart() {
    let mut shaper = shaper();
    // 同一个字两行：除基线外一切相同，差值应当正好是一倍行高。
    let quads = shaper.layout("A\nA", STYLE, LogicalPosition::new(0.0, 0.0), [255; 4], 1.0);

    assert_eq!(quads.len(), 2);
    assert_eq!(
      quads[1].rect.y - quads[0].rect.y,
      f64::from(STYLE.line_height)
    );
    assert_eq!(quads[0].rect.x, quads[1].rect.x, "换行不该挪水平位置");
  }

  /// 空格占位但不画：字形方片只有可见的两个字。
  #[test]
  fn spaces_advance_but_draw_nothing() {
    let mut shaper = shaper();
    let quads = shaper.layout("A A", STYLE, LogicalPosition::new(0.0, 0.0), [255; 4], 1.0);

    assert_eq!(quads.len(), 2, "空格不该产出方片");
    assert!(quads[1].rect.x > quads[0].rect.x, "空格要占掉前进量");
  }

  /// 缓存命中：第二次排同一段文本不产生新字形，渲染侧不该重复上传。
  #[test]
  fn a_second_layout_reports_no_new_glyphs() {
    let mut shaper = shaper();
    let origin = LogicalPosition::new(0.0, 0.0);

    shaper.layout("AB", STYLE, origin, [255; 4], 1.0);
    let first = shaper.take_new_glyphs();
    assert_eq!(first.len(), 2, "两个不同的字应各光栅化一次");

    shaper.layout("AB", STYLE, origin, [255; 4], 1.0);
    assert!(shaper.take_new_glyphs().is_empty(), "第二次应全部命中缓存");
  }

  /// 高 DPI 下字更大更清晰（物理分辨率更高），但**逻辑尺寸不变**——UI 摆位不必逐处乘 DPI。
  #[test]
  fn the_same_text_keeps_its_logical_size_across_dpi() {
    let mut shaper = shaper();
    let origin = LogicalPosition::new(0.0, 0.0);

    let low = shaper.layout("A", STYLE, origin, [255; 4], 1.0);
    let high = shaper.layout("A", STYLE, origin, [255; 4], 2.0);

    // 字号取整与 DPI 相乘后难免有亚像素差，放到 1 逻辑像素的宽容度里比。
    let (low, high) = (low[0].rect, high[0].rect);
    assert!((low.width - high.width).abs() < 1.0, "{low:?} vs {high:?}");
    assert!(
      (low.height - high.height).abs() < 1.0,
      "{low:?} vs {high:?}"
    );
    assert!((low.x - high.x).abs() < 1.0, "{low:?} vs {high:?}");
    assert!((low.y - high.y).abs() < 1.0, "{low:?} vs {high:?}");

    // 物理位图确实翻倍了——这才是「高 DPI 更清晰」的来源。
    let keys = shaper.take_new_glyphs();
    let sizes: Vec<u32> = keys.iter().map(|bitmap| bitmap.width).collect();
    assert_eq!(keys.len(), 2, "两个 DPI 各一份位图：{sizes:?}");
    assert!(sizes[1] > sizes[0], "高 DPI 的位图应更宽：{sizes:?}");
  }

  /// 非法缩放因子（0 / NaN）不能把坐标变成 `inf`——与 `core::geometry` 同一守卫。
  #[test]
  fn an_illegal_scale_factor_still_lays_out() {
    let mut shaper = shaper();
    let origin = LogicalPosition::new(10.0, 20.0);
    let quad = shaper.layout("A", STYLE, origin, [255; 4], 0.0);

    assert_eq!(quad.len(), 1);
    assert!(quad[0].rect.x.is_finite());
    assert!(quad[0].rect.y.is_finite());
    assert!(
      (quad[0].rect.x - origin.x).abs() < 2.0,
      "{:?}",
      quad[0].rect
    );
  }
}
