//! 可选横切能力：webview、media、update 之类**与主流程正交**的东西。
//!
//! 与 `platform` 的分工是「怎么开窗」与「支不支持某件事」的分工：
//!
//! - `platform` 按 `cfg(target_os)` 分档，每个支持的平台都有一份实装
//! - `capability` 逐个 feature 隔离重依赖，能力以**查询**形式暴露（见 [`webview::capabilities`]）
//!
//! 于是「不支持」是一种**正常的答案**，不是编译错误：编辑器（不开 `render` 也不开 `webview-wry`）
//! 能照常写 `Builder::webview(..)`，运行期只是建不起来、记一条日志——与「字体读不进来」「渲染器
//! 建不起来」同一口径（见 `docs/architecture.md` 分层与 feature 分档）。

pub mod webview;
