#![allow(non_camel_case_types, non_snake_case, non_upper_case_globals, unnecessary_transmutes, clippy::all)]

#[cfg(not(target_pointer_width = "64"))]
compile_error!("bindings.rs assumes the 64-bit struct JSValue representation");

include!("bindings.rs");

/// Rust equivalents of the `JS_MKVAL`/`JS_VALUE_GET_*` macros for the 64-bit representation.
impl JSValue {
    pub const fn mkval(tag: i32, val: i32) -> JSValue {
        JSValue {
            u: JSValueUnion {
                uint64: val as u32 as u64,
            },
            tag: tag as i64,
        }
    }

    pub fn mkptr(tag: i32, ptr: *mut ::std::os::raw::c_void) -> JSValue {
        JSValue {
            u: JSValueUnion { ptr },
            tag: tag as i64,
        }
    }

    pub const fn tag(&self) -> i32 {
        self.tag as i32
    }

    pub fn int(&self) -> i32 {
        unsafe { self.u.uint64 as i32 }
    }

    pub fn float64(&self) -> f64 {
        unsafe { self.u.float64 }
    }

    pub fn ptr(&self) -> *mut ::std::os::raw::c_void {
        unsafe { self.u.ptr }
    }

    pub fn has_ref_count(&self) -> bool {
        (self.tag as i32 as u32) >= (JS_TAG_FIRST as u32)
    }
}

pub const JS_NULL: JSValue = JSValue::mkval(JS_TAG_NULL, 0);
pub const JS_UNDEFINED: JSValue = JSValue::mkval(JS_TAG_UNDEFINED, 0);
pub const JS_FALSE: JSValue = JSValue::mkval(JS_TAG_BOOL, 0);
pub const JS_TRUE: JSValue = JSValue::mkval(JS_TAG_BOOL, 1);
pub const JS_EXCEPTION: JSValue = JSValue::mkval(JS_TAG_EXCEPTION, 0);
pub const JS_UNINITIALIZED: JSValue = JSValue::mkval(JS_TAG_UNINITIALIZED, 0);
