//! lxlake-demo：引擎侧应用。
//!
//! M1 的内容是**一座浮空岛**：自由飞行相机 + 区块流式生成 + wgpu 渲染。M2 加上**交互**：
//! 射线破坏 / 放置 + 碰撞夹紧。三者都来自引擎，本文件只做「接线」——把事件翻译成意图、把意图
//! 解析成命令、把流式产出交给渲染器，不实现任何算法。
//!
//! 键位是一张**键位表**（数据，不是散落的 `match`），链路见 `docs/roadmap.md` 的
//! 「输入与模拟的分界」：
//!
//! ```text
//!   W / S / A / D   前后左右      Space / Ctrl   上升 / 下降
//!   Shift           加速          左键 / 右键     破坏 / 放置
//!   Esc             退出
//! ```

use lxlake::camera::{CameraInput, FlyCamera};
use lxlake::core::command::{Command, Intent};
use lxlake::core::event::Event;
use lxlake::core::geometry::LogicalSize;
use lxlake::core::input::{InputSource, IntentState, Key, Keymap, MouseButton};
use lxlake::core::window::WindowDesc;
use lxlake::render::Renderer;
use lxlake::runtime::{App, AppContext, Frame, JobPool};
use lxlake::world::block::{BlockDef, BlockId, BlockPalette, BlockRegistry, FaceTiles};
use lxlake::world::chunk::ChunkPos;
use lxlake::world::collision::sweep;
use lxlake::world::raycast::raycast;
use lxlake::world::terrain::{ISLAND_MAX_CHUNK_Y, ISLAND_MIN_CHUNK_Y, IslandGenerator};
use lxlake::world::{Aabb, ChunkStreamer, StreamBounds, StreamOutput, World};
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

/// 相机碰撞盒的半长（方块）：一个「脑袋」的尺寸。
const PLAYER_HALF: [f32; 3] = [0.3, 0.3, 0.3];

/// 手够得到的距离（方块）：射线射程上限。
const REACH: f32 = 8.0;

/// 图集每格的基色，下标与方块定义里的 tile 一致（0 号格保留给空气）。
const ATLAS_TILES: [[u8; 3]; 4] = [[0, 0, 0], [130, 130, 132], [132, 96, 66], [94, 154, 70]];

/// 应用声明的默认键位表：`输入源 → 意图`。
///
/// 做成一等数据，是为了让第三方应用也能声明自己的默认键位（见 `docs/roadmap.md` 的
/// 「键位表」）；用户覆盖与改键界面分别留到 M3 / M4。
fn default_keymap() -> Keymap {
  Keymap::new()
    .bind(InputSource::Key(Key::W), Intent::MoveForward)
    .bind(InputSource::Key(Key::S), Intent::MoveBackward)
    .bind(InputSource::Key(Key::A), Intent::MoveLeft)
    .bind(InputSource::Key(Key::D), Intent::MoveRight)
    .bind(InputSource::Key(Key::Space), Intent::Ascend)
    .bind(InputSource::Key(Key::Control), Intent::Descend)
    .bind(InputSource::Key(Key::Shift), Intent::Boost)
    .bind(InputSource::Key(Key::Escape), Intent::Quit)
    .bind(InputSource::Mouse(MouseButton::Left), Intent::Break)
    .bind(InputSource::Mouse(MouseButton::Right), Intent::Place)
}

