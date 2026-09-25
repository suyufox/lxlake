# 架构

## 定位

lxlake 本质是**跨平台应用框架**，同时具备游戏引擎的能力：

- **框架主线**：类 tauri 的纯跨平台应用框架，对应 `apps/lxlake-editor`
- **引擎侧线**：启用 `render` 特性后作为引擎分发，对应 `apps/lxlake-demo`
- **侧线与主线同步兼容**：侧线不独立演进，共用同一套契约与运行时

第三方依赖的尺度是**只复用底层、核心自研**：winit / wgpu / wry 这类底层可以复用，场景、命令、UI、区块等核心概念全部自己定。

## 平台范围

**只做纯设备平台，不含 web。**

| 平台       | 状态                                                                |
| ---------- | ------------------------------------------------------------------- |
| windows    | 首要目标                                                            |
| linux      | 目标                                                                |
| android    | **接入已落地**：与桌面共用 winit 后端，类型检查已过；打包与真机在后 |
| macos      | 目标，实装排后                                                      |
| ios        | 目标，实装排后                                                      |
| headless   | 无窗口后端，供测试与服务器使用                                      |
| web / wasm | **不做**                                                            |

android **不另起一个后端**：winit 自己就带 android 后端（`android-game-activity` 特性），事件循环、
窗口抽象、`ControlFlow::WaitUntil`、跨线程唤醒全都成立，于是它与桌面共用 `platform/winit.rs`。
真正的差异只有三处，都在那一处收口——入口多一个系统递进来的 `AndroidApp`（沙箱根由它给），
`resumed` 会反复来（切后台再回前台 = 一次新的 `InitWindow`），`suspended` 要把窗口整体摘掉
（系统已销毁 surface）。`AndroidApp` 由 winit 再导出，因此不直接依赖 `android-activity` / `ndk` / `jni`。

带 C 依赖的部分（vcpkg 清单）覆盖 windows / linux 宿主 + android 附加系；apple 平台不纳入 C 依赖清单，这不影响纯 Rust 部分的平台目标。

**渲染后端在 Windows 上只开 D3D12**（其余平台照旧全开）。理由不是偏好，是本机 Intel Gen9 的
Vulkan 驱动一进 `adapter.request_device` 就 AV 崩掉进程；多挂一个后端只多一份「在谁的机器上
崩」的排查成本。这条落在 `render` 建实例的地方，靠 `cfg(target_os)` 分档。

## 分层

分层落在**模块树**上，不落在 crate 边界上。`lxlake` 内部按职责分区：

| 模块         | 职责                                                           | 关键约束                                           |
| ------------ | -------------------------------------------------------------- | -------------------------------------------------- |
| `core`       | 契约：场景、Widget、命令、事件                                 | **不出现 winit / wgpu 类型**，不依赖任何平台       |
| `runtime`    | 事件循环、`App`、pump 钩子、任务调度                           | 帧边界与外部事件源的唯一归属                       |
| `platform`   | 平台后端（窗口、输入、IME、文件系统）                          | 按 `cfg(target_os)` 分档，**不按 feature**         |
| `render`     | wgpu 渲染：设备 / 表面、自绘方片管线、3D 管线与纹理导入        | 只落在 `gpu` / `ui-render` / `render` 三档特性之下 |
| `world`      | 体素世界：方块注册表、区块、浮岛生成、区块流式、射线与碰撞查询 | 纯 CPU，**不含 GPU 类型**；不加 feature 门         |
| `meshing`    | 区块 → 顶点 / 索引（greedy meshing）                           | 纯 CPU，输出 POD 顶点，无 GPU 类型                 |
| `camera`     | 自由飞行相机                                                   | 只吃已映射的轴值，不认识按键与窗口事件             |
| `ui`         | 自绘 UI：布局、文本排版、交互                                  | 自绘，不引入系统控件或 HTML 渲染                   |
| `capability` | webview / media / update 等可选横切能力                        | 逐个 feature 隔离，能力以**查询**形式暴露          |
| `plugin`     | 插件宿主                                                       | 只留一条窄的、可版本化的 C ABI 边界                |

