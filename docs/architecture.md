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
| headless   | **规划中，未实装**：今天测试能跑只是因为单测不建窗口                |
| web / wasm | **不做**                                                            |

android **不另起一个后端**：winit 自己就带 android 后端（`android-game-activity` 特性），事件循环、
窗口抽象、`ControlFlow::WaitUntil`、跨线程唤醒全都成立，于是它与桌面共用 `platform/winit.rs`。
真正的差异只有三处，都在那一处收口——入口多一个系统递进来的 `AndroidApp`（沙箱根由它给），
`resumed` 会反复来（切后台再回前台 = 一次新的 `InitWindow`），`suspended` 要把窗口整体摘掉
（系统已销毁 surface）。`AndroidApp` 由 winit 再导出，因此不直接依赖 `android-activity` / `ndk` / `jni`。

除这四个 target 外，`platform` 后端直接 `compile_error!`（见 `platform/mod.rs`）——所以 headless 不是「有个能跑的后端」，而是**还没有**。

**平铺的是「专有物」，不是共用主干。** `platform/` 下每个平台一个模块（`windows.rs` / `linux.rs` /
`macos.rs` / `android.rs`），**平级并列**，各装各的原生能力：Windows 的 DPI 通知与 CEF 共享纹理导入、
android 的 activity 生命周期补丁、各平台通知的原生 API 等等。共用的 winit 事件循环与窗口抽象仍收在
`platform/winit.rs` 一处——把它在四个模块里各抄一份，就等于把上面那条（android 与桌面共用 winit）
换来的收益又丢掉。谁暂时没有专有物，谁就先留空。

这样切之后，`capability` 那层才是平台中立的：一个能力一个模块（见「两层」），它按平台选一层原语，
自己答「这个平台支不支持」。

带 C 依赖的部分（vcpkg 清单）覆盖 windows / linux 宿主 + android 附加系；apple 平台不纳入 C 依赖清单，这不影响纯 Rust 部分的平台目标。

**渲染后端在 Windows 上只开 D3D12**（其余平台照旧全开）。理由不是偏好，是本机 Intel Gen9 的
Vulkan 驱动一进 `adapter.request_device` 就 AV 崩掉进程；多挂一个后端只多一份「在谁的机器上
崩」的排查成本。这条落在 `render` 建实例的地方，靠 `cfg(target_os)` 分档。

## 分层

分层落在**模块树**上，不落在 crate 边界上。`lxlake` 内部按职责分区：

| 模块         | 职责                                                               | 关键约束                                           |
| ------------ | ------------------------------------------------------------------ | -------------------------------------------------- |
| `core`       | 契约：Widget、命令、事件、几何、输入、窗口                         | **不出现 winit / wgpu 类型**，不依赖任何平台       |
| `runtime`    | 事件循环、`App`、pump 钩子、任务调度                               | 帧边界与外部事件源的唯一归属                       |
| `platform`   | 平台**一层原语**：窗口 / 输入 / IME / 文件系统，按平台平铺专有模块 | 按 `cfg(target_os)` 分档，**不按 feature**         |
| `path`       | 系统目录与应用目录的**唯一判断处**                                 | 不引第三方目录库；android 走 activity 递的沙箱根   |
| `project`    | 项目：清单（`lxlake.toml`）+ 项目根 + 文档发现                     | 纯 CPU；清单是唯一入口，**不引 serde 派生**        |
| `locale`     | 本地化：文案表 + 语言回退链 + 参数插值——**规划中，未实装**         | 纯 CPU，零新依赖；不加 feature 门                  |
| `ecs`        | **最简 ECS 接口**：实体句柄、组件存储、查询、最小世界——**规划中**  | 纯 CPU，**不引第三方 ECS**；不加 feature 门        |
| `scene`      | 场景的**二级封装**（层级、变换、相机、光照）——**规划中**           | 只经 `ecs` 最简接口落数据，不认识渲染与平台        |
| `entity`     | 实体的**二级封装**（属性、模型实例、行为）——**规划中**             | 同上；体素区块不走这里，区块归 `world`             |
| `render`     | wgpu 渲染：设备 / 表面、自绘方片管线、3D 管线与纹理导入            | 只落在 `gpu` / `ui-render` / `render` 三档特性之下 |
| `world`      | 体素世界：方块注册表、区块、浮岛生成、区块流式、射线与碰撞查询     | 纯 CPU，**不含 GPU 类型**；不加 feature 门         |
| `meshing`    | 区块 → 顶点 / 索引（greedy meshing）                               | 纯 CPU，输出 POD 顶点，无 GPU 类型                 |
| `camera`     | 自由飞行相机                                                       | 只吃已映射的轴值，不认识按键与窗口事件             |
| `ui`         | 自绘 UI：布局、文本排版、交互                                      | 自绘，不引入系统控件或 HTML 渲染                   |
| `capability` | **二层能力封装**，一个能力一个模块：`webview` 已落位               | 逐个 feature 隔离重依赖，能力以**查询**形式暴露    |
| `plugin`     | 插件宿主——**规划中，未实装**（`src/plugin/` 还没建）               | 只留一条窄的、可版本化的 C ABI 边界                |

