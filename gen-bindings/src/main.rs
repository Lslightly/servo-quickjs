//! Regenerates `quickjs-sys/src/bindings.rs` and `quickjs-sys/src/static_wrappers.c`.
//! Run from the workspace root: `LIBCLANG_PATH=/usr/lib/llvm-18/lib cargo run -p gen-bindings`.

fn main() {
    let out = std::path::Path::new("quickjs-sys/src");
    bindgen::Builder::default()
        .header("quickjs-sys/quickjs/quickjs.h")
        .allowlist_function("(__)?JS_.*|js_.*")
        .allowlist_type("JS.*")
        .allowlist_var("JS_.*")
        .wrap_static_fns(true)
        .wrap_static_fns_path(out.join("static_wrappers"))
        .wrap_static_fns_suffix("__sys")
        .layout_tests(false)
        .size_t_is_usize(true)
        .prepend_enum_name(false)
        .derive_default(true)
        .generate()
        .expect("bindgen failed")
        .write_to_file(out.join("bindings.rs"))
        .expect("write failed");
    let wrappers = out.join("static_wrappers.c");
    let c = std::fs::read_to_string(&wrappers).unwrap();
    std::fs::write(&wrappers, c.replace("\"quickjs-sys/quickjs/quickjs.h\"", "\"quickjs.h\""))
        .unwrap();
}
