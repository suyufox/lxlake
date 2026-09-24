//! lxlake-demo：引擎侧应用的**库**。
//!
//! 逻辑住在这里、可执行入口住在同包的 `main.rs`（它只调一行 [`run`]）——分开是为了 Android：
//! 那边的真实入口是 `android_main` 这个导出符号，与 `fn main` 无关（见 `#[lxlake::entry]`）。
//!
//! M1 的内容是**一座浮空岛**：自由飞行相机 + 区块流式生成 + wgpu 渲染。M2 加上**交互**：
//! 射线破坏 / 放置 + 碰撞夹紧。M3 加上**自绘 HUD**：左上角一块信息面板 + 屏幕中央的准星，
//! 外加一块 Tab 打开的调试面板——自绘 UI 第一次**抢输入**（闸口见 `ui_owns_control`）。
//! 三者都来自引擎，本模块只做「接线」——把事件翻译成意图、把意图解析成命令、把流式产出交给
//! 渲染器、把排版交给 UI，不实现任何算法。
//!
//! 键位是一张**键位表**（数据，不是散落的 `match`），链路见 `docs/roadmap.md` 的
//! 「输入与模拟的分界」：
//!
//! ```text
//!   W / S / A / D   前后左右      Space / Ctrl   上升 / 下降
//!   Shift           加速          左键 / 右键     破坏 / 放置
//!   Tab             开关调试面板   Esc            退出（面板开着时先关面板）
//! ```

use lxlake::camera::{CameraInput, FlyCamera};
use lxlake::core::command::{Command, Intent};
use lxlake::core::event::Event;
use lxlake::core::geometry::{LogicalPosition, LogicalSize};
use lxlake::core::input::{InputSource, IntentState, Key, Keymap, MouseButton};
use lxlake::core::widget::{Anchor, UiId, Widget};
use lxlake::core::window::WindowDesc;
use lxlake::render::Renderer;
use lxlake::runtime::{App, AppContext, Capabilities, Frame};
use lxlake::ui::{Quad, TextShaper, TextStyle, UiTree};
use lxlake::world::block::{BlockDef, BlockId, BlockPalette, BlockRegistry, FaceTiles};
use lxlake::world::chunk::ChunkPos;
use lxlake::world::collision::sweep;
use lxlake::world::raycast::raycast;
use lxlake::world::terrain::{ISLAND_MAX_CHUNK_Y, ISLAND_MIN_CHUNK_Y, IslandGenerator};
use lxlake::world::{Aabb, ChunkStreamer, StreamBounds, StreamOutput, World};
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

/// HUD 字体文件（OFL，随仓库提供，见 `data/fonts/`）。
///
/// 路径相对**工作区根目录**：`data/` 是发行物的资产目录（见 `docs/architecture.md` 的分层），
/// 应用按约定从那里读，不是把 8MB 字体编进二进制。
const HUD_FONT: &str = "data/fonts/NotoSansSC-Regular.otf";

/// HUD 的字号与行高（逻辑像素）。
const HUD_STYLE: TextStyle = TextStyle::new(14.0, 18.0);

/// 面板到文字的内边距，以及面板到窗口左上角的距离（逻辑像素）。
const HUD_PADDING: f64 = 8.0;
const HUD_MARGIN: [f64; 2] = [12.0, 12.0];

/// HUD 配色（sRGB + alpha）：半透明深色面板压住背景，文字近白。
const HUD_PANEL_COLOR: [u8; 4] = [10, 12, 16, 170];
const HUD_TEXT_COLOR: [u8; 4] = [235, 240, 245, 255];

/// 准星的臂长（半长）与线宽（逻辑像素）。
const CROSSHAIR_ARM: f64 = 5.0;
const CROSSHAIR_THICKNESS: f64 = 2.0;
const CROSSHAIR_COLOR: [u8; 4] = [255, 255, 255, 200];

/// 帧率的平滑系数：HUD 上要连续变化，瞬时值（`1 / delta`）跳得没法看。
const FPS_SMOOTHING: f32 = 0.05;