feature 只用来隔离**重依赖**（wgpu / wry / cef 这类）。`world` / `meshing` / `camera` 是引擎侧
概念，但都是纯 CPU，所以不加门——门多了会出现「框架主线不知该开哪个」的混乱。

`core` 这条约束是**可机检的**：契约层一旦出现 `wgpu` 或 `winit` 类型，框架主线就再也无法在不编 3D 的前提下干净编译。

## 分包

```
lxlake/
├── Cargo.toml                 # workspace：3 个实体 crate + 2 个 bin
├── crates/
│   ├── lxlake/                # ★ 唯一实体库，上面那张模块表就是它的模块树
│   │   └── src/
│   │       ├── lib.rs         # 门面 + 装配（#[lxlake::entry] 在这儿）
│   │       ├── core/
│   │       ├── runtime/
│   │       ├── platform/
│   │       ├── world/         # 体素世界（block / chunk / terrain / stream / raycast / collision）
│   │       ├── meshing/
│   │       ├── camera.rs
│   │       ├── render/
│   │       ├── ui/
│   │       ├── capability/
│   │       └── plugin/
│   └── lxlake-macros/         # proc-macro —— Cargo 硬约束，必须独立
├── apps/
│   ├── lxlake-editor/         # 纯框架：ui-render（自绘 UI，不编 3D）
│   └── lxlake-demo/           # 引擎：render + 空岛
└── packages/                  # node 侧：起步空着，等真有工具再建
```

原则是**默认收束在主库**：一个能力只有确实被别处需要时才拆成独立功能库。概念分区照旧存在，但它表达为模块，不表达为 crate。

### 拆包判据

只有满足下面任一条，才允许新建实体 crate；其余一律不拆。

1. **Cargo 硬约束** —— proc-macro 只能是独立 crate（`lxlake-macros` 是唯一立刻成立的）
2. **依赖成环** —— 模块 A 的实装必须反过来依赖 B，只能靠 crate 边界打断。这是唯一的结构性理由
3. **改动独立性** —— 某模块长期独立演进（改它不牵动别处），拆出去才换得来增量编译收益

**按「概念」拆不算理由。** 参考项目 `luoxinglake` 曾一次性建 34 个 crate 骨架（按概念拆），结果是改一处牵动多片、增量编译收益为负，只多了 34 份 `Cargo.toml` 和一张依赖登记表，而 webview 后端、字体后端、移动端端到端等实装全部悬空。新增 crate 时，提交信息里要写明命中上面哪一条。

## feature 分档

**平台后端用 `cfg(target_os)`，不用 feature。** Rust 本来就按 target 编译，平台后端天然互斥；这同时避开了「同一 target 上两个后端都自称平台」的歧义。

**同 target 上的可选能力用 feature**：

```
gpu         = ["dep:wgpu"]                        # 设备 / 队列 / 表面 / 取帧
ui-render   = ["gpu", "dep:bytemuck"]             # + 自绘方片管线与 UI 图集
render      = ["ui-render"]                       # + 3D 管线 / 方块图集 / 深度与阴影
webview-wry = ["dep:wry"]
webview-cef = ["dep:cef"]
cef-ffmpeg  = ["webview-cef", "dep:ffmpeg"]   # GPL 隔离，单独发行物
media       = ["dep:ffmpeg"]
plugin      = ["dep:wasmtime", "dep:wit-bindgen"]
```

渲染三档**单调递增**，按**消费者**切而不是按概念切：`gpu` 的消费者是将来 CEF 的纹理模式（要设备与取帧，不要方片管线），`ui-render` 的消费者是编辑器（要自绘 UI，不要 3D），`render` 才是引擎侧（demo）。三档下 `Renderer` 都是同一个类型，只是字段与方法按档收——`runtime` 因此只认**地板档** `gpu`，不必知道当前是哪一档。`naga` 不再显式声明：wgpu 自己传递依赖它，仓库里没有任何 `naga::` 引用。