/// 意图状态 → 相机输入。
///
/// **只叠轴值，不含转向**：转向由 [`FlyCamera::look`] 单独吃鼠标位移（见 `camera` 模块文档）。
fn camera_input(intents: &IntentState) -> CameraInput {
  let axis = |positive: Intent, negative: Intent| {
    f32::from(intents.is_held(positive) as u8) - f32::from(intents.is_held(negative) as u8)
  };

  CameraInput {
    forward: axis(Intent::MoveForward, Intent::MoveBackward),
    right: axis(Intent::MoveRight, Intent::MoveLeft),
    up: axis(Intent::Ascend, Intent::Descend),
    boost: intents.is_held(Intent::Boost),
    look: [0.0, 0.0],
  }
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
  /// 默认键位表（应用声明的那一层）。
  keymap: Keymap,
  /// 键位表的落点：轴意图按住 / 动作意图排队。
  intents: IntentState,
  /// 本帧攒下的命令：帧边界按序应用（M2 验收第 4 条要能打印出这条序列）。
  commands: Vec<Command>,
  /// 放置用的方块。
  place_block: BlockId,
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
      keymap: default_keymap(),
      intents: IntentState::new(),
      commands: Vec::new(),
      place_block: palette.grass,
      look: [0.0, 0.0],
      frames: 0,
      last_report: Duration::ZERO,
    }
  }

  /// 模拟层：把一个动作意图解析成命令。轴意图（含 Quit）在这里返回 `None`。
  ///
  /// 意图不含世界坐标，坐标要在这里、用射线从**相机眼位沿朝向**解出来：
  /// 破坏 = 命中方块本身，放置 = 命中方块 + 命中面法线（法线是唯一能区分「点的是哪一面」的
  /// 信息，见 `docs/roadmap.md` 的「射线投射」）。
  fn command_for(&self, intent: Intent) -> Option<Command> {
    let hit = raycast(
      &self.world,
      self.camera.position(),
      self.camera.forward(),
      REACH,
    )?;
    match intent {
      Intent::Break => Some(Command::Break { pos: hit.block }),
      Intent::Place => {
        let step = hit.face.step();
        Some(Command::Place {
          pos: [
            hit.block[0] + step[0],
            hit.block[1] + step[1],
            hit.block[2] + step[2],
          ],
          block: self.place_block,
        })
      }
      _ => None,
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
      // 设备事件到这里就只剩「源 + 按下/抬起」：查表、记状态都在 `IntentState`，应用不再碰按键。
      Event::KeyboardInput { key, pressed, .. } => {
        self
          .intents
          .handle(&self.keymap, InputSource::Key(*key), *pressed);
      }
      Event::MouseButton {
        button, pressed, ..
      } => {
        self
          .intents
          .handle(&self.keymap, InputSource::Mouse(*button), *pressed);
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
      // 失焦就放开光标，不然切出去还锁着鼠标没法操作别的窗口；同时丢掉按住的意图，
      // 切回来不该还在往前飞。
      Event::Focused { focused, .. } => {
        if !*focused {
          self.intents.clear();
        }
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

      // 转向与位移分开走：位移**先过一遍碰撞夹紧**再落位，撞墙就停（见 `camera` / `collision`）。
      self.camera.look(look);
      let desired = self
        .camera
        .desired_delta(&camera_input(&self.intents), delta);
      let allowed = sweep(
        &self.world,
        Aabb::new(self.camera.position(), PLAYER_HALF),
        desired,
      );
      self.camera.translate(allowed);

      // 意图 → 命令：动作意图在这一帧排空，解析成带坐标的命令。
      for intent in self.intents.take_actions() {
        match intent {
          Intent::Quit => cx.exit(),
          Intent::Break | Intent::Place => {
            if let Some(command) = self.command_for(intent) {
              self.commands.push(command);
            }
          }
          _ => {}
        }
      }

      // 命令在帧边界、模拟之前按序应用：本帧改的方块本帧就进重算队列，画面下一帧更新。
      // 这条序列就是 M2 验收第 4 条要的「命令流」——M4 存档与将来联机从这里接。
      if !self.commands.is_empty() {
        let line = self
          .commands
          .iter()
          .map(Command::to_string)
          .collect::<Vec<_>>()
          .join(" | ");
        println!("本帧命令：{line}");

        let mut dirty = Vec::new();
        for command in self.commands.drain(..) {
          dirty.extend(self.world.apply(&command));
        }
        self.streamer.mark_dirty(&dirty);
      }

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