feature 只用来隔离**重依赖**（wgpu / wry / cef 这类）。`world` / `meshing` / `camera` / `ecs` /
`scene` / `entity` / `locale` 是引擎侧或应用侧概念，但都是纯 CPU，所以不加门——门多了会出现
「框架主线不知该开哪个」的混乱。

### 两层：一层给原语，二层做封装

这一版新立的一条贯穿约定，**两侧各有一对**：

- **平台侧**：`platform` 是**一层**，按平台平铺专有模块，只暴露该平台的原生能力（窗口消息、DPI、
  IME、通知、CEF 的子进程探测与共享纹理导入）；`capability` 是**二层**，一个能力一个模块，把一层
  收成**平台中立**的接口 + 能力查询。`capability::webview` 就是现成的样子：接口层常编译，后端关在
  `webview-wry` 后面，其余平台答 `Unsupported`
- **引擎侧**：`ecs` 是**一层**，只做最简接口（实体句柄、组件存储、查询、最小世界）；`scene` 与
  `entity` 是**二层**，各自在它上面做封装。外部可以只用最简接口定义自己要什么，也可以用二层封装
  快速开发——**两层都在**，不是「先做个简的、以后换成封装的」

判据是「**谁提供原语，谁做封装**」：一层不认识上层的语义，同一层的模块之间互不依赖。平摊下来每个
平台、每个能力改自己的模块，不牵动别处。

`EntityId` / `EntityCommand` 已经在 `core::command` 里——**句柄类型留在 `core`**：命令要能序列化，
而命令层不该反过来依赖 `ecs`。`ecs` 拿它当实体句柄用，代际（防 ABA）打包进那 64 位里；句柄对使用者
始终不透明。

「能力」在两处同名的 `Capabilities` 上是**两种意思**，别读混：`runtime::Capabilities` 答的是「**这个构建**里这会儿拿得到哪些资源」（`jobs` / `text` / `gpu`，`None` = 应用没配），`capability::webview::Capabilities` 答的是「**这个平台 / 这个构建**支不支持这件事」（`overlay` / `texture` / `sandbox` / `devtools`，`false` 是正常答案而不是错误）。

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
│   │       ├── platform/      # 一层：平台专有模块平铺
│   │       │   ├── mod.rs     #   公共层 + cfg 选档
│   │       │   ├── winit.rs   #   桌面与 android 共用的事件循环 / 窗口主干
│   │       │   ├── windows.rs #   各平台**专有**物（DPI、CEF 子进程与共享纹理导入、通知…）
│   │       │   ├── linux.rs   #   没有专有物就留空，**不复制**共用主干
│   │       │   ├── macos.rs
│   │       │   └── android.rs #   activity 生命周期补丁、surface 销毁、通知
│   │       ├── path.rs        # 系统目录与应用目录的唯一判断处
│   │       ├── project.rs     # 项目：清单 + 项目根 + 文档发现
│   │       ├── world/         # 体素世界（block / chunk / terrain / stream / raycast / collision）
│   │       ├── meshing/
│   │       ├── camera.rs
│   │       ├── render/
│   │       ├── ui/
│   │       └── capability/    # 二层：一个能力一个模块
│   │           ├── mod.rs
│   │           └── webview/   #   已落位：接口常编译，webview-wry 出 Windows 后端
│   │       # ecs/           —— 规划中：最简 ECS 接口（见上面模块表）
│   │       # scene/         —— 规划中：场景的二级封装
│   │       # entity/        —— 规划中：实体的二级封装
│   │       # locale.rs      —— 规划中：本地化
│   │       # plugin/        —— 规划中，还没建（见上面模块表）
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
# 现状：`crates/lxlake/Cargo.toml` 里就是这四档，逐字一致
gpu         = ["dep:wgpu"]                        # 设备 / 队列 / 表面 / 取帧
ui-render   = ["gpu", "dep:bytemuck"]             # + 自绘方片管线与 UI 图集
render      = ["ui-render"]                       # + 3D 管线 / 方块图集 / 深度与阴影
webview-wry = ["dep:wry"]                         # Windows 的 WebView2 覆盖层