/// HUD 里那四个 Widget 的身份。集中在这里，因为布局与出图都按它们记账。
const HUD_PANEL_ID: UiId = UiId(1);
const HUD_CROSSHAIR_H_ID: UiId = UiId(2);
const HUD_CROSSHAIR_V_ID: UiId = UiId(3);
/// 调试面板（Tab 开关）。**是模态的**：开着的时候键盘与视角都归它（见 `ui_owns_control`）。
const HUD_HELP_ID: UiId = UiId(4);

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
  camera: FlyCamera,
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
  /// HUD 的 Widget 树。每帧重建，命中测试（输入闸口）用的就是它。
  tree: UiTree,
  /// 光标在窗口里的位置（逻辑像素）。命中测试要它——带位置的只有 `CursorMoved` 那一类事件。
  cursor: LogicalPosition,
  /// 调试面板是否打开。面板是**模态**的：开着时键盘与视角都归它。
  panel_open: bool,
  /// 平滑后的帧率。
  fps: f32,
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
      camera,
      output: StreamOutput::default(),
      keymap: default_keymap(),
      intents: IntentState::new(),
      commands: Vec::new(),
      place_block: palette.grass,
      look: [0.0, 0.0],
      tree: UiTree::new(),
      cursor: LogicalPosition::new(0.0, 0.0),
      panel_open: false,
      fps: 0.0,
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
  ///
  /// `uploaded`（GPU 上的区块数）由调用方递进来：渲染器住在能力里，不从自己身上取。
  fn report(&mut self, cx: &AppContext, frame: Frame, uploaded: usize) {
    let since = frame.elapsed.saturating_sub(self.last_report);
    if since < REPORT_INTERVAL {
      return;
    }

    let fps = f64::from(self.frames) / since.as_secs_f64();
    self.frames = 0;
    self.last_report = frame.elapsed;

    if let Some(window) = cx.main_window() {
      window.handle().set_title(&format!(
        "lxlake demo — {fps:.1} fps | GPU 区块 {uploaded} | 世界 {} | 流式 {}",
        self.world.chunk_count(),
        self.streamer.tracked()
      ));
    }
  }

  /// 拼本帧的 HUD：左上角一块信息面板 + 屏幕中央的准星 +（面板开着时的）一块调试面板，
  /// 产出**逻辑像素**的方片。
  ///
  /// 摆位走引擎那套锚定布局（[`UiTree`]）：先按视口算矩形，再往矩形里塞方片。不直接写死坐标是
  /// 因为输入闸口要用同一棵树做命中测试——树里没有的东西，点上去也就不会有反应。
  ///
  /// **加入顺序有语义**：后加的在上层（见 [`UiTree::hit_test`]），所以准星排在最底。这个顺序
  /// 就是准星不吞点击的凭据——帮助面板压在正中，面板开着时正中那一下落在面板上（见
  /// `ui_owns_click`）。
  ///
  /// `scale_factor` 只参与字形光栅化：字按物理分辨率画得清楚，矩形仍旧是逻辑像素（见 `ui::text`）。
  ///
  /// `text` 与 `uploaded` 由调用方递进来，不从自己身上取：排版器住在 `App` 的能力里（见
  /// [`Capabilities`]），HUD 只管排版，不再管字体的来路。
  fn build_hud(
    &mut self,
    text: Option<&mut TextShaper>,
    viewport: LogicalSize,
    scale_factor: f64,
    uploaded: usize,
  ) -> Vec<Quad> {
    if viewport.width <= 0.0 || viewport.height <= 0.0 {
      return Vec::new();
    }

    // 面板的尺寸要先量出来（见下），所以文本得在借用排版器之前凑齐。
    let info = hud_lines(
      self.fps,
      uploaded,
      self.world.chunk_count(),
      self.camera.position(),
    );
    let help = if self.panel_open {
      help_lines()
    } else {
      Vec::new()
    };

    let Some(shaper) = text else {
      // 字体没读进来：HUD 整块不画（启动时已经报过原因）。
      return Vec::new();
    };

    self.tree.clear();
    // 准星先加（在最底）：两根细方片而不是贴图——自绘 UI 的出图形式只有方片一种（见 `ui::draw`）。
    self.tree.add(Widget::new(
      HUD_CROSSHAIR_H_ID,
      Anchor::Center,
      LogicalSize::new(CROSSHAIR_ARM * 2.0, CROSSHAIR_THICKNESS),
    ));
    self.tree.add(Widget::new(
      HUD_CROSSHAIR_V_ID,
      Anchor::Center,
      LogicalSize::new(CROSSHAIR_THICKNESS, CROSSHAIR_ARM * 2.0),
    ));
    self.tree.add(
      Widget::new(
        HUD_PANEL_ID,
        Anchor::TopLeft,
        panel_size(shaper, &info, scale_factor),
      )
      .offset(HUD_MARGIN),
    );
    if self.panel_open {
      // 帮助面板摆正中：既在视觉上是「模态」，也顺手盖住准星。
      self.tree.add(Widget::new(
        HUD_HELP_ID,
        Anchor::Center,
        panel_size(shaper, &help, scale_factor),
      ));
    }
    self.tree.layout(viewport);

    let mut quads = Vec::new();
    for id in [HUD_CROSSHAIR_H_ID, HUD_CROSSHAIR_V_ID] {
      if let Some(rect) = self.tree.rect_of(id) {
        quads.push(Quad::solid(rect, CROSSHAIR_COLOR));
      }
    }
    push_panel_quads(
      &mut quads,
      &self.tree,
      shaper,
      HUD_PANEL_ID,
      &info,
      scale_factor,
    );
    if self.panel_open {
      push_panel_quads(
        &mut quads,
        &self.tree,
        shaper,
        HUD_HELP_ID,
        &help,
        scale_factor,
      );
    }
    quads
  }

  /// 开关调试面板。四件事一起翻，少翻一件都会出现「面板开着但相机还在转」这类怪相：
  /// 面板状态、意图清空、光标抓取、光标可见性。
  ///
  /// 清意图是必须的：面板期间那些按键抬起事件被闸口挡在外面，不清就会留下「一直按着 W」的
  /// 轴向，关面板后相机自己往前飞。
  fn set_panel_open(&mut self, cx: &AppContext, open: bool) {
    if self.panel_open == open {
      return;
    }
    self.panel_open = open;
    self.intents.clear();
    if let Some(window) = cx.main_window() {
      // 面板要鼠标（以后要能点控件），世界要锁定光标转视角：两者互斥。
      window.handle().set_cursor_grab(!open);
      window.handle().set_cursor_visible(open);
    }
  }
}

