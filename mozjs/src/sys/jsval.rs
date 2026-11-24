/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at http://mozilla.org/MPL/2.0/. */

//! `JS::Value` on QuickJS: the 64-bit QuickJS representation (`asBits_` = payload, `tag` = QuickJS
//! tag), layout-compatible with `servo_quickjs::JSValue`.
//!
//! Values visible to Rust never carry `JS_TAG_STRING_ROPE`; the shim flattens ropes when values
//! cross into Rust.

#![allow(non_camel_case_types)]
#![allow(non_snake_case)]

use crate::sys::jsapi::JSContext;
use crate::sys::jsapi::JSObject;
use crate::sys::jsapi::JSString;
use crate::sys::jsapi::JS::BigInt;
use crate::sys::jsapi::JS::Symbol;
use crate::sys::jsapi::JS::TraceKind;
use crate::sys::jsapi::JS::Value;

use libc::c_void;
use servo_quickjs as q;
use std::default::Default;

pub type JSVal = Value;

/// Tag of values that carry a Rust pointer (`PrivateValue`). QuickJS only uses it internally for
/// catch offsets on its own stack, so it never meets these values.
pub(crate) const TAG_PRIVATE: i64 = q::JS_TAG_CATCH_OFFSET as i64;
/// SpiderMonkey's magic values, with the `JSWhyMagic` reason as payload.
pub(crate) const TAG_MAGIC: i64 = q::JS_TAG_UNINITIALIZED as i64;

#[inline(always)]
const fn mkval(tag: i32, bits: u64) -> JSVal {
    Value {
        asBits_: bits,
        tag: tag as i64,
    }
}

#[inline(always)]
pub fn NullValue() -> JSVal {
    mkval(q::JS_TAG_NULL, 0)
}

#[inline(always)]
pub fn UndefinedValue() -> JSVal {
    mkval(q::JS_TAG_UNDEFINED, 0)
}

#[inline(always)]
pub fn Int32Value(i: i32) -> JSVal {
    mkval(q::JS_TAG_INT, i as u32 as u64)
}

#[inline(always)]
pub fn DoubleValue(f: f64) -> JSVal {
    mkval(q::JS_TAG_FLOAT64, f.to_bits())
}

#[inline(always)]
pub fn UInt32Value(ui: u32) -> JSVal {
    if ui > 0x7fffffff {
        DoubleValue(ui as f64)
    } else {
        Int32Value(ui as i32)
    }
}

#[inline(always)]
pub fn StringValue(s: &JSString) -> JSVal {
    mkval(q::JS_TAG_STRING, s as *const JSString as usize as u64)
}

#[inline(always)]
pub fn BooleanValue(b: bool) -> JSVal {
    mkval(q::JS_TAG_BOOL, b as u64)
}

#[inline(always)]
pub fn ObjectValue(o: *mut JSObject) -> JSVal {
    assert!(!o.is_null());
    mkval(q::JS_TAG_OBJECT, o as usize as u64)
}

#[inline(always)]
pub fn ObjectOrNullValue(o: *mut JSObject) -> JSVal {
    if o.is_null() {
        NullValue()
    } else {
        ObjectValue(o)
    }
}

#[inline(always)]
pub fn SymbolValue(s: &Symbol) -> JSVal {
    mkval(q::JS_TAG_SYMBOL, s as *const Symbol as usize as u64)
}

#[inline(always)]
pub fn BigIntValue(s: &BigInt) -> JSVal {
    mkval(q::JS_TAG_BIG_INT, s as *const BigInt as usize as u64)
}

#[inline(always)]
pub fn PrivateValue(o: *const c_void) -> JSVal {
    Value {
        asBits_: o as usize as u64,
        tag: TAG_PRIVATE,
    }
}

impl JSVal {
    #[inline(always)]
    #[allow(non_snake_case)]
    pub fn asBits(&self) -> u64 {
        self.asBits_
    }

    #[inline(always)]
    fn qtag(&self) -> i32 {
        self.tag as i32
    }

    #[inline(always)]
    pub fn is_undefined(&self) -> bool {
        self.qtag() == q::JS_TAG_UNDEFINED
    }

    #[inline(always)]
    pub fn is_null(&self) -> bool {
        self.qtag() == q::JS_TAG_NULL
    }

    #[inline(always)]
    pub fn is_null_or_undefined(&self) -> bool {
        self.is_null() || self.is_undefined()
    }

    #[inline(always)]
    pub fn is_boolean(&self) -> bool {
        self.qtag() == q::JS_TAG_BOOL
    }

    #[inline(always)]
    pub fn is_int32(&self) -> bool {
        self.qtag() == q::JS_TAG_INT
    }

    #[inline(always)]
    pub fn is_double(&self) -> bool {
        self.qtag() == q::JS_TAG_FLOAT64
    }

    #[inline(always)]
    pub fn is_number(&self) -> bool {
        self.is_int32() || self.is_double()
    }