# 规划：**都还没实装**，Cargo.toml 里并不存在这几行，别照着开
webview-cef = ["dep:cef"]
cef-ffmpeg  = ["webview-cef", "dep:ffmpeg"]   # GPL 隔离，单独发行物
media       = ["dep:ffmpeg"]
plugin      = ["dep:wasmtime", "dep:wit-bindgen"]
```

渲染三档**单调递增**，按**消费者**切而不是按概念切：`gpu` 的消费者是将来 CEF 的纹理模式（要设备与取帧，不要方片管线），`ui-render` 的消费者是编辑器（要自绘 UI，不要 3D），`render` 才是引擎侧（demo）。三档下 `Renderer` 都是同一个类型，只是字段与方法按档收——`runtime` 因此只认**地板档** `gpu`，不必知道当前是哪一档。`naga` 不再显式声明：wgpu 自己传递依赖它，仓库里没有任何 `naga::` 引用。

`webview-wry` **不挂渲染三档**——覆盖层是原生子窗口，不需要设备。四档今天都各自单独构建过（`cargo build -p lxlake --no-default-features --features <档>`）。

重依赖（wgpu / wry / 将来的 cef）一律 `optional = true`，只经 feature 拉入。

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
- **动态库只出现在两处**：`native/` 下的 C 依赖，以及 `plugins/` 的插件位——**这两处今天都还不存在**
  （`native/` 等 CEF 那一步，`plugins/` 等上面的 `plugin` 模块）。

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
两端都从 `Builder` 进运行时（`Builder::run` / `Builder::run_android`），插件将来也在两端都生效（`plugin` 模块尚在规划）。

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
  `ecs` 最简接口落地再接——**不造空壳**

## 场景与实体：ECS 两层

引擎侧**全线走一个自研 ECS**，不复用第三方（bevy_ecs / hecs 那类）——仓库既有的口径是「场景、命令、
UI、区块这类核心概念全部自己定」，而 ECS 正是场景的存储本体。纯 CPU，**不加 feature 门**。

### 一层：`ecs` 最简接口

只做四件事，多一件都不加：

- **实体句柄**：就是 `core::command::EntityId`（类型留在契约层，见上面「两层」那节），代际打包进
  那 64 位，防句柄回收后再被命中（ABA）
- **组件存储**：按类型分列的列式存储。**布局留到实装时按查询形状定**（稀疏集还是 archetype），
  接口不暴露布局
- **查询**：按「有哪些组件」筛实体，出迭代器
- **最小世界**：只管生死（spawn / despawn）与上面三样的所有权

**不做系统调度器**：引擎的帧节奏已经由 `runtime` 定了，再叠一层调度器会出现两个「什么时候跑什么」
的权威。

### 二层：`scene` 与 `entity`

**两层都在**，不是「先做个简的、以后换成封装的」——一层永远留着，外部可以只用它，也可以在它上面
定义自己要的封装：

- **`scene`**：场景的封装——层级（父子变换）、变换、相机、光照。它就是「场景」这个词的落点，
  也是先前模块表里写 `core` 职责含「场景」那条的**正主**（`core` 里没有 scene.rs，也不需要）
- **`entity`**：世界里的实体的封装——属性、模型实例、行为。**体素区块不走这里**：
  「体素管世界，模型管世界里的实体」（见[路线图](roadmap.md)的资产管线）

谁先有真东西谁先建，**不为对称先搭空壳**。

### 与 `world` 的分界

两者互不依赖，也都不认识对方：

- `world` 管方块与区块（体素数据、贪心网格化）——**那些不是实体**，不进 ECS
- `ecs` 管世界里的那些「东西」——渲染走模型那条上传路径，与体素的路径并列
- 命令落地因此有两个执行器：`WorldExecutor`（方块命令 → 标脏区块，已有）与**实体执行器**（等 `ecs`）

## webview 双线

两种呈现形态是**平台限制，不是设计选择**：

| 形态     | 机制             | 落位                                          | 后端 |
| -------- | ---------------- | --------------------------------------------- | ---- |
| 覆盖层   | 原生子窗口       | `capability` 层，父窗口经 `raw-window-handle` | wry  |
| 纹理模式 | 离屏成纹理再绘制 | `capability` 与 `render` 的交界               | CEF  |

- 覆盖层与契约层的 overlays 同构，永远浮在最上、不可裁剪，**不参与 z 序与裁剪**
- 纹理模式可参与 z 序与裁剪。**CEF 自带的离屏渲染（OSR）就是这一形态的落点**：加速 OSR 要求 CEF
  以 GPU 支持构建 + `windowless_rendering_enabled = true`，而回调 `OnAcceleratedPaint` 给的是
  **只在回调期内有效的共享纹理句柄**，不是像素缓冲——所以「取帧 → 纹理」在**每个平台**都得自己走
  一条路（见下面「OSR 的导入按平台分」）；仓库里这条路**还不存在**
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

### OSR 的导入按平台分（一层原语）

CEF 的离屏渲染是自带能力，但**它给的不是一张能直接用的纹理**，而是「一个句柄 + 一句『回调结束前用完』」。
于是导入必然是**每平台一份适配器**——这正是「平台专有模块平铺」要装的那类东西：

| 平台    | CEF 给的东西                            | 导入路径                                                                         |
| ------- | --------------------------------------- | -------------------------------------------------------------------------------- |
| Windows | 池化的共享 D3D 纹理句柄（回调期内有效） | 回调里用 D3D11 `CopyResource` 拷进自有共享纹理 → D3D12 `OpenSharedHandle` → wgpu |
| macOS   | `IOSurfaceRef`                          | `CFRetain` → `MTLTexture` → wgpu Metal                                           |
| Linux   | 原生 pixmap / DMA-BUF 平面              | `dup` 每个 fd → `VK_EXT_external_memory_dma_buf` → wgpu Vulkan                   |

两条坑写在这里，免得实装时再查：

- **句柄不能存过回调**。Windows 那个句柄只在 `OnAcceleratedPaint` 返回前有效，必须当场拷一份；macOS
  要 `CFRetain`，Linux 的 fd 也要 `dup`。存下来慢慢用 = 花屏或崩
- **子进程税**：CEF 靠**重执行宿主二进制**起 renderer / GPU / utility 进程，所以 `main()` 的**最开头**
  必须探测「我是不是子进程」并立即按给定码退出，否则 GPU 进程根本不起、`OnAcceleratedPaint` 一次不来。
  这条不属于 `Builder`，它落在**入口**上——`#[lxlake::entry]` 生成的 `run()` 第一行就该做；应用自己
  在 `fn main` 里写了前置语句的，也得先调它