impl Demo {
  /// 起步：抓住光标，鼠标位移才能一直喂给视角控制。
  ///
  /// 作业池、字体、渲染器都不在这里建——它们的配置写在装配层（`Builder::workers` /
  /// `font_path` / `renderer`），由运行时按配置建好，应用只管从能力里借（见 `Capabilities`）。
  fn startup(&self, cx: &AppContext) {
    let Some(window) = cx.main_window() else {
      eprintln!("lxlake demo：没有窗口，只跑世界不渲染");
      return;
    };
    window.handle().set_cursor_grab(true);
    window.handle().set_cursor_visible(false);
  }

  fn handle_event(&mut self, cx: &mut AppContext, event: &Event) {
    match event {
      Event::CloseRequested { .. } => cx.exit(),
      // 设备事件到这里就只剩「源 + 按下/抬起」：查表、记状态都在 `IntentState`，应用不再碰按键。
      // 闸口在最前面：UI 抢走控制权时，事件根本走不到键位表。
      Event::KeyboardInput { key, pressed, .. } => {
        if ui_owns_control(self.panel_open) {
          if *pressed {
            // 面板里只认这两个键。Esc 在这里是「关面板」，不是键位表上的「退出」——
            // 退出那条键位仍在表里，面板关掉后照旧生效。
            if matches!(key, Key::Tab | Key::Escape) {
              self.set_panel_open(cx, false);
            }
          }
          return;
        }
        // 面板关着：Tab 开面板（只认按下，按住 Tab 的自动重复不该反复开关）。
        if *pressed && *key == Key::Tab {
          self.set_panel_open(cx, true);
          return;
        }
        self
          .intents
          .handle(&self.keymap, InputSource::Key(*key), *pressed);
      }
      Event::MouseButton {
        button, pressed, ..
      } => {
        // 点在面板上才归 UI；其余一律穿透到世界（准星就靠这条不吞点击，见 `ui_owns_click`）。
        if ui_owns_click(self.panel_open, &self.tree, self.cursor) {
          return;
        }
        self
          .intents
          .handle(&self.keymap, InputSource::Mouse(*button), *pressed);
      }
      // 设备级位移，与光标抓没抓住无关，直接攒起来；面板开着则不攒——那时用户是在瞄面板。
      Event::MouseMotion { delta } => {
        if ui_owns_control(self.panel_open) {
          return;
        }
        self.look[0] += delta[0];
        self.look[1] += delta[1];
      }
      // 位置只在命中测试里用，攒着供 `MouseButton` 那一下取。
      Event::CursorMoved { position, .. } => self.cursor = *position,
      // resize 与 DPI 变化不在这里接：它们先落到该窗的渲染器上（表面重配 + 深度附件重建 +
      // 换算比例），这是每个应用都要写一遍的样板，已经收进装配层的内建转发（见 `Builder`）。
      // 失焦就放开光标，不然切出去还锁着鼠标没法操作别的窗口；同时丢掉按住的意图，
      // 切回来不该还在往前飞。面板开着时本来就该是自由光标，两个条件一起看。
      Event::Focused { focused, .. } => {
        if !*focused {
          self.intents.clear();
        }
        if let Some(window) = cx.main_window() {
          window
            .handle()
            .set_cursor_grab(*focused && !self.panel_open);
          window
            .handle()
            .set_cursor_visible(!*focused || self.panel_open);
        }
      }
      _ => {}
    }
  }