`content` 之外的依赖一律 `optional = true`，只经 feature 拉入。

`winit` **不在上面任何一档里**——开窗是框架主线（`runtime` + `platform`）的能力，不是渲染的。M0 的空窗口不开任何渲染特性也必须能跑，所以 `winit` 是基础依赖。

### feature 统一，以及为什么不靠拆 crate 解决

`cargo build --workspace` 会把 `lxlake-demo` 的 `render` 统一进 `lxlake-editor`（resolver v3 也救不了普通依赖的 feature 统一）。但**按包构建就不会**——`cargo build -p lxlake-editor` 只见 editor 自己的依赖图。

所以约定是：**CI 与本地一律按包构建，不跑 `--workspace`**。这条约定替代了「为了隔离而拆 crate」。

## 发行物形状

运行时目录的形状**模仿 Unity，但代码分层不模仿**：

```text
lxlake-demo.exe        主 bin
data/                  引擎资产：locale/ models/ textures/ shaders/ fonts/
native/                经 vcpkg 引入的 C 依赖动态库，各自成目录
  cef/                 libcef.dll + resources/ + locales/（CEF 自带目录结构）
  ffmpeg/              av*.dll（仅 cef-ffmpeg 发行物）
plugins/               插件位：经那条窄 C ABI 加载
```

两件事要分清，混起来就会走进「为了打包好看而改分层」的坑：

- **引擎自己的分层不进动态库**。内部一律 rlib 静态链接（全量单态化 + 跨 crate LTO）；`dylib` 不用于分发
  ——它不打包 std、须随附与编译器精确匹配的 `std-<hash>`，且跨边界完全无法 LTO。Unity 的「main bin +
  一堆 dll」是 C++ 引擎预编译 + 脚本程序集的产物，不是一种可选形状。
- **动态库只出现在两处**：`native/` 下的 C 依赖，以及 `plugins/` 的插件位。

`native/` 从 vcpkg 方向引入，因此**要随发行物出 dll 的依赖，vcpkg triplet 必须是动态档**——静态 triplet
根本不会产生可随附的 dll。CEF 没有静态选项，必须走这条路；ffmpeg 可静可动，留到打包时再定。GPL 隔离
照旧：`cef-ffmpeg` 是单独发行物，默认发行物的 `native/ffmpeg/` 不存在。

`data/` 与 `plugins/` 的落地依赖**资产管线**（本地化、模型、纹理的外部文件），目前只有 `fonts/` 落了
地——M3 的 HUD 字体（OFL，随仓库提供），是应用按约定从 `data/` 读的第一个外部资产。其余仍是程序化
生成：M1 的图集、几何全由噪声密度场算出来。见[路线图](roadmap.md)的资产管线一节。

## 应用装配

应用只写一个返回 `Builder` 的**工厂函数**（标 `#[lxlake::entry]`），其余交给运行时：装配顺序、建窗
时机、事件派发、帧节奏都不由应用操心。运行时里有三个对象，**别混**：

| 对象          | 面向谁 | 是什么                                                                         |
| ------------- | ------ | ------------------------------------------------------------------------------ |
| `Application` | 平台   | 内部契约（`windows` / `on_event` / `on_frame` …），`Builder` 自己满足它        |
| `App`         | 用户   | 状态容器：托管状态 + `AppContext`，生命周期闭包拿到的是它                      |
| `Builder`     | 用户   | 装配面：链式声明「要什么」，`run()` 时变成 `App`（装配期与运行期是同一份数据） |

入口宏族把平台差异收在宏里：`entry` 同时产出桌面 `run()` 与 android `android_main`，两者
**互为 `cfg` 门控**（桌面构建下 android 那段不参与解析，反之亦然），共用同一个工厂函数。
两端都从 `Builder` 进运行时（`Builder::run` / `Builder::run_android`），插件因此在两端都生效。