还有一条**只在非 Windows 受力**：Windows 的 OSR 走 CEF 自己的专用消息循环线程，宿主不用管；其余平台
要在宿主 tick 上调 `do_message_loop_work()`——就是下面那节说的 pump 钩子。

### 硬前置：pump 钩子（wry 用不上，留给 CEF）

webview 集成要先有**「外部事件源 / pump」钩子**，否则一定变成 hack。该钩子由 M0 交付**接口**
（见[路线图](roadmap.md)）。

接上 wry 之后口径要修正一处：**wry 0.57 没有 `pump` 接口**。WebView2 的活跑在宿主消息循环里，
`winit` 的事件循环本身就是它的泵，所以**覆盖层这一半用不上外部事件源钩子**。

CEF 那边还要再收一格：**Windows 的 OSR 用 CEF 自己的专用消息循环线程**，宿主不必泵；**非 Windows
才要在宿主 tick 上调 `do_message_loop_work()`**。所以这套 `EventSource` 语义到那时真正受力的是
linux / macOS 那条路——它今天仍属「接口有、语义未检验」。

## 编辑器

编辑器是**独立应用**（`apps/lxlake-editor`），走框架主线、不启用 `render`。「文档模型」这一层已按
下面的评估落位（项目概念、`.lxml` 只读装载、父子层级）；积木 / 代码那两条线仍是评估。

### 已落位：项目概念 + 文档层（M3 切 4 之后）

