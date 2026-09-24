//! lxlake-demo 的可执行入口。
//!
//! 逻辑与装配都在同包库 [`lxlake_demo`] 里：入口宏 `#[lxlake::entry]` 挂在那个库的工厂函数
//! 上，由它产出 `run()`（本文件调的就是这个）与 Android 的 `android_main`。

fn main() {
  lxlake_demo::run();
}
