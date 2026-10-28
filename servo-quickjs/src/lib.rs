#![allow(
    non_camel_case_types,
    non_snake_case,
    non_upper_case_globals,
    unnecessary_transmutes,
    clippy::all
)]

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

/// `__js_rc`: the reference count is stored just before the object.
#[inline(always)]
unsafe fn ref_count(v: JSValue) -> *mut i32 {
    unsafe { (v.ptr() as *mut i32).sub(1) }
}

/// The QuickJS class name of `v` (e.g. `"HTMLDivElement"`), or `""` if it is not an object.
///
/// This is the same string `mozjs::engine::class::class_name` returns: that one reads the
/// `JSClass.name` of the class, and this one asks the runtime's class table. `mozjs` depends on
/// this crate, so its helper cannot be called from here.
///
/// ASCII names are returned by QuickJS as a pointer into the atom (no copy, `buf` unused);
/// other names are copied into `buf`, truncated to its size. Needs `rt`: with a null runtime
/// (some `JS_DupValue` calls) there is no class table to read.
#[inline(always)]
pub unsafe fn class_name<'a>(rt: *mut JSRuntime, v: JSValue, buf: &'a mut [u8]) -> &'a str {
    use std::os::raw::c_int;
    if rt.is_null() || v.tag() != JS_TAG_OBJECT {
        return "";
    }
    let ptr = unsafe {
        JS_GetClassNameRT(
            rt,
            JS_GetClassID(v),
            buf.as_mut_ptr().cast(),
            buf.len() as c_int,
        )
    };
    if ptr.is_null() {
        return "";
    }
    unsafe { std::ffi::CStr::from_ptr(ptr) }.to_str().unwrap_or("")
}

/// The `static inline` reference counting functions of quickjs.h, inlined into Rust callers.
#[inline(always)]
pub unsafe fn JS_DupValueRT(rt: *mut JSRuntime, v: JSValue) -> JSValue {
    if v.has_ref_count() {
        let mut name = [0u8; 64];
        tracing::trace!(
            target: "mozjs_gc",
            {
                tag = v.tag(),
                ptr = v.ptr() as usize,
                class = unsafe { class_name(rt, v, &mut name) },
            },
            "JS_DupValue",
        );
        unsafe { *ref_count(v) += 1 };
    }
    v
}

/// The reference count of a value that has one.
#[inline(always)]
pub unsafe fn JS_GetRefCount(v: JSValue) -> i32 {
    unsafe { *ref_count(v) }
}

#[inline(always)]
pub unsafe fn JS_DupValue(ctx: *mut JSContext, v: JSValue) -> JSValue {
    let rt = if ctx.is_null() {
        std::ptr::null_mut()
    } else {
        unsafe { JS_GetRuntime(ctx) }
    };
    unsafe { JS_DupValueRT(rt, v) }
}

#[inline(always)]
pub unsafe fn JS_FreeValueRT(rt: *mut JSRuntime, v: JSValue) {
    if v.has_ref_count() {
        let count = ref_count(v);
        *count -= 1;
        let mut name = [0u8; 64];
        tracing::trace!(
            target: "mozjs_gc",
            {
                tag = v.tag(),
                ptr = v.ptr() as usize,
                class = unsafe { class_name(rt, v, &mut name) },
            },
            "JS_FreeValue",
        );
        if *count <= 0 {
            unsafe { __JS_FreeValueRT(rt, v); }
        }
    }
}

#[inline(always)]
pub unsafe fn JS_FreeValue(ctx: *mut JSContext, v: JSValue) {
    let rt = if ctx.is_null() {
        std::ptr::null_mut()
    } else {
        unsafe { JS_GetRuntime(ctx) }
    };
    unsafe { JS_FreeValueRT(rt, v) }
}

unsafe extern "C" {
    #[link_name = "JS_FreeContext"]
    fn JS_FreeContext_out_of_line(ctx: *mut JSContext);
}

/// `JS_DupContext`, inlined (contexts carry the same reference count header as values): entering
/// a realm counts its context, on every native call and proxy trap.
#[inline(always)]
pub unsafe fn JS_DupContext(ctx: *mut JSContext) -> *mut JSContext {
    unsafe { *(ctx as *mut i32).sub(1) += 1 };
    ctx
}

/// `JS_FreeContext`, with the count decremented inline unless the context is freed.
#[inline(always)]
pub unsafe fn JS_FreeContext(ctx: *mut JSContext) {
    unsafe {
        let count = (ctx as *mut i32).sub(1);
        if *count > 1 {
            *count -= 1;
        } else {
            JS_FreeContext_out_of_line(ctx);
        }
    }
}

pub const JS_NULL: JSValue = JSValue::mkval(JS_TAG_NULL, 0);
pub const JS_UNDEFINED: JSValue = JSValue::mkval(JS_TAG_UNDEFINED, 0);
pub const JS_FALSE: JSValue = JSValue::mkval(JS_TAG_BOOL, 0);
pub const JS_TRUE: JSValue = JSValue::mkval(JS_TAG_BOOL, 1);
pub const JS_EXCEPTION: JSValue = JSValue::mkval(JS_TAG_EXCEPTION, 0);
pub const JS_UNINITIALIZED: JSValue = JSValue::mkval(JS_TAG_UNINITIALIZED, 0);