  /// 一帧：模拟 → 流式 → HUD → 出画。
  ///
  /// 作业池、排版器、渲染器都由调用方从能力里递进来（见 [`Capabilities`]），本方法只管它们的
  /// **用法**：状态推进要池子、拼 HUD 要排版器、出画要渲染器。
  fn frame(&mut self, cx: &mut AppContext, frame: Frame, capabilities: Capabilities<'_>) {
    self.frames += 1;
    // 帧率的平滑值（标题栏那份仍是每秒的窗口平均，见 `report`）。
    let delta = frame.delta.as_secs_f32();
    if delta > 0.0 {
      let instant = 1.0 / delta;
      self.fps = if self.frames > 1 {
        self.fps * (1.0 - FPS_SMOOTHING) + instant * FPS_SMOOTHING
      } else {
        instant
      };
    }

    // `text` 这一帧要借两次（先拼 HUD，再取本帧新光栅化的字形），所以绑成 `mut`、用
    // `as_deref_mut` 逐次借出，而不是把整个 `Option` 交出去。
    let Capabilities {
      jobs,
      mut text,
      gpu,
    } = capabilities;
    let Some(pool) = jobs else {
      // 流式生成全在作业池上跑（见装配层的 `workers`）：没有池子这一帧就无事可做。
      return;
    };

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

    // HUD 在流式之后、渲染之前拼：面板上的数字要是这一帧的。
    let (viewport, scale_factor) =
      cx.main_window()
        .map_or((LogicalSize::new(0.0, 0.0), 1.0), |window| {
          let scale_factor = window.handle().scale_factor();
          (
            window.handle().size().to_logical(scale_factor),
            scale_factor,
          )
        });
    let uploaded = gpu
      .as_ref()
      .map_or(0, |renderer| renderer.uploaded_chunks());
    let quads = self.build_hud(text.as_deref_mut(), viewport, scale_factor, uploaded);
    // 本帧新光栅化的字形交给渲染侧增量补进 UI 图集；交出即清空（见 `ui::text`）。
    let glyphs = text.map(TextShaper::take_new_glyphs).unwrap_or_default();

    if let Some(renderer) = gpu {
      for mesh in &self.output.meshed {
        renderer.upload(mesh);
      }
      for pos in &self.output.unloaded {
        renderer.unload(*pos);
      }
      renderer.upload_glyphs(&glyphs);
      if let Err(error) = renderer.render(&self.camera, &quads) {
        eprintln!("lxlake demo：渲染失败：{error}");
        cx.exit();
      }
    }

    self.report(cx, frame, uploaded);
  }
}

