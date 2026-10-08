/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at http://mozilla.org/MPL/2.0/. */

//! Arbitrary bytes into the structured-clone reader (Servo reads stored clones back from
//! IndexedDB), then the result written again.

#![no_main]

#[path = "common.rs"]
mod common;

use libfuzzer_sys::fuzz_target;
use mozjs::jsval::UndefinedValue;
use mozjs::rooted;

fuzz_target!(|data: &[u8]| {
    common::with_realm(|cx, _global| {
        if let Some(v) = common::read(cx, data) {
            rooted!(&in(cx) let v = v);
            let _ = common::write(cx, v.get(), UndefinedValue());
        }
    });
});
