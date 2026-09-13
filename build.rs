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
    let config = slint_build::CompilerConfiguration::new().with_style("fluent".into());
    slint_build::compile_with_config("src/ui/slint/app.slint", config)
        .expect("compiling src/ui/slint/app.slint");
}