### 窗口身份：标签是钥匙

窗口是有**身份**的，不是「那个窗口」：

- `WindowLabel`：稳定标识。`main` 是**保留标签**，只归主窗口（`Builder::main_window` 注入）
- `WindowSpec`：建窗数据（标签 + `WindowDesc`）；`Window`：运行期对象，住在 `WindowRegistry` 里
- 取窗按标签（`AppContext::window(label)`）或 `main_window()`；再建窗口走 `Builder::create_window`

标签既然是取窗的钥匙，装配期就校验（非空、不重复、`main` 不被冒用），且**所有入口走同一道闸**。
关一个窗口**不等于**退出：注册表空了才轮到事件循环退；想「关主窗即退出」，由应用在事件钩子里调
`App::exit()`。

### 路径：唯一的判断处

系统目录与应用目录的判断收在 `lxlake::path` **一处**，不引第三方目录库（`dirs` 那类在 android
上不一定好用）：

- `Paths` 是**值**（数据，装着各根目录），`BaseDirectory` 是**枚举**（参照 tauri 的 `BaseDirectory`），
  `dir(base)` / `resolve(path, base)` / `resolve_string(..)` 是取路径的三个口
- 各平台自己解析根：windows 走 `%APPDATA%` 一族，linux 走 XDG，macOS 走 `Library/…`，
  android 走 activity 递进来的沙箱根（`internal_data_path`）
- 全局装一次（`path::install`，`OnceLock`），之后 `path::dir(..)` 直接取；**没装就退临时目录兜底**，
  测试与被别的库调用时不必先装配
- 装的地方是**平台入口**（桌面 `Paths::for_app(app_id)`、android `Paths::for_android(沙箱根)`）——
  只有平台知道自己在哪、根在哪

### 日志：数据与安装分离

`LogConfig` 是**数据**（指令过滤、是否进 stdout、`FileSink` 三档：关闭 / 定文件 / 轮转），`install()`
是**动作**。分开是为了时序：

- 默认落点在应用目录下，得**先装路径**才解析得出来；而窗口与渲染后端初始化阶段的日志也该被捕获
- 于是顺序写死在平台入口：`path::install` → `LogConfig::install`，早于建事件循环。应用只声明配置
  （`Builder::log` / `log_with`），不关心什么时候装
- 重复安装静默容忍——全局订阅器一个进程只能装一次，测试并行跑时必然撞上

## 任务与异步

**帧循环拥有等待权，异步只是租客。** 事件循环怎么等待由平台决定（winit 的 `ControlFlow::WaitUntil`），任何异步机制都不得在主线程上与它争抢 park。参考项目 `luoxinglake` 的坑与选择也落在这条上：它的 tokio 跑在**独立 worker 线程**，主线程留给窗口系统。

### 任务系统（自建，M1 起）

区块生成与网格化是 CPU 密集、数据并行的作业，且要压在帧预算内——tokio 对此是错的工具（它的阻塞池没有优先级、没有窃取，语义是 I/O 阻塞而非计算）。`runtime` 自建作业系统：

- 工作窃取线程池
- poll 式任务句柄
- 主线程只在**帧边界**收结果，工作线程不碰世界状态

不引 rayon 一类现成全局池：作业线程、渲染线程、主线程的核数分配要统一持有，线程数不能交给外部全局池决定。

### 异步运行时（基础依赖，但跑在专用线程）

原本的口径是「异步运行时**不作为基础依赖**」，落地时改了：**tokio 是基础依赖**。理由是把它当可选
特性拉入，会让「日志落到文件」这种基础需求反而要开 feature。但依赖只开 `rt` / `rt-multi-thread` /
`time` / `sync`，**不碰 net / io 驱动**——真要网线的 capability 自己加特性，框架主线不为一句
`spawn` 拉进 mio。

