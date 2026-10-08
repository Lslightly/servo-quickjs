/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at http://mozilla.org/MPL/2.0/. */

//! Strings through the API Servo uses: UTF-16 and UTF-8 in, Latin-1/UTF-16 data, UTF-8 and Rust
//! strings out, atoms and property keys. Valid input must round-trip exactly.

#![no_main]

#[path = "common.rs"]
mod common;

use std::marker::PhantomData;
use std::ptr::NonNull;

use libfuzzer_sys::fuzz_target;
use mozjs::conversions::jsstr_to_string;
use mozjs::jsapi::mozilla::{Range, RangedPtr};
use mozjs::jsapi::{JS_GetStringLength, JS_NewUCStringCopyN, PropertyKey, UTF8Chars};
use mozjs::jsval::StringValue;
use mozjs::rooted;
use mozjs::rust::wrappers2::{
    JS_AtomizeStringN, JS_NewStringCopyUTF8N, RUST_INTERNED_STRING_TO_JSID,
};

fuzz_target!(|data: &[u8]| {
    common::with_realm(|cx, _global| unsafe {
        // UTF-16, including lone surrogates.
        let units: Vec<u16> = data
            .chunks_exact(2)
            .map(|c| u16::from_le_bytes([c[0], c[1]]))
            .collect();
        let s = JS_NewUCStringCopyN(cx.raw_cx(), units.as_ptr(), units.len());
        assert!(!s.is_null());
        rooted!(&in(cx) let s16 = StringValue(&*s));
        assert_eq!(JS_GetStringLength(s), units.len());
        let back = jsstr_to_string(cx, NonNull::new(s16.get().to_string()).unwrap());
        assert_eq!(back, String::from_utf16_lossy(&units));

        // UTF-8: valid input round-trips; invalid input must not crash.
        let range = data.as_ptr_range();
        let utf8 = UTF8Chars {
            _base: Range {
                _phantom_0: PhantomData,
                mStart: RangedPtr {
                    _phantom_0: PhantomData,
                    mPtr: range.start as *mut _,
                },
                mEnd: RangedPtr {
                    _phantom_0: PhantomData,
                    mPtr: range.end as *mut _,
                },
            },
        };
        let s = JS_NewStringCopyUTF8N(cx, &utf8);
        if let Ok(text) = std::str::from_utf8(data) {
            assert!(!s.is_null());
            rooted!(&in(cx) let s8 = StringValue(&*s));
            assert_eq!(
                jsstr_to_string(cx, NonNull::new(s8.get().to_string()).unwrap()),
                text
            );
        }
        mozjs::rust::wrappers2::JS_ClearPendingException(cx);

        // Latin-1 bytes as an atom and a property key.
        let atom = JS_AtomizeStringN(cx, data.as_ptr() as *const _, data.len());
        if !atom.is_null() {
            rooted!(&in(cx) let a = StringValue(&*atom));
            let expected: String = data.iter().map(|&b| b as char).collect();
            assert_eq!(
                jsstr_to_string(cx, NonNull::new(a.get().to_string()).unwrap()),
                expected
            );
            rooted!(&in(cx) let mut id = PropertyKey::default());
            RUST_INTERNED_STRING_TO_JSID(cx, atom, id.handle_mut());
        }
        mozjs::rust::wrappers2::JS_ClearPendingException(cx);
    });
});
