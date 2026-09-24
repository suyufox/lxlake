//! lxlake 的 proc-macro。
//!
//! 本 crate 独立存在是 **Cargo 硬约束**：proc-macro 只能是独立 crate。
//!
//! 入口宏族把**平台差异收在宏里**：应用只写一个返回 `lxlake::Builder` 的**工厂函数**
//! （标 `#[lxlake::entry]`），宏把它原样回插，再按需追加两个入口——
//!
//! - `pub fn run()`：桌面入口，由同包 `main.rs` 的 `fn main()` 调一行；
//! - `#[unsafe(no_mangle)] extern "C" fn android_main(..)`：Android 入口，由 activity 按符号
//!   加载。整体被 `#[cfg(target_os = "android")]` 门控，**桌面构建下这段代码不参与解析**，
//!   所以即使 `lxlake::AndroidApp` / `runtime::run_android` 尚未实装，也编译得过。
//!
//! 两者共用同一份装配（同一个工厂函数），主程序不必写任何平台分支。

use proc_macro::TokenStream;
use quote::quote;
use syn::{Item, ItemFn, parse_macro_input, parse_quote};

/// 标注应用入口的**工厂函数**，同时生成桌面 `run()` 与 Android `android_main`。
///
/// 被标注的函数必须无参数、非 `async`，且**函数块的值**就是装配好的 `lxlake::Builder`：
///
/// ```ignore
/// #[lxlake::entry]
/// fn app() -> lxlake::Builder {
///   lxlake::Builder::new().main_window(desc)
/// }
/// ```
///
/// 展开后等价于回插 `fn app()`，外加 `pub fn run()` 与（仅 android 构建的）`android_main`，
/// 两者都把 `app()` 的值交给运行时；启动失败时打印并退出。
#[proc_macro_attribute]
pub fn entry(_attr: TokenStream, item: TokenStream) -> TokenStream {
  expand_entry(item, true, true)
}

/// 只生成桌面 `pub fn run()`——纯桌面应用用它，不产出 `android_main`。
#[proc_macro_attribute]
pub fn entry_desktop(_attr: TokenStream, item: TokenStream) -> TokenStream {
  expand_entry(item, true, false)
}

/// 只生成 Android `android_main`——纯移动端应用用它，不产出桌面 `run()`。
///
/// 桌面构建下工厂函数无人引用（`android_main` 被 `cfg` 门控掉），故宏给它补一条
/// `#[allow(dead_code)]`，避免无害告警。
#[proc_macro_attribute]
pub fn entry_mobile(_attr: TokenStream, item: TokenStream) -> TokenStream {
  expand_entry(item, false, true)
}

/// 平台 `cfg` 简写：把任意条目（fn / struct / mod / impl …）限定在桌面平台编译。
///
/// 等价于 `#[cfg(any(target_os = "windows", target_os = "linux", target_os = "macos"))]`——
/// 与 `platform` 后端的桌面档一致（见 `docs/architecture.md` 平台范围）。
#[proc_macro_attribute]
pub fn desktop(_attr: TokenStream, item: TokenStream) -> TokenStream {
  let item = parse_macro_input!(item as Item);
  quote! {
    #[cfg(any(target_os = "windows", target_os = "linux", target_os = "macos"))]
    #item
  }
  .into()
}

/// 平台 `cfg` 简写：把任意条目限定在移动端平台编译。
///
/// 等价于 `#[cfg(any(target_os = "android", target_os = "ios"))]`。
#[proc_macro_attribute]
pub fn mobile(_attr: TokenStream, item: TokenStream) -> TokenStream {
  let item = parse_macro_input!(item as Item);
  quote! {
    #[cfg(any(target_os = "android", target_os = "ios"))]
    #item
  }
  .into()
}

/// 共享展开：校验 → 回插工厂函数 → 按开关追加桌面 / 移动端入口。
///
/// 参数名用宏内约定名 `__lxlake_app`，避免与用户工厂函数里的名字互遮（工厂名也禁止以
/// `__lxlake` 开头，见 [`validate`]）。
fn expand_entry(item: TokenStream, desktop: bool, mobile: bool) -> TokenStream {
  let mut factory = parse_macro_input!(item as ItemFn);

  if let Err(err) = validate(&factory) {
    return err.to_compile_error().into();
  }

  let name = factory.sig.ident.clone();

  // 仅移动端：桌面构建下工厂函数不会被任何代码引用，补一行避免死代码告警。
  if mobile && !desktop {
    factory.attrs.push(parse_quote!(#[allow(dead_code)]));
  }

  let mut generated = quote! { #factory };

  if desktop {
    generated = quote! {
      #generated

      /// 桌面入口：装配应用并交给运行时，阻塞至退出。
      ///
      /// 由同包 `main.rs` 的 `fn main()` 调用。
      pub fn run() {
        if let Err(err) = ::lxlake::runtime::run(#name()) {
          eprintln!("lxlake: {err}");
          std::process::exit(1);
        }
      }
    };
  }

  if mobile {
    generated = quote! {
      #generated

      /// Android 入口：由 activity 按 `android_main` 符号加载（仅 `target_os = "android"` 编译）。
      #[cfg(target_os = "android")]
      #[unsafe(no_mangle)]
      pub extern "C" fn android_main(__lxlake_app: ::lxlake::AndroidApp) {
        if let Err(err) = ::lxlake::runtime::run_android(__lxlake_app, #name()) {
          eprintln!("lxlake: {err}");
          std::process::exit(1);
        }
      }
    };
  }

  generated.into()
}

/// 工厂函数不能带参数，也不能是 `async`——运行时自己拥有等待权，不借用别人的 executor。
///
/// 名字还不能以 `__lxlake` 开头：那是宏内约定前缀，同名会与生成的 `android_main` 参数互遮。
fn validate(func: &ItemFn) -> Result<(), syn::Error> {
  if !func.sig.inputs.is_empty() {
    return Err(syn::Error::new_spanned(
      &func.sig,
      "入口宏标记的函数不能有参数：它是**工厂函数**，值交给运行时",
    ));
  }
  if func.sig.asyncness.is_some() {
    return Err(syn::Error::new_spanned(
      &func.sig,
      "入口宏标记的函数不能是 async：运行时自己拥有等待权",
    ));
  }
  if func.sig.ident.to_string().starts_with("__lxlake") {
    return Err(syn::Error::new_spanned(
      &func.sig.ident,
      "`__lxlake` 是入口宏的内部前缀，函数名不能用它开头",
    ));
  }
  Ok(())
}