- **项目 = 一个目录**：清单 `lxlake.toml` 所在的目录**就是**项目根——避免「清单说根在哪、根在哪说
  清单在哪」的循环。清单给 `name` / `version`（缺省 `0.1.0`）/ `entry`，文档放 `ui/` 下；
  `Project::open` 递归发现并按相对路径排序，`entry` 不在其列就报错
- 清单用 `toml`（普通依赖，与 `ab_glyph` 同档，**不加 feature 门**），但**不做 serde 派生**：手工取
  字段才给得出中文诊断；语法错的字节偏移再转 1-based 行列号，缺字段类诊断没有源位置（列 0）
- **文档层**：`Document` / `DocNode` 是有身份的持久树。`NodeId` 优先取 `key`，否则「类型#索引」；
  有父时父的身份数字并进路径，于是**根级 id 与旧规则完全一致**，旧文档的选中态不会错位
- **`.lxml` 装载只读**（`from_lxml`）：标签名 → 节点类型（根元素也入文档），认
  `name` / `anchor` / `x` / `y` / `width` / `height` 与旧仓的 `rect="x,y,w,h"`；认不出的属性出
  `Warning` 而非静默丢弃，`#text` 出 `Hint`。**写盘尚未做**
- **层级摆位**：子节点的容器是**父节点的矩形**，`instantiate` 递归算完各自矩形再入树
  （`UiTree::add_placed`）；代价是这棵树**不该再调 `layout_in`**，那会把层级压平
- 编辑器因此**不再自己构造文档**：命令行给项目根（缺省 `data/projects/sample`），打开项目装 `entry`；
  左栏结构树按 `walk()` 前序出层级（缩进即深度），预览区描边、右栏 Inspector 改属性 ⇒ 下一帧投影出
  新矩形。**打开失败不 panic**：文案画在预览区，文档留空，结构树与 Inspector 自然空掉
- **仍需补**：容器**裁剪**、流式布局、`.lxml` 写盘、增删节点、撤销

### 现有 `ui` 对可视化编辑的适配度：加一层，不换栈

结论：**方便程度中上，但必须补一层「文档模型」。**

- 现有 `ui` 是**立即模式锚定**：每帧从 `Widget` + `Anchor` 重建 `UiTree`、出 `Quad`、`hit_test`。
  它是渲染与命中测试的良好内核，但**不是**可编辑的文档——没有持久树、没有属性内省、没有序列化、
  没有样式系统
- 可视化编辑要四样东西：① 可序列化的文档模型（存盘 / diff / 撤销）② 稳定身份（选中、拖动要跨帧
  对得上）③ 属性描述（类型 / 范围 / 枚举，Inspector 据此生成控件）④ 实时预览
- 立即模式在 ④ 上是**天然优势**（每帧重建，改一个属性下一帧就反映），在 ①②③ 上是**缺口**
- 落法因此是**加一层**：`文档（可序列化 + 可内省） → instantiate → 当前 UiTree`。可复用旧仓
  `ui/core` 的 `.lxml` IR 思路（纯 CPU、零依赖、手写递归下降 + 行列号诊断），
  **不需要**引入旧仓 `ui/kit` 的 reconciler / hooks / CSS 式样式
- 缺口 ①（持久树 + 稳定身份 + 属性描述）**已按上面一节落位**，父子层级也补上了（子节点容器 = 父节点
  矩形）；**容器裁剪、流式布局仍缺**——编辑器要拖放组合容器，还得再补这两样，仍是文档层的自然延伸，
  不是范式变更

### 双线：积木与代码，共用「图 / 语句 IR + 求值器」

- **积木式**：旧仓 `crates/editor/blocks` 的 `Block` / `Port` / `Endpoint` / `Link` / `BlockGraph`
  是**真实装**的图 IR（serde + `validate`：块 id 重复、端口重复、连线悬空、方向不匹配、自连），
  但**只到结构**——积木目录、画布 / 连线交互、求值全没有；依赖只有 `serde`，可直接搬 IR 与校验
- **代码式**：**不接 LSP**。旧仓 `editor/lsp` 服务 `.lxml` 诊断且绑 LSP 通道，不采用；编辑器
  **内部自行解析**，写法可复用旧仓 `ui/core` 的手写递归下降 + 1-based 行列号诊断 + 尽力返回部分树
- **共同缺口**：没有求值 / 执行引擎。积木出图、代码出源码，最终都要降到同一份**可求值**的东西上
- 判断：双线的共用核心是**「图 / 语句 IR + 求值器」**。所以正确顺序是**先定求值目标**（字节码 /
  AST 解释器 / WIT 组件），再让两条线各自降到它——先做积木语言或先做代码语言都会返工