**等待权仍在帧循环手里**：tokio 跑在**专用宿主线程**（`AsyncRuntime`，线程名与 worker 数可配），
主线程不 park 给它；任务结果经**帧边界邮箱**（`Mailbox<T>` / `MailboxSender<T>`，入队即唤醒）回到
主线程，在 `on_frame` 里排空。收尾（关停 worker、等 blocking 任务）也不落在事件循环线程上。

一句话口径没变：**异步在边缘，帧内保持同步。** 帧里能读到的只有邮箱里已经攒下的东西。

## 世界与区块

区块是 M1 引入的第一个**数据并行**负载，它的线程模型也定下了后续所有重 CPU 负载的模板。

### 数据所有权：可替换的不可变快照

世界只持 `Arc<Chunk>`（32³ 方块 + 非空气计数）。区块在生成阶段用 `&mut` 填数据，装进世界
后就再没有写口——要改方块（M2）走**换一份新的 `Arc`**（copy-on-write），而不是就地改。

由此换来两条：

- **作业不需要锁**：网格化作业的输入是邻域快照（中心区块 + 六个面邻居的 `Arc` 克隆），
  不是引用。世界之后怎么增删区块都与在跑的作业无关
- **「工作线程不碰世界状态」是类型层面成立的**，不是靠纪律

copy-on-write 的代价是「改一个方块复制 32KB」。放在「玩家点一下」这个频率上可以忽略；将来要
高频批量改（爆炸、流体），再引入 16³ 子区块细分——M2 不做。

### 线程模型

```text
  主线程（帧边界）                        worker 线程
  ────────────────                        ──────────
  stream.update(world, pool, focus)
    ├─ 申请：缺的区块 → spawn(生成) ──────→ 噪声 → Chunk（纯函数）
    ├─ 邻居齐的 → spawn(网格化) ──────────→ greedy meshing → ChunkMesh（纯函数）
    ├─ poll 句柄：收 Chunk / ChunkMesh
    └─ 写 world（插入 / 卸载）+ 网格交给渲染侧
```

- **世界只被主线程写**，且只在 `stream.update` 这一处写——帧内保持同步
- **结果只在帧边界收**：作业完成时经 `Wakeup` 打断主循环的等待，结果仍由主线程 poll
  （作业池落在 `runtime::jobs`）
- 卸载是同步的：区块立刻从世界摘除，还在跑的作业被 `cancel`，回来的结果直接丢

### 区块生命周期

```text
  申请 → Generating → PendingMesh → Meshing → Ready → 卸载
         生成作业在跑   等视距内邻居齐  网格化作业在跑   世界+GPU 就位
                                          ↑            │
                                          └── 标脏 ─────┘
```

网格化只等**视距内的**六个邻居就位（`world/stream.rs` 的 `neighbors_ready`）：视距外那边本没有
地形，越界那面照常长出来；相机移过去、真实邻居补上后，这一面会被邻居的实体几何挡在内部，
看不见，也就不必回头重算。于是每块只网格化一次，代价只剩最外圈区块晚一拍出画面。

M1 是单向流，M2 给槽位加了**回头的一步**：方块被改 → 标脏（改动点落在区块边界 1 格内时，该轴的
面邻居一起标）→ 下一帧重新提交网格化作业（吃**此刻**的邻域快照）→ 收割后走 `Renderer::upload`
覆盖上传。作业在跑的时候又标脏，那份结果按「旧了」处理，退回待网格化再排一次。空网格也照交——
`upload` 对空网格就是回收资源，挖光的区块正需要这一下。

### 相机与输入的分界

相机只吃**已映射好的轴值**（`forward` / `right` / `up` / `boost` + 鼠标位移），不认识按键、
不认识窗口事件。「W → `forward = 1`」这类映射在应用 / 输入层做。好处是改键位、加手柄、
将来 HUD 抢输入都碰不到相机与渲染代码。

M2 把这条分界落成了三层（`docs/roadmap.md` 的「输入与模拟的分界」）：

```text
  平台事件 → 键位表（core::input）→ 意图 → 模拟层（应用）→ 命令 → 世界 → 重网格化
```

