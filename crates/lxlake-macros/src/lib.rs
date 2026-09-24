//! lxlake 的 proc-macro。
//!
//! 本 crate 独立存在是 **Cargo 硬约束**：proc-macro 只能是独立 crate。

use proc_macro::TokenStream;
use quote::quote;
use syn::{ItemFn, parse_macro_input};

/// 标记应用入口。
///
/// 被标记的函数必须无参数，且**函数块的值**会作为应用实例交给运行时：
///
/// ```ignore
/// #[lxlake::entry]
/// fn main() -> Demo {
///   Demo::default()
/// }
/// ```
///
/// 展开后等价于 `fn main() { lxlake::runtime::run(<块的值>) }`，启动失败时打印并退出。
#[proc_macro_attribute]
pub fn entry(_attr: TokenStream, item: TokenStream) -> TokenStream {
  let func = parse_macro_input!(item as ItemFn);

  if let Err(err) = validate(&func) {
    return err.to_compile_error().into();
  }

  let attrs = &func.attrs;
  let vis = &func.vis;
  let name = &func.sig.ident;
  let body = &func.block;

  quote! {
    #(#attrs)*
    #vis fn #name() {
      if let Err(err) = ::lxlake::runtime::run(#body) {
        eprintln!("lxlake: {err}");
        std::process::exit(1);
      }
    }
  }
  .into()
}

/// 入口函数不能带参数，也不能是 `async`——运行时自己拥有等待权，不借用别人的 executor。
fn validate(func: &ItemFn) -> Result<(), syn::Error> {
  if !func.sig.inputs.is_empty() {
    return Err(syn::Error::new_spanned(
      &func.sig,
      "#[lxlake::entry] 标记的函数不能有参数",
    ));
  }
  if func.sig.asyncness.is_some() {
    return Err(syn::Error::new_spanned(
      &func.sig,
      "#[lxlake::entry] 标记的函数不能是 async",
    ));
  }
  Ok(())
}