/// 生命周期：装配层（[`Builder`](lxlake::Builder)）把钩子交给下面这几个自由函数，
/// 每个钩子从托管状态里取出 [`Demo`]，再交给它自己的方法。
///
/// 状态与上下文**同时**要用的地方走 [`App::with_state`]（状态临时取出，两个借用互不相干）；
/// 状态、能力、上下文**三者**都要的地方走 [`App::with_capabilities`]（帧就是这样）。
fn on_startup(app: &mut App) {
  app.with_state::<Demo, _>(|demo, cx| demo.startup(cx));
}

fn on_event(app: &mut App, event: &Event) {
  app.with_state::<Demo, _>(|demo, cx| demo.handle_event(cx, event));
}

fn on_frame(app: &mut App, frame: Frame) {
  app.with_capabilities::<Demo, _>(|demo, capabilities, cx| demo.frame(cx, frame, capabilities));
}

/// 世界坐标 → 区块坐标（焦点）。用欧几里得除法换算，负坐标才不会偏一格。
fn chunk_focus(position: [f32; 3]) -> ChunkPos {
  ChunkPos::from_world([
    position[0].floor() as i32,
    position[1].floor() as i32,
    position[2].floor() as i32,
  ])
}

/// HUD 的文本行：帧率、GPU 上的区块、世界里的区块、相机坐标。
///
/// 拼文本是应用的事，引擎只负责排版——所以这几行不放进引擎，也不做成可配置的「HUD 描述符」。
fn hud_lines(fps: f32, uploaded: usize, chunks: usize, position: [f32; 3]) -> Vec<String> {
  vec![
    format!("{fps:.0} fps"),
    format!("GPU 区块 {uploaded}"),
    format!("世界区块 {chunks}"),
    format!(
      "相机 {:.1}, {:.1}, {:.1}",
      position[0], position[1], position[2]
    ),
  ]
}

/// 调试面板的文本：默认键位表的人话版。
///
/// 面板是**模态**的（见 [`ui_owns_control`]），所以这张表里必须有「怎么关掉它」——否则用户会
/// 以为程序卡住了。Esc 在这儿仍旧写「退出」：面板开着时那一下先关面板，退出要再按一次。
fn help_lines() -> Vec<String> {
  vec![
    "WASD 移动 / Space 上升 / Ctrl 下降".to_owned(),
    "Shift 加速 / 左键破坏 / 右键放置".to_owned(),
    "Tab 开关面板 / Esc 退出 / 鼠标转向".to_owned(),
  ]
}

/// UI 是否该独占**连续控制**（键盘轴值与视角转向）。
///
/// 判据只有「面板开着吗」这一条：面板是模态的，读面板时按 WASD 不该把相机开走、动鼠标不该把
/// 画面转走。不做逐键、逐区域的判断——键盘没有「位置」，那样只会得到一份没人能预测的规则。
fn ui_owns_control(panel_open: bool) -> bool {
  panel_open
}

