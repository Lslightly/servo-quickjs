/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at http://mozilla.org/MPL/2.0/. */

//! Generates the SpiderMonkey API (bindgen types, stubbed functions) from `upstream-bindgen/`,
//! forwarding every function implemented in `src/engine` (see `stub_bindings.py`).

use std::path::PathBuf;
use std::process::Command;

fn main() {
    let out = PathBuf::from(std::env::var("OUT_DIR").unwrap());
    for name in ["jsapi.rs", "gluebindings.rs"] {
        let status = Command::new("python3")
            .arg("stub_bindings.py")
            .arg(format!("upstream-bindgen/{name}"))
            .arg(out.join(name))
            .arg("src/engine")
            .status()
            .expect("failed to run python3");
        assert!(status.success(), "stub_bindings.py failed for {name}");
    }
    println!("cargo:rerun-if-changed=stub_bindings.py");
    println!("cargo:rerun-if-changed=upstream-bindgen");
    println!("cargo:rerun-if-changed=src/engine");
}