- **键位表是数据**（`Keymap`：`输入源 → 意图` 的表），不是散在各处的 `match`；应用声明默认键位，
  用户覆盖留到 M3 / M4
- **意图**（`Intent`）与设备无关、不含世界坐标；**命令**（`Command`）已解析出目标坐标、可序列化，
  落地在世界侧（`WorldExecutor` → `World::apply`，见下一节）——分开是为了 M4 存档与将来的联机不必回头拆
- 相机的转向与位移也拆开了（`look` / `desired_delta` / `translate`）：位移先过一遍碰撞夹紧再落位，
  相机因此不必认识世界

### 命令的落地：总线管顺序，执行器管落地

命令身上有两件事：**顺序**与**落地**。M2 时它们混在应用的一小段帧代码里（一个 `Vec<Command>` 加
一个 for 循环），再加一类命令就得在帧函数里多接一条分支。现在拆成两层：

- `CommandBus` 只管**顺序**（`enqueue` 攒、帧边界 `flush` 按序派发），以及「这一帧产生了什么命令」
  （`describe`——M2 验收第 4 条要打印的那一行）
- `CommandExecutor` 只管**落地**，签名就是「一条命令 → 要标脏的区块」。不认的命令返回空，总线接着
  问下一个执行器：**认领即止**，否则同一条命令会被多个执行器各落地一次
- 执行器**不注册在总线里**，而是派发时按序传进来（`flush(&mut [&mut dyn CommandExecutor])`）。
  因为 `WorldExecutor` 借的是 `&mut World`，而世界由**应用**托管——要把它收进 `Box<dyn _>` 就得把
  世界也搬进总线，应用的射线 / 碰撞 / 网格化全得改道去总线里取世界
- 实体命令（`EntityId` / `EntityCommand`）在契约层已就位，`WorldExecutor` 对它松手；实体执行器等
  真有实体存储再接——**不造空壳**

## webview 双线

两种呈现形态是**平台限制，不是设计选择**：

| 形态     | 机制             | 落位                                          | 后端 |
| -------- | ---------------- | --------------------------------------------- | ---- |
| 覆盖层   | 原生子窗口       | `capability` 层，父窗口经 `raw-window-handle` | wry  |
| 纹理模式 | 离屏成纹理再绘制 | `capability` 与 `render` 的交界               | CEF  |

- 覆盖层与契约层的 overlays 同构，永远浮在最上、不可裁剪，**不参与 z 序与裁剪**
- 纹理模式可参与 z 序与裁剪，取帧 → `Renderer::import_texture`
- 分工定案：**wry 只做覆盖层、CEF 只做纹理**；能力以查询形式暴露，不靠调用方挨个试错
- **沙箱默认 fail-closed**：不允许以 `--no-sandbox` 交付，环境不支持就拒绝创建
- **GPL 隔离**：`cef-ffmpeg` 单独特性、单独发行物，默认发行物不含任何 GPL 代码

**覆盖层已落位**（M3 切 4）：

- 接口层在 `capability::webview`，**常编译**（`OverlaySpec` / `WebViewHandle` / `Config` / 能力查询），
  后端关在 `webview-wry` 特性之后
- **只做 Windows**（WebView2）。其余平台 `create_overlay` 返回 `Unsupported`——于是 linux 不必引
  webkit2gtk 的系统依赖，android 也一个字节都不拉。接口层不摆平台判据，判据在后端里
- 父窗口**不下沉到 `platform`**：装配期只声明「摆在哪、装什么」（`Builder::webview`），视口与矩形由
  运行时在**建窗时**算（`ui::place` 与自绘共用同一份 Widget 数据），句柄经 `raw-window-handle` 递给
  `build_as_child`。覆盖层因此与渲染器同构：**按窗惰建、resize / DPI 重算、摘窗释放、退出清空**

### 硬前置：pump 钩子（wry 用不上，留给 CEF）

