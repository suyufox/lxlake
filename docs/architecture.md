# 架构

## 定位

lxlake 本质是**跨平台应用框架**，同时具备游戏引擎的能力：

- **框架主线**：类 tauri 的纯跨平台应用框架，对应 `apps/lxlake-editor`
- **引擎侧线**：启用 `render` 特性后作为引擎分发，对应 `apps/lxlake-demo`
- **侧线与主线同步兼容**：侧线不独立演进，共用同一套契约与运行时

第三方依赖的尺度是**只复用底层、核心自研**：winit / wgpu / wry 这类底层可以复用，场景、命令、UI、区块等核心概念全部自己定。

## 平台范围

**只做纯设备平台，不含 web。**

| 平台       | 状态                                         |
| ---------- | -------------------------------------------- |
| windows    | 首要目标                                     |
| linux      | 目标                                         |
| android    | 目标（NDK 已就位，见[环境](environment.md)） |
| macos      | 目标，实装排后                               |
| ios        | 目标，实装排后                               |
| headless   | 无窗口后端，供测试与服务器使用               |
| web / wasm | **不做**                                     |

带 C 依赖的部分（vcpkg 清单）覆盖 windows / linux 宿主 + android 附加系；apple 平台不纳入 C 依赖清单，这不影响纯 Rust 部分的平台目标。

## 分层

分层落在**模块树**上，不落在 crate 边界上。`lxlake` 内部按职责分区：

| 模块         | 职责                                    | 关键约束                                     |
| ------------ | --------------------------------------- | -------------------------------------------- |
| `core`       | 契约：场景、Widget、命令、事件          | **不出现 winit / wgpu 类型**，不依赖任何平台 |
| `runtime`    | 事件循环、`App`、pump 钩子、任务调度    | 帧边界与外部事件源的唯一归属                 |
| `platform`   | 平台后端（窗口、输入、IME、文件系统）   | 按 `cfg(target_os)` 分档，**不按 feature**   |
| `render`     | wgpu 渲染管线、纹理导入                 | 全部落在 `render` 特性之后                   |
| `ui`         | 自绘 UI：布局、文本排版、交互           | 自绘，不引入系统控件或 HTML 渲染             |
| `capability` | webview / media / update 等可选横切能力 | 逐个 feature 隔离，能力以**查询**形式暴露    |
| `plugin`     | 插件宿主                                | 只留一条窄的、可版本化的 C ABI 边界          |

`core` 这条约束是**可机检的**：契约层一旦出现 `wgpu` 或 `winit` 类型，框架主线就再也无法在不启用渲染的情况下干净编译。

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
│   │       ├── render/
│   │       ├── ui/
│   │       ├── capability/
│   │       └── plugin/
│   └── lxlake-macros/         # proc-macro —— Cargo 硬约束，必须独立
├── apps/
│   ├── lxlake-editor/         # 纯框架：不启用 render
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
render      = ["dep:wgpu", "dep:naga"]
webview-wry = ["dep:wry"]
webview-cef = ["dep:cef"]
cef-ffmpeg  = ["webview-cef", "dep:ffmpeg"]   # GPL 隔离，单独发行物
media       = ["dep:ffmpeg"]
plugin      = ["dep:wasmtime", "dep:wit-bindgen"]
```

`content` 之外的依赖一律 `optional = true`，只经 feature 拉入。

`winit` **不在 `render` 里**——开窗是框架主线（`runtime` + `platform`）的能力，不是渲染的。M0 的空窗口不开 `render` 也必须能跑，所以 `winit` 是基础依赖。

### feature 统一，以及为什么不靠拆 crate 解决

`cargo build --workspace` 会把 `lxlake-demo` 的 `render` 统一进 `lxlake-editor`（resolver v3 也救不了普通依赖的 feature 统一）。但**按包构建就不会**——`cargo build -p lxlake-editor` 只见 editor 自己的依赖图。

所以约定是：**CI 与本地一律按包构建，不跑 `--workspace`**。这条约定替代了「为了隔离而拆 crate」。

## 任务与异步

**帧循环拥有等待权，异步只是租客。** 事件循环怎么等待由平台决定（winit 的 `ControlFlow::WaitUntil`），任何异步机制都不得在主线程上与它争抢 park。参考项目 `luoxinglake` 的坑与选择也落在这条上：它的 tokio 跑在**独立 worker 线程**，主线程留给窗口系统。

### 任务系统（自建，M1 起）

区块生成与网格化是 CPU 密集、数据并行的作业，且要压在帧预算内——tokio 对此是错的工具（它的阻塞池没有优先级、没有窃取，语义是 I/O 阻塞而非计算）。`runtime` 自建作业系统：

- 工作窃取线程池
- poll 式任务句柄
- 主线程只在**帧边界**收结果，工作线程不碰世界状态

不引 rayon 一类现成全局池：作业线程、渲染线程、主线程的核数分配要统一持有，线程数不能交给外部全局池决定。

### 异步运行时（不进基础依赖）

异步运行时**不作为 `lxlake` 的基础依赖**。它解决的是阻塞式 I/O 的并发（网络、进程、文件），属于框架侧横切能力，与引擎主线无关。落地方式：

- 契约层只出**运行时无关**的窄接口：跨线程唤醒句柄 + 帧边界排队。唤醒句柄就是一条 `Arc<dyn Fn() + Send + Sync>` 之类的回调，平台能提供就提供（winit 的 `EventLoopProxy`），提供不了就返回 `None`，控制面降级为帧边界处理
- 真出现异步消费者时（`update` / `media` 那类），按 `capability` 的可选特性拉入，tokio / smol 只是实现细节；运行时跑在专用 worker 线程，结果经唤醒句柄回主循环

一句话口径：**异步在边缘，帧内保持同步。** M0 不落任何异步代码，只交上面那条缝。

## webview 双线

两种呈现形态是**平台限制，不是设计选择**：

| 形态     | 机制             | 落位                            | 后端 |
| -------- | ---------------- | ------------------------------- | ---- |
| 覆盖层   | 原生子窗口       | `platform` 层，经 `Widget` 摆位 | wry  |
| 纹理模式 | 离屏成纹理再绘制 | `capability` 与 `render` 的交界 | CEF  |

- 覆盖层与契约层的 overlays 同构，永远浮在最上、不可裁剪，**不参与 z 序与裁剪**
- 纹理模式可参与 z 序与裁剪，取帧 → `Renderer::import_texture`
- 分工定案：**wry 只做覆盖层、CEF 只做纹理**；能力以查询形式暴露，不靠调用方挨个试错
- **沙箱默认 fail-closed**：不允许以 `--no-sandbox` 交付，环境不支持就拒绝创建
- **GPL 隔离**：`cef-ffmpeg` 单独特性、单独发行物，默认发行物不含任何 GPL 代码

### 硬前置

运行时必须先有**「外部事件源 / pump」钩子**：wry 与 CEF 都要求外部事件源约每 ~10ms 泵一次，而运行时只有重绘节奏。这条不补，webview 集成一定变成 hack。该钩子由 M0 交付**接口**（见[路线图](roadmap.md)）。

## 参考

`d:\workspace\luoxinglake` 是同一方向的早期尝试，本项目只参考其代码与契约设计，不复用其仓库组织：

- **可继承**：webview 的 `default = []` 纯接口 + feature 隔离、覆盖层/纹理二分、`Capabilities` 能力查询、`SandboxPolicy` fail-closed、`WebView::pump` 的宿主侧要求
- **要避开**：一次性批量建壳（34 个 crate）、按概念拆包、把依赖目录做成一张需要人工维护的登记表
