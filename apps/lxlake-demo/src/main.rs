//! lxlake-demo：引擎侧应用。M0 只开一个空窗口，验证帧率、resize 与 DPI 响应；M1 起承载空岛。

use lxlake::core::event::Event;
use lxlake::core::geometry::LogicalSize;
use lxlake::core::window::WindowDesc;
use lxlake::runtime::{App, AppContext, Frame};
use std::time::Duration;

/// 帧率汇总周期。
const REPORT_INTERVAL: Duration = Duration::from_secs(1);

struct Demo {
  frames: u32,
  /// 上次汇总时的 `Frame::elapsed`。
  last_report: Duration,
}

impl App for Demo {
  fn windows(&self) -> Vec<WindowDesc> {
    vec![WindowDesc {
      title: "lxlake demo".to_owned(),
      size: LogicalSize::new(1280.0, 720.0),
      ..WindowDesc::default()
    }]
  }

  fn on_event(&mut self, _cx: &mut AppContext, event: &Event) {
    match event {
      Event::Resized { size, .. } => println!("resized: {}x{}", size.width, size.height),
      Event::ScaleFactorChanged {
        scale_factor, size, ..
      } => {
        let logical = size.to_logical(*scale_factor);
        println!(
          "scale: {scale_factor} → 逻辑 {:.0}x{:.0}",
          logical.width, logical.height
        );
      }
      Event::Focused { focused, .. } => println!("focused: {focused}"),
      Event::CloseRequested { .. } => println!("close requested"),
    }
  }

  fn on_frame(&mut self, cx: &mut AppContext, frame: Frame) {
    self.frames += 1;

    let since = frame.elapsed.saturating_sub(self.last_report);
    if since < REPORT_INTERVAL {
      return;
    }

    let fps = f64::from(self.frames) / since.as_secs_f64();
    self.frames = 0;
    self.last_report = frame.elapsed;

    if let Some(window) = cx.main_window() {
      let size = window.size();
      window.set_title(&format!(
        "lxlake demo — {fps:.1} fps | {}x{} @ {:.2}x",
        size.width,
        size.height,
        window.scale_factor()
      ));
    }
  }
}

#[lxlake::entry]
fn main() -> Demo {
  Demo {
    frames: 0,
    last_report: Duration::ZERO,
  }
}
