//! lxlake-demo：引擎侧应用。
//!
//! M1 的内容是**一座浮空岛**：自由飞行相机 + 区块流式生成 + wgpu 渲染。三者都来自引擎，
//! 本文件只做「接线」——把事件翻译成相机输入、把流式产出交给渲染器，不实现任何算法。
//!
//! 键位（全在应用层映射，相机不认识按键）：
//!
//! ```text
//!   W / S / A / D   前后左右      Space / Ctrl   上升 / 下降
//!   Shift           加速          Esc            退出
//! ```

use lxlake::camera::{CameraInput, FlyCamera};
use lxlake::core::event::Event;
use lxlake::core::geometry::LogicalSize;
use lxlake::core::input::Key;
use lxlake::core::window::WindowDesc;
use lxlake::render::Renderer;
use lxlake::runtime::{App, AppContext, Frame, JobPool};
use lxlake::world::block::{BlockDef, BlockPalette, BlockRegistry, FaceTiles};
use lxlake::world::chunk::ChunkPos;
use lxlake::world::terrain::{ISLAND_MAX_CHUNK_Y, ISLAND_MIN_CHUNK_Y, IslandGenerator};
use lxlake::world::{ChunkStreamer, StreamBounds, StreamOutput, World};
use std::sync::Arc;
use std::time::Duration;

/// 帧率汇总周期。
const REPORT_INTERVAL: Duration = Duration::from_secs(1);

/// 视距（区块）。
const VIEW_RADIUS: i32 = 5;

/// 世界种子。固定下来：这一个岛是「理解世界」的基准，换岛的入口留给 M2。
const SEED: u64 = 0x1A1A_2026;

/// 起步位置：岛的上方偏外侧，回头（−Z）看得到整座岛。
const START_POSITION: [f32; 3] = [0.0, 78.0, 95.0];

/// 起步的俯视角（弧度，负值 = 低头）。
const START_PITCH: f32 = -0.28;

/// 单帧时长上限（秒）。
///
/// 卡一下（窗口拖动、首帧生成）不该让相机瞬移出去——位移是按时间积分的，时间戳得封顶。
const MAX_DELTA: f32 = 0.1;

/// 图集每格的基色，下标与方块定义里的 tile 一致（0 号格保留给空气）。
const ATLAS_TILES: [[u8; 3]; 4] = [[0, 0, 0], [130, 130, 132], [132, 96, 66], [94, 154, 70]];

/// 按下的键。
///
/// 相机只吃轴值（见 `camera`），所以「哪个键是哪个轴」这件事在这里落地。
#[derive(Default)]
struct HeldKeys {
  forward: bool,
  backward: bool,
  left: bool,
  right: bool,
  up: bool,
  down: bool,
  boost: bool,
}

impl HeldKeys {
  /// 更新一个键。相机不用的键（Esc、Q、E）安静忽略。
  fn update(&mut self, key: Key, pressed: bool) {
    let slot = match key {
      Key::W => &mut self.forward,
      Key::S => &mut self.backward,
      Key::A => &mut self.left,
      Key::D => &mut self.right,
      Key::Space => &mut self.up,
      Key::Control => &mut self.down,
      Key::Shift => &mut self.boost,
      Key::Q | Key::E | Key::Escape => return,
    };
    *slot = pressed;
  }

  /// 攒出本帧的相机输入。`look` 是本帧累计的鼠标位移（像素）。
  fn camera_input(&self, look: [f32; 2]) -> CameraInput {
    CameraInput {
      forward: axis(self.forward, self.backward),
      right: axis(self.right, self.left),
      up: axis(self.up, self.down),
      boost: self.boost,
      look,
    }
  }
}

/// 一对相反键 → 轴值。
fn axis(positive: bool, negative: bool) -> f32 {
  f32::from(positive as u8) - f32::from(negative as u8)
}

struct Demo {
  world: World,
  streamer: ChunkStreamer,
  /// 作业池要拿运行时的 [`lxlake::runtime::Wakeup`]，因此只能在 `on_startup` 里建。
  pool: Option<JobPool>,
  camera: FlyCamera,
  renderer: Option<Renderer>,
  /// 流式产出：每帧复用同一块缓冲，避免逐帧分配。
  output: StreamOutput,
  keys: HeldKeys,
  /// 本帧累计的鼠标位移，`on_frame` 里消费掉。
  look: [f32; 2],
  frames: u32,
  /// 上次汇总时的 `Frame::elapsed`。
  last_report: Duration,
}

impl Demo {
  /// 建方块表、解析方块集、起生成器与流式器。
  fn new() -> Self {
    let mut registry = BlockRegistry::new();
    let mut register = |key: &'static str, tile: u16| {
      registry.register(BlockDef {
        key,
        solid: true,
        tiles: FaceTiles::uniform(tile),
      })
    };
    let palette = BlockPalette {
      stone: register("stone", 1),
      dirt: register("dirt", 2),
      grass: register("grass", 3),
    };

    // 纵向只加载岛占的那几层：相机飞到天上也不需要多一组区块（见 `world::terrain`）。
    let bounds = StreamBounds {
      radius: VIEW_RADIUS,
      min_y: ISLAND_MIN_CHUNK_Y,
      max_y: ISLAND_MAX_CHUNK_Y,
    };