## 能力规划

能力分**两侧**铺：**应用侧**（框架主线，`apps/lxlake-editor` 那条线，开 `ui-render`）与**引擎侧**
（启用 `render` 之后，`apps/lxlake-demo` 那条线）。口径先定死三条：

- **已落位的只一行带过**，判断依据是代码（括号里给落点），不在这一节复述用法
- **重点写没实装的、以及还没设计的**——那才是规划
- **只定边界、依赖与先后，不挂里程碑号**。里程碑是[路线图](roadmap.md)的事，这里只回答「谁卡谁」

### 应用侧能力（框架主线）

| 能力       | 现状（落点在代码里）                                                    | 缺口 / 待设计                                                                        |
| ---------- | ----------------------------------------------------------------------- | ------------------------------------------------------------------------------------ |
| 事件循环   | `runtime::run` / `run_android`、`Application`、`FrameClock`             | 「无窗口应用」表达得出来，但 **headless 后端还没有**                                 |
| 装配面     | `Builder` 的链式口、`App::manage`、`#[lxlake::entry]`                   | 无                                                                                   |
| 多窗口     | `WindowRegistry` / `WindowLabel` / `WindowSpec`；渲染器与覆盖层按窗惰建 | 窗口间通信（广播 / 定向消息）还没设计                                                |
| 路径       | `path::{Paths, BaseDirectory}`，各平台自己解析根                        | 无                                                                                   |
| 日志       | `runtime::log`（`LogConfig` / `Rotation` / `FileSink`）                 | 轮转之外的保留策略（总量上限、清理）还没做                                           |
| 作业池     | `runtime::jobs`（`JobPool` / `JobHandle` / `JobContext`）               | 无                                                                                   |
| 异步运行时 | `runtime::exec`（`AsyncConfig` / `AsyncRuntime` / `Mailbox`）           | 无                                                                                   |
| 命令总线   | `runtime::command`（总线管序、执行器管落地）                            | **没有实体存储**：`EntityCommand` 在契约层就位却无人执行——等 `ecs`                   |
| 文本排版   | `ui::text`（`TextShaper` / `GlyphKey` / `GlyphBitmap`）                 | 字体族、回退链、富文本（一段里混样式）都还没有                                       |
| 输入       | `core::input`（`Key` / `MouseButton` / `Keymap` / `IntentState`）       | **IME 未做**（模块表写了、实装没有）；手柄与 android 触摸同理                        |
| 自绘 UI    | `ui`（`Widget` / `Anchor` / `UiTree` / `Quad` / `hit_test`）            | **裁剪、流式布局**；`Widget` 本身无父子，层级只在文档层                              |
| 文档层     | `ui::doc`（`Document` / `DocNode` / `instantiate` / `from_lxml`）       | `.lxml` **写盘**、增删节点、撤销 / 重做、文本输入                                    |
| 项目       | `project.rs`（`lxlake.toml` + 项目根 + 文档发现）                       | **构建打包**（项目 → 发行物那一步完全没有）                                          |
| webview    | `capability::webview` 接口常编译，`webview-wry` 出 Windows 覆盖层       | 纹理模式（CEF）；linux / android 的后端（今天一律 `Unsupported`）                    |
| 插件       | 只有进程内 `Plugin` trait 的接位（`runtime::builder`）                  | 宿主本身：`src/plugin/` **还没建**，C ABI / wasmtime 未定                            |
| 平台入口   | windows / linux / macos / android 四 target（`platform/mod.rs` 分档）   | **ios 后端**、headless 后端；**平台专有模块还没平铺**（今天只有 `winit.rs`）         |
| 通知       | **全缺**                                                                | 一层在平台专有模块（各平台原生 API），二层做 `capability::notify`（接口 + 能力查询） |
| 本地化     | **全缺**                                                                | `locale` 模块：文案表 + 语言回退链 + 参数插值；纯 CPU、零新依赖                      |
| 系统集成   | **全缺**（通知与本地化已单列，这里指其余）                              | 托盘、剪贴板、文件对话框、全局快捷键——**连文档都还没提过**                           |
| 横切能力   | `capability` 下只有 `webview` 一个                                      | `notify`（二层的规矩已立，代码未建）、`media` / `update`                             |

### 引擎侧能力