/// UI 是否该独占这一次**指针点击**。
///
/// 两个前提缺一不可：
/// - 面板开着——面板没开时光标是锁住的，根本不存在「指针在哪」这回事，命中测试无从谈起；
/// - 指针确实落在某个 Widget 上。
///
/// 于是**准星不会吞点击**：它是屏幕正中一个 10×10 的 Widget，但帮助面板开着时也压在正中且
/// 后加（在上层，见 [`Demo::build_hud`] 的加入顺序），命中测试落在面板上；面板没开时第一个
/// 前提就不成立。所以「对着中心挖方块」的那一下永远到得了世界。
fn ui_owns_click(panel_open: bool, tree: &UiTree, cursor: LogicalPosition) -> bool {
  panel_open && tree.hit_test(cursor).is_some()
}

/// 量一块面板的尺寸：宽度取最宽的一行，高度取行数，四周各留一个 `HUD_PADDING`。
///
/// 先量后摆：文字与底色共用同一个矩形，不会出现「文字溢出面板」。
fn panel_size(shaper: &TextShaper, lines: &[String], scale_factor: f64) -> LogicalSize {
  let mut width = 0.0f64;
  for line in lines {
    width = width.max(shaper.measure(line, HUD_STYLE, scale_factor).width);
  }
  LogicalSize::new(
    width + HUD_PADDING * 2.0,
    lines.len() as f64 * f64::from(HUD_STYLE.line_height) + HUD_PADDING * 2.0,
  )
}

/// 把一块**已摆好**的面板出成方片：先底色，再逐行文字。
///
/// 摆位与出图分两步是因为底色要拿矩形——矩形得等 [`UiTree::layout`] 算过才有，所以这里只做
/// 后半截。行距用 `HUD_STYLE.line_height`，与 [`panel_size`] 量高度时是同一个数。
fn push_panel_quads(
  quads: &mut Vec<Quad>,
  tree: &UiTree,
  shaper: &mut TextShaper,
  id: UiId,
  lines: &[String],
  scale_factor: f64,
) {
  let Some(panel) = tree.rect_of(id) else {
    return;
  };
  quads.push(Quad::solid(panel, HUD_PANEL_COLOR));

  let mut baseline = panel.y + HUD_PADDING;
  for line in lines {
    quads.extend(shaper.layout(
      line,
      HUD_STYLE,
      LogicalPosition::new(panel.x + HUD_PADDING, baseline),
      HUD_TEXT_COLOR,
      scale_factor,
    ));
    baseline += f64::from(HUD_STYLE.line_height);
  }
}

/// 应用装配：`#[lxlake::entry]` 只标**装配**这一层——函数块的值就是装配好的应用，
/// 宏据此产出桌面 `run()`（`main.rs` 调的）与 Android `android_main`。
///
/// 作业池、字体、渲染器都在这儿声明，不在应用体内建：它们是**引擎能力**（见
/// `Capabilities`），装配层说清「要什么」，运行时按配置建好并从能力里借出去。
#[lxlake::entry]
fn app() -> lxlake::Builder {
  lxlake::Builder::new()
    .app_id("com.lxlake.demo")
    .main_window(WindowDesc {
      title: "lxlake demo".to_owned(),
      size: LogicalSize::new(1280.0, 720.0),
      ..WindowDesc::default()
    })
    .workers(Demo::worker_count())
    .font_path(HUD_FONT)
    .renderer(|window| Renderer::new(window, &ATLAS_TILES))
    .manage(Demo::new())
    .on_startup(on_startup)
    .on_event(on_event)
    .on_frame(on_frame)
}

#[cfg(test)]
mod tests {
  use super::*;