    #[inline(always)]
    pub fn is_primitive(&self) -> bool {
        !self.is_object()
    }

    #[inline(always)]
    pub fn is_string(&self) -> bool {
        self.qtag() == q::JS_TAG_STRING || self.qtag() == q::JS_TAG_STRING_ROPE
    }

    #[inline(always)]
    pub fn is_object(&self) -> bool {
        self.qtag() == q::JS_TAG_OBJECT
    }

    #[inline(always)]
    pub fn is_object_or_null(&self) -> bool {
        self.is_object() || self.is_null()
    }

    #[inline(always)]
    pub fn is_magic(&self) -> bool {
        self.tag == TAG_MAGIC
    }

    #[inline(always)]
    pub fn is_symbol(&self) -> bool {
        self.qtag() == q::JS_TAG_SYMBOL
    }

    #[inline(always)]
    pub fn is_bigint(&self) -> bool {
        self.qtag() == q::JS_TAG_BIG_INT || self.qtag() == q::JS_TAG_SHORT_BIG_INT
    }

    /// Whether the value holds a QuickJS reference count (SpiderMonkey: a GC thing).
    #[inline(always)]
    pub fn is_gcthing(&self) -> bool {
        (self.qtag() as u32) >= (q::JS_TAG_FIRST as u32)
    }

    #[inline(always)]
    pub fn to_boolean(&self) -> bool {
        assert!(self.is_boolean());
        self.asBits_ != 0
    }

    #[inline(always)]
    pub fn to_int32(&self) -> i32 {
        assert!(self.is_int32());
        self.asBits_ as u32 as i32
    }

    #[inline(always)]
    pub fn to_double(&self) -> f64 {
        assert!(self.is_double());
        f64::from_bits(self.asBits_)
    }

    #[inline(always)]
    pub fn to_number(&self) -> f64 {
        assert!(self.is_number());
        if self.is_double() {
            self.to_double()
        } else {
            self.to_int32() as f64
        }
    }

    #[inline(always)]
    pub fn to_string(&self) -> *mut JSString {
        assert!(self.qtag() == q::JS_TAG_STRING, "not a flat string");
        self.asBits_ as usize as *mut JSString
    }

    #[inline(always)]
    pub fn to_object(&self) -> *mut JSObject {
        assert!(self.is_object());
        self.asBits_ as usize as *mut JSObject
    }

    #[inline(always)]
    pub fn to_object_or_null(&self) -> *mut JSObject {
        assert!(self.is_object_or_null());
        if self.is_null() {
            std::ptr::null_mut()
        } else {
            self.to_object()
        }
    }

    #[inline(always)]
    pub fn to_symbol(&self) -> *mut Symbol {
        assert!(self.is_symbol());
        self.asBits_ as usize as *mut Symbol
    }

    #[inline(always)]
    pub fn to_bigint(&self) -> *mut BigInt {
        assert!(
            self.qtag() == q::JS_TAG_BIG_INT,
            "short BigInts have no heap representation"
        );
        self.asBits_ as usize as *mut BigInt
    }

    #[inline(always)]
    pub fn to_private(&self) -> *const c_void {
        assert!(self.tag == TAG_PRIVATE);
        self.asBits_ as usize as *const c_void
    }

    #[inline(always)]
    pub fn to_gcthing(&self) -> *mut c_void {
        assert!(self.is_gcthing());
        self.asBits_ as usize as *mut c_void
    }

    #[inline(always)]
    pub fn is_markable(&self) -> bool {
        self.is_gcthing()
    }

    #[inline(always)]
    pub fn trace_kind(&self) -> TraceKind {
        assert!(self.is_markable());
        if self.is_object() {
            TraceKind::Object
        } else if self.is_string() {
            TraceKind::String
        } else if self.is_symbol() {
            TraceKind::Symbol
        } else {
            TraceKind::BigInt
        }
    }

    /// The same value as a `servo_quickjs::JSValue` (no reference count change).
    #[inline(always)]
    pub fn to_qjs(self) -> q::JSValue {
        unsafe { std::mem::transmute::<JSVal, q::JSValue>(self) }
    }

    /// Wraps a `servo_quickjs::JSValue` (no reference count change).
    #[inline(always)]
    pub fn from_qjs(v: q::JSValue) -> JSVal {
        unsafe { std::mem::transmute::<q::JSValue, JSVal>(v) }
    }
}

const _: () = assert!(std::mem::size_of::<JSVal>() == std::mem::size_of::<q::JSValue>());

impl Default for JSVal {
    fn default() -> JSVal {
        UndefinedValue()
    }
}

#[inline(always)]
pub unsafe fn JS_ARGV(_cx: *mut JSContext, vp: *mut JSVal) -> *mut JSVal {
    vp.offset(2)
}

#[inline(always)]
pub unsafe fn JS_CALLEE(_cx: *mut JSContext, vp: *mut JSVal) -> JSVal {
    *vp
}