| 能力     | 现状（落点在代码里）                                                       | 缺口 / 待设计                                                                                                                                                        |
| -------- | -------------------------------------------------------------------------- | -------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| 渲染三档 | `render::{GpuContext, Renderer, Pipelines, Atlas}`，逐档收字段与方法       | **`gpu` 档今天没有调用方**——它的消费者（CEF 纹理模式）还没来                                                                                                         |
| 纹理导入 | **无**                                                                     | CEF 的加速 OSR 给的是**回调期内的共享纹理句柄**，导入按平台分（D3D12 `OpenSharedHandle` / Metal `IOSurface` / Vulkan `DMA-BUF`）——它是**一层原语**，不是一个中立接口 |
| 世界     | `world`（`BlockRegistry` / `Chunk` / `IslandGenerator` / `ChunkStreamer`） | 方块行为、方块实体、光照传播                                                                                                                                         |
| 网格化   | `meshing::mesh_chunk`（greedy）                                            | 贪心之外的 LOD；动画网格（骨骼 / 形变）                                                                                                                              |
| 相机     | `camera::{FlyCamera, CameraInput}`                                         | 其他模式（轨道 / 跟随 / 过场）                                                                                                                                       |
| 查询     | `raycast` / `sweep`（体素专用）                                            | 通用物理（刚体 / 约束）**不在计划内**，别把体素碰撞当物理引擎                                                                                                        |
| 命令落地 | `WorldExecutor`（方块类命令 → 标脏区块）                                   | **实体执行器**——等 `ecs` 最简接口                                                                                                                                    |
| ECS      | **零**；`src/ecs/` 不存在                                                  | **最简接口**：实体句柄、组件存储、查询、最小世界。自研、纯 CPU，一切下游的前提                                                                                       |
| 场景     | **零**；`src/scene/` 不存在                                                | **二级封装**：层级、变换、相机、光照                                                                                                                                 |
| 实体     | **零**；`src/entity/` 不存在                                               | **二级封装**：属性、模型实例、行为——「世界里的实体」，区块不在此列                                                                                                   |
| 存档     | **未开始**                                                                 | 区块与实体的持久化、版本迁移；命令可序列化是给它留的口子                                                                                                             |
| 资产管线 | 只有 `data/fonts/` 落地；模型基准定为 glTF 2.0 `.glb`（见路线图）          | 本地化文案表、模型、纹理、着色器的装载与烘焙；`data/` 其余目录仍是空的                                                                                               |
| 音频     | **零**                                                                     | 播放 / 混音 / 3D 定位音；`media` 特性是它的重依赖位                                                                                                                  |
| 动画     | **零**                                                                     | 骨骼 / 关键帧 / 状态机                                                                                                                                               |
| 联机     | **零**                                                                     | 命令可序列化只是前提之一，网络层与同步模型都还没设计                                                                                                                 |

### 缺口清单：提到过但没实装

单列这一档，是为了**不再把规划当现状读**。正文里标注过的「像已有、其实没有」都在这里，给证据位置。

| 缺口                                                           | 文档里的说法                                     | 代码事实                                                                                                                                    |
| -------------------------------------------------------------- | ------------------------------------------------ | ------------------------------------------------------------------------------------------------------------------------------------------- |
| 取帧 → 纹理（`import_texture` 一类）                           | webview 双线一节曾当已有接口写                   | 全仓不存在（已改成标注）。而且它**不该是一个中立接口**：CEF 给的是回调期内的共享纹理句柄，导入按平台分                                      |
| `core/scene.rs`                                                | 模块表曾写 core 职责含「场景」                   | 文件不存在，**也不再需要**——场景改由 `ecs` 之上的 `scene` 二级封装承担；`core/` 下只有 command / event / geometry / input / window / widget |
| `src/plugin/`                                                  | 模块表与发行物一节都提到插件位                   | 目录未建，只有 `Builder::plugin` + `Plugin` trait 接位                                                                                      |
| headless 后端                                                  | 平台表列了 headless                              | 未实装；单测能跑只是因为不建窗口（平台表已改）                                                                                              |
| `webview-cef` / `cef-ffmpeg` / `media` / `plugin` 四个 feature | feature 一节列过                                 | `Cargo.toml` 里**没有这几行**（feature 一节已加警示）                                                                                       |
| `gpu` 档                                                       | 列为渲染三档的地板                               | 构建得过，但**没有任何调用方**；`runtime` 只把它当能力地板                                                                                  |
| IME                                                            | `platform` 模块表写「窗口、输入、IME、文件系统」 | 无实装、无论文；输入只有键与鼠标                                                                                                            |
| 系统集成（托盘 / 剪贴板 / 文件对话框 / 全局快捷键）            | **从没提过**                                     | 代码全无——属于「连规划都没写」的那一类（通知与本地化本轮已入册，见上）                                                                      |