  /// 字体资源的**绝对**路径。
  ///
  /// `HUD_FONT` 是相对工作区根的路径（发行物按工作目录读资产），而 `cargo test` 的工作目录是
  /// **包根**，所以测试里不能被它复用：`concat!(env!("CARGO_MANIFEST_DIR"), ...)` 在编译期拼出
  /// 绝对路径，与工作目录无关。
  const FONT_PATH: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../data/fonts/NotoSansSC-Regular.otf"
  );

  /// 建一个装了字体的 Demo（字体是 HUD 的前提，没有就没有可测的布局）。
  ///
  /// 排版器单独交出来：运行期它在 `App` 的能力里（见 [`Capabilities`]），测试里没有那层，
  /// 直接拿在手上当能力递进 [`Demo::build_hud`]。
  fn demo_with_font() -> Option<(Demo, TextShaper)> {
    let bytes = std::fs::read(FONT_PATH).ok()?;
    let shaper = TextShaper::from_bytes(bytes).ok()?;
    Some((Demo::new(), shaper))
  }

  #[test]
  fn a_click_lands_on_the_ui_only_when_the_pointer_is_over_the_open_panel() {
    let mut tree = UiTree::new();
    tree.add(Widget::new(
      UiId(1),
      Anchor::TopLeft,
      LogicalSize::new(200.0, 100.0),
    ));
    tree.layout(LogicalSize::new(1000.0, 600.0));

    // 面板关着：点在哪都不归 UI——光标此刻锁着，没有「指针在哪」这回事。
    assert!(!ui_owns_click(
      false,
      &tree,
      LogicalPosition::new(10.0, 10.0)
    ));
    // 面板开着、点在面板上：归 UI。
    assert!(ui_owns_click(true, &tree, LogicalPosition::new(10.0, 10.0)));
    // 面板开着、点在外面：穿透到世界。
    assert!(!ui_owns_click(
      true,
      &tree,
      LogicalPosition::new(500.0, 300.0)
    ));
  }

  #[test]
  fn the_open_panel_takes_the_keyboard_and_the_view() {
    assert!(!ui_owns_control(false), "面板关着时键盘与视角都归世界");
    assert!(ui_owns_control(true), "面板是模态的");
  }

  #[test]
  fn the_panel_is_sized_around_its_widest_line() {
    let Some((_, shaper)) = demo_with_font() else {
      return; // 没有字体资产就不测：HUD 本身也画不出来。
    };

    let lines = vec!["iiii".to_owned(), "WWWWWWWW".to_owned()];
    let size = panel_size(&shaper, &lines, 1.0);
    let widest = shaper.measure(&lines[1], HUD_STYLE, 1.0).width;

    assert!(widest > shaper.measure(&lines[0], HUD_STYLE, 1.0).width);
    assert!(
      (size.width - (widest + HUD_PADDING * 2.0)).abs() < 1e-9,
      "宽取最宽一行 + 两侧内边距，实际 {}",
      size.width
    );
    assert!(
      (size.height - (2.0 * f64::from(HUD_STYLE.line_height) + HUD_PADDING * 2.0)).abs() < 1e-9,
      "高取行数 × 行高 + 上下内边距，实际 {}",
      size.height
    );
  }

  /// 闸口真正的凭据：面板开着时正中那一下落在**面板**上，不是准星上。
  #[test]
  fn the_open_panel_covers_the_crosshair_at_the_center() {
    let Some((mut demo, mut shaper)) = demo_with_font() else {
      return;
    };
    let viewport = LogicalSize::new(1000.0, 600.0);
    let center = LogicalPosition::new(500.0, 300.0);

    demo.build_hud(Some(&mut shaper), viewport, 1.0, 0);
    assert!(!ui_owns_click(demo.panel_open, &demo.tree, center));
    assert_eq!(
      demo.tree.hit_test(center),
      Some(HUD_CROSSHAIR_V_ID),
      "面板关着时正中归准星（竖臂后加，压在横臂上；但光标锁着，这一下仍不归 UI）"
    );

    demo.panel_open = true;
    demo.build_hud(Some(&mut shaper), viewport, 1.0, 0);
    assert_eq!(
      demo.tree.hit_test(center),
      Some(HUD_HELP_ID),
      "帮助面板后加、压在正中，所以正中那一下是它的"
    );
    assert!(ui_owns_click(demo.panel_open, &demo.tree, center));
  }
}