    let mut camera = FlyCamera::new(START_POSITION);
    // 相机默认看向水平方向；从这个位置那是正对天空，所以先低头再看。
    let tilt = -START_PITCH / camera.sensitivity();
    camera.update(
      &CameraInput {
        look: [0.0, tilt],
        ..CameraInput::default()
      },
      0.0,
    );

    Self {
      world: World::new(registry),
      streamer: ChunkStreamer::new(bounds, IslandGenerator::new(SEED, palette)),
      pool: None,
      camera,
      renderer: None,
      output: StreamOutput::default(),
      keys: HeldKeys::default(),
      look: [0.0, 0.0],
      frames: 0,
      last_report: Duration::ZERO,
    }
  }

  /// 作业线程数：留一个核给主线程。
  fn worker_count() -> usize {
    std::thread::available_parallelism().map_or(4, |count| count.get().saturating_sub(1).max(1))
  }

  /// 每秒把帧率与流式进度写到标题栏。
  fn report(&mut self, cx: &AppContext, frame: Frame) {
    let since = frame.elapsed.saturating_sub(self.last_report);
    if since < REPORT_INTERVAL {
      return;
    }

    let fps = f64::from(self.frames) / since.as_secs_f64();
    let uploaded = self
      .renderer
      .as_ref()
      .map_or(0, |renderer| renderer.uploaded_chunks());
    self.frames = 0;
    self.last_report = frame.elapsed;

    if let Some(window) = cx.main_window() {
      window.set_title(&format!(
        "lxlake demo — {fps:.1} fps | GPU 区块 {uploaded} | 世界 {} | 流式 {}",
        self.world.chunk_count(),
        self.streamer.tracked()
      ));
    }
  }
}

impl App for Demo {
  fn windows(&self) -> Vec<WindowDesc> {
    vec![WindowDesc {
      title: "lxlake demo".to_owned(),
      size: LogicalSize::new(1280.0, 720.0),
      ..WindowDesc::default()
    }]
  }

  fn on_startup(&mut self, cx: &mut AppContext) {
    self.pool = Some(JobPool::with_workers(Self::worker_count(), cx.wakeup()));

    let Some(window) = cx.main_window().cloned() else {
      eprintln!("lxlake demo：没有窗口，只跑世界不渲染");
      return;
    };

    match Renderer::new(Arc::clone(&window), &ATLAS_TILES) {
      Ok(renderer) => self.renderer = Some(renderer),
      Err(error) => eprintln!("lxlake demo：渲染初始化失败：{error}"),
    }

    // 抓住光标，鼠标位移才能一直喂给视角控制。
    window.set_cursor_grab(true);
    window.set_cursor_visible(false);
  }

  fn on_event(&mut self, cx: &mut AppContext, event: &Event) {
    match event {
      Event::CloseRequested { .. } => cx.exit(),
      Event::KeyboardInput { key, pressed, .. } => {
        if *key == Key::Escape && *pressed {
          cx.exit();
        }
        self.keys.update(*key, *pressed);
      }
      // 设备级位移，与光标抓没抓住无关，直接攒起来。
      Event::MouseMotion { delta } => {
        self.look[0] += delta[0];
        self.look[1] += delta[1];
      }
      Event::Resized { size, .. } => {
        if let Some(renderer) = &mut self.renderer {
          renderer.resize(*size);
        }
      }
      // 失焦就放开光标，不然切出去还锁着鼠标没法操作别的窗口。
      Event::Focused { focused, .. } => {
        if let Some(window) = cx.main_window() {
          window.set_cursor_grab(*focused);
          window.set_cursor_visible(!*focused);
        }
      }
      _ => {}
    }
  }

  fn on_frame(&mut self, cx: &mut AppContext, frame: Frame) {
    self.frames += 1;

    if let Some(pool) = &self.pool {
      let delta = frame.delta.as_secs_f32().min(MAX_DELTA);
      let look = std::mem::take(&mut self.look);
      self.camera.update(&self.keys.camera_input(look), delta);

      // 流式的入口只有一个：焦点区块。相机飞过区块边界，视距内的区块才随之换一批。
      let focus = chunk_focus(self.camera.position());
      let streamer = &mut self.streamer;
      let world = &mut self.world;
      let output = &mut self.output;
      streamer.update(world, pool, focus, output);

      if let Some(renderer) = &mut self.renderer {
        for mesh in &self.output.meshed {
          renderer.upload(mesh);
        }
        for pos in &self.output.unloaded {
          renderer.unload(*pos);
        }
        if let Err(error) = renderer.render(&self.camera) {
          eprintln!("lxlake demo：渲染失败：{error}");
          cx.exit();
        }
      }
    }

    self.report(cx, frame);
  }

  fn on_shutdown(&mut self, _cx: &mut AppContext) {
    // 显式放掉：GPU 资源与池都该在事件循环退出前收干净，别留给进程退出去处理。
    self.renderer = None;
    self.pool = None;
  }
}

/// 世界坐标 → 区块坐标（焦点）。用欧几里得除法换算，负坐标才不会偏一格。
fn chunk_focus(position: [f32; 3]) -> ChunkPos {
  ChunkPos::from_world([
    position[0].floor() as i32,
    position[1].floor() as i32,
    position[2].floor() as i32,
  ])
}

#[lxlake::entry]
fn main() -> Demo {
  Demo::new()
}