模块表里已经明确标了「规划中，未实装」的那几个（`ecs` / `scene` / `entity` / `locale` / `notify` /
`plugin`）不在此列——**标注已经跟着走了**；这张表只收「没标注却像已有」的。

### 依赖与先后

**谁卡谁**（箭头 = 前者是后者的前置）：

```text
ecs 最简接口 ──┬─→ 实体命令执行器（WorldExecutor 的下一档）
               ├─→ 存档（区块 + 实体）
               ├─→ 动画（骨骼要绑到实体上）
               ├─→ 脚本 / 插件绑定（「操作谁」得先有个谁）
               └─→ scene / entity 二级封装 ──→ 场景里的模型实例（模型资产得挂在实体上）

platform 专有模块（一层）──→ capability 各模块（二层）──→ 通知 / webview 纹理模式 / …
   CEF 那条要的专有物有两件：子进程探测（落在入口）+ 共享纹理导入（D3D12 OpenSharedHandle）
gpu 档（设备与取帧，已有）＋ 上面那件导入 ──→ CEF 纹理模式
pump 钩子（接口有，语义未检验）········→ 只在**非 Windows** 受力（Windows 用 CEF 自己的消息循环线程）

求值目标（字节码 / AST / WIT 组件）──→ 代码语言与积木语言 ──→ 脚本插件边界 ──→ 插件宿主

ui 裁剪 + 流式布局 ─┐
locale ────────────┼─ 不依赖任何其它能力，随时可插
IME ───────────────┘
```

由此得到的先后（**只排依赖，不排工期**）：

1. **`ecs` 最简接口**先做——它解锁的下游最多（命令落地、存档、动画、脚本绑定、二级封装全等它），
   也是唯一一个「不做就卡住一片」的。它纯 CPU，不欠渲染也不欠平台
2. **平台专有模块平铺**跟 `ecs` **互不依赖**，可以并行。平铺**只建有专有物的那几个**（今天看得见
   的是 Windows 的 CEF 两件与 android 的生命周期补丁），其余留空——**不为对称而建空文件**
3. **`capability` 的二层要等一层有东西**才封得出来：`notify` 等的通知原语在平台层，`webview` 的
   纹理模式等的共享纹理导入在 Windows 专有模块
4. 所以 **CEF 纹理模式的入口不是「先引 cef」**，而是先把 Windows 专有模块平出来（子进程探测 +
   共享纹理导入、D3D12 `OpenSharedHandle`）——设备与取帧 `gpu` 档已经有了，这三件齐了才轮到 cef 自己
5. **先定求值目标，再定插件边界，最后才谈插件宿主**。反过来做（先挑 wasmtime、先写 C ABI）会返工：
   边界形状由 IR 与求值器决定，不由宿主运行时决定
6. **`ui` 裁剪 / 流式布局**、**`locale`**、**IME** 不欠任何能力，随时可以插进来；编辑器那条线的
   「拖放组合容器」只等裁剪
7. 资产管线、存档、音频、动画、联机是**各自独立的长线**，互不为前置；谁先做看应用先需要什么

**通知这条要单独定一次依赖口径**：一层是各平台原生 API，Windows 上要发真 toast 就得引 `windows`
（WinRT）——那是重依赖，按仓库口径该进 `notify` 特性；不引就只能退到托盘气泡或无实现。这条留到
实装时再拍，先按「能不加依赖就不加」处理。

## 参考

`d:\workspace\luoxinglake` 是同一方向的早期尝试，本项目只参考其代码与契约设计，不复用其仓库组织：

- **可继承**：webview 的 `default = []` 纯接口 + feature 隔离、覆盖层/纹理二分、`Capabilities` 能力查询、`SandboxPolicy` fail-closed、宿主侧 pump 的接口要求（落到 `EventSource`，留给 CEF；wry 用不上，见上）
- **要避开**：一次性批量建壳（34 个 crate）、按概念拆包、把依赖目录做成一张需要人工维护的登记表
