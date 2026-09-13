//! Compiles the `.slint` sources into Rust.
//!
//! Everything the front end is made of is declared in `src/ui/slint/`, next to
//! the modules that drive it: `app.slint` is the window, and it imports the
//! design tokens, the widget library and one file per page. The generated types
//! are pulled in by `slint::include_modules!()` in `src/ui/mod.rs`.

fn main() {
    println!("cargo:rerun-if-changed=src/ui/slint");

    // `fluent` is Slint's WinUI-flavoured widget set (ADR-018). We only borrow
    // its `ScrollView`; every other control is drawn in `widgets.slint`, because
    // what makes Windows 11 recognisable is the hover/press/expand transitions,
    // and a stock style does not expose those.
    //
    // `with_debug_info` 是给 `render/window_test.rs` 用的:只有带调试信息的生成代码,
    // Slint 的 `ElementHandle` 才查得到元素(元素类型名/几何),那条"没有哪一段比窗口宽"
    // 的断言就靠它 —— min-width 撑破窗口这种错,肉眼要量像素才发现(UI_GUIDE §7.14)。
    let config = slint_build::CompilerConfiguration::new()
        .with_style("fluent".into())
        .with_debug_info(true);
    slint_build::compile_with_config("src/ui/slint/app.slint", config)
        .expect("compiling src/ui/slint/app.slint");
}