webview 集成要先有**「外部事件源 / pump」钩子**，否则一定变成 hack。该钩子由 M0 交付**接口**
（见[路线图](roadmap.md)）。

接上 wry 之后口径要修正一处：**wry 0.57 没有 `pump` 接口**。WebView2 的活跑在宿主消息循环里，
`winit` 的事件循环本身就是它的泵，所以**覆盖层这一半用不上外部事件源钩子**。钩子的受力点在 CEF：
离屏渲染要按自己的节奏泵 message loop，那套 `EventSource` 语义到那时才真正被检验。

## 编辑器

编辑器是**独立应用**（`apps/lxlake-editor`），走框架主线、不启用 `render`。两条评估落在这里，本轮
**代码不动**。

### 现有 `ui` 对可视化编辑的适配度：加一层，不换栈

结论：**方便程度中上，但必须补一层「文档模型」。**

- 现有 `ui` 是**立即模式锚定**：每帧从 `Widget` + `Anchor` 重建 `UiTree`、出 `Quad`、`hit_test`。
  它是渲染与命中测试的良好内核，但**不是**可编辑的文档——没有持久树、没有属性内省、没有序列化、
  没有样式系统
- 可视化编辑要四样东西：① 可序列化的文档模型（存盘 / diff / 撤销）② 稳定身份（选中、拖动要跨帧
  对得上）③ 属性描述（类型 / 范围 / 枚举，Inspector 据此生成控件）④ 实时预览
- 立即模式在 ④ 上是**天然优势**（每帧重建，改一个属性下一帧就反映），在 ①②③ 上是**缺口**
- 落法因此是**加一层**：`文档（可序列化 + 可内省） → instantiate → 当前 UiTree`。可复用旧仓
  `ui/core` 的 `.lxml` IR 思路（纯 serde、零 `lxlake` 依赖、手写递归下降 + 行列号诊断），
  **不需要**引入旧仓 `ui/kit` 的 reconciler / hooks / CSS 式样式
- 一处要预先知道的缺口：`Widget` 现在只有 `Anchor` + `size` + `offset`，**没有父子层级、没有容器
  裁剪、没有流式布局**。编辑器要拖放组合容器，就得在 `ui` 上补一层容器 / 层级——这是文档层的自然
  延伸，仍不是范式变更

### 双线：积木与代码，共用「图 / 语句 IR + 求值器」

- **积木式**：旧仓 `crates/editor/blocks` 的 `Block` / `Port` / `Endpoint` / `Link` / `BlockGraph`
  是**真实装**的图 IR（serde + `validate`：块 id 重复、端口重复、连线悬空、方向不匹配、自连），
  但**只到结构**——积木目录、画布 / 连线交互、求值全没有；依赖只有 `serde`，可直接搬 IR 与校验
- **代码式**：**不接 LSP**。旧仓 `editor/lsp` 服务 `.lxml` 诊断且绑 LSP 通道，不采用；编辑器
  **内部自行解析**，写法可复用旧仓 `ui/core` 的手写递归下降 + 1-based 行列号诊断 + 尽力返回部分树
- **共同缺口**：没有求值 / 执行引擎。积木出图、代码出源码，最终都要降到同一份**可求值**的东西上
- 判断：双线的共用核心是**「图 / 语句 IR + 求值器」**。所以正确顺序是**先定求值目标**（字节码 /
  AST 解释器 / WIT 组件），再让两条线各自降到它——先做积木语言或先做代码语言都会返工

## 参考

`d:\workspace\luoxinglake` 是同一方向的早期尝试，本项目只参考其代码与契约设计，不复用其仓库组织：

- **可继承**：webview 的 `default = []` 纯接口 + feature 隔离、覆盖层/纹理二分、`Capabilities` 能力查询、`SandboxPolicy` fail-closed、宿主侧 pump 的接口要求（落到 `EventSource`，留给 CEF；wry 用不上，见上）
- **要避开**：一次性批量建壳（34 个 crate）、按概念拆包、把依赖目录做成一张需要人工维护的登记表
