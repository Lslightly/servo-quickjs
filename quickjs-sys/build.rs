use std::path::Path;

fn main() {
    let dir = Path::new("quickjs");
    let version = std::fs::read_to_string(dir.join("VERSION")).unwrap();
    let mut build = cc::Build::new();
    build
        .files(
            ["quickjs.c", "dtoa.c", "libregexp.c", "libunicode.c", "cutils.c"]
                .iter()
                .map(|f| dir.join(f)),
        )
        .file("src/static_wrappers.c")
        .include(dir)
        .define("_GNU_SOURCE", None)
        .define("CONFIG_VERSION", format!("\"{}\"", version.trim()).as_str())
        .flag_if_supported("-fwrapv")
        .flag_if_supported("-Wno-array-bounds")
        .flag_if_supported("-Wno-format-truncation")
        .flag_if_supported("-Wno-infinite-recursion")
        .warnings(false);
    build.compile("quickjs");
    println!("cargo:rerun-if-changed=quickjs");
    println!("cargo:rerun-if-changed=src/static_wrappers.c");
}
