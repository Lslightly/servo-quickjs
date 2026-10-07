/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at http://mozilla.org/MPL/2.0/. */

//! ArrayBuffers, typed arrays and DataViews.
//!
//! QuickJS never moves buffer storage, so the data pointers handed out stay valid until script
//! runs or the buffer is detached. Patch Q8 reads buffers and views without a context or
//! exceptions. Without cross-compartment wrappers, the `Unwrap*` functions only check the class.
//!
//! Contents passed to `NewArrayBufferWithContents` and returned by `StealArrayBufferContents`
//! are QuickJS allocations (`js_malloc_rt` / `js_free_rt`; plain `malloc` with the default
//! allocator).

use std::ffi::{c_int, c_void};
use std::mem::size_of;
use std::ptr;

use quickjs_sys as q;

use super::{adopt_object, obj_val, val_obj, Cx};
use crate::sys::jsapi as root;
use root::JS::Scalar::Type;

/// A (Shared)ArrayBuffer, typed array or DataView.
struct Buf {
    kind: q::JSBufferKind,
    info: q::JSBufferInfo,
}

impl Buf {
    unsafe fn of(obj: *mut root::JSObject) -> Option<Buf> {
        if obj.is_null() {
            return None;
        }
        let mut info = q::JSBufferInfo::default();
        let kind = q::JS_GetBufferInfo(obj_val(obj), &mut info);
        (kind != q::JS_BUFFER_NONE).then_some(Buf { kind, info })
    }

    fn is_array_buffer(&self) -> bool {
        self.kind == q::JS_BUFFER_ARRAY_BUFFER
    }

    fn is_shared_array_buffer(&self) -> bool {
        self.kind == q::JS_BUFFER_SHARED_ARRAY_BUFFER
    }

    fn is_buffer(&self) -> bool {
        self.is_array_buffer() || self.is_shared_array_buffer()
    }

    fn is_view(&self) -> bool {
        self.kind == q::JS_BUFFER_TYPED_ARRAY || self.kind == q::JS_BUFFER_DATA_VIEW
    }

    fn is_typed_array(&self) -> bool {
        self.kind == q::JS_BUFFER_TYPED_ARRAY
    }

    /// The element type of a typed array; `MaxTypedArrayViewType` for anything else.
    fn scalar_type(&self) -> Type {
        let Ok(ty) = q::JSTypedArrayEnum::try_from(self.info.type_) else {
            return Type::MaxTypedArrayViewType;
        };
        match ty {
            q::JS_TYPED_ARRAY_UINT8C => Type::Uint8Clamped,
            q::JS_TYPED_ARRAY_INT8 => Type::Int8,
            q::JS_TYPED_ARRAY_UINT8 => Type::Uint8,
            q::JS_TYPED_ARRAY_INT16 => Type::Int16,
            q::JS_TYPED_ARRAY_UINT16 => Type::Uint16,
            q::JS_TYPED_ARRAY_INT32 => Type::Int32,
            q::JS_TYPED_ARRAY_UINT32 => Type::Uint32,
            q::JS_TYPED_ARRAY_BIG_INT64 => Type::BigInt64,
            q::JS_TYPED_ARRAY_BIG_UINT64 => Type::BigUint64,
            q::JS_TYPED_ARRAY_FLOAT16 => Type::Float16,
            q::JS_TYPED_ARRAY_FLOAT32 => Type::Float32,
            q::JS_TYPED_ARRAY_FLOAT64 => Type::Float64,
            _ => Type::MaxTypedArrayViewType,
        }
    }

    /// The underlying (Shared)ArrayBuffer (the object itself for buffers).
    fn buffer(&self) -> *mut root::JSObject {
        val_obj(self.info.buffer)
    }

    fn data(&self) -> *mut u8 {
        self.info.data
    }

    fn byte_length(&self) -> usize {
        self.info.byte_length
    }

    fn byte_offset(&self) -> usize {
        self.info.byte_offset
    }

    /// Typed arrays: the number of elements.
    fn length(&self) -> usize {
        self.byte_length() / element_size(self.scalar_type())
    }

    fn detached(&self) -> bool {
        self.info.detached != 0
    }

    fn shared(&self) -> bool {
        self.info.shared != 0
    }

    fn resizable(&self) -> bool {
        self.info.resizable != 0
    }
}

unsafe fn array_buffer(obj: *mut root::JSObject) -> Option<Buf> {
    Buf::of(obj).filter(Buf::is_array_buffer)
}

unsafe fn shared_array_buffer(obj: *mut root::JSObject) -> Option<Buf> {
    Buf::of(obj).filter(Buf::is_shared_array_buffer)
}

unsafe fn any_buffer(obj: *mut root::JSObject) -> Option<Buf> {
    Buf::of(obj).filter(Buf::is_buffer)
}

unsafe fn view(obj: *mut root::JSObject) -> Option<Buf> {
    Buf::of(obj).filter(Buf::is_view)
}

unsafe fn typed_array(obj: *mut root::JSObject) -> Option<Buf> {
    Buf::of(obj).filter(Buf::is_typed_array)
}

unsafe fn typed_array_of(obj: *mut root::JSObject, ty: Type) -> Option<Buf> {
    typed_array(obj).filter(|b| b.scalar_type() == ty)
}

fn element_size(ty: Type) -> usize {
    match ty {
        Type::Int8 | Type::Uint8 | Type::Uint8Clamped => 1,
        Type::Int16 | Type::Uint16 | Type::Float16 => 2,
        Type::Int32 | Type::Uint32 | Type::Float32 => 4,
        Type::Float64 | Type::BigInt64 | Type::BigUint64 | Type::Int64 => 8,
        Type::Simd128 => 16,
        Type::MaxTypedArrayViewType => 1,
    }
}

fn qjs_type(ty: Type) -> q::JSTypedArrayEnum {
    match ty {
        Type::Int8 => q::JS_TYPED_ARRAY_INT8,
        Type::Uint8 => q::JS_TYPED_ARRAY_UINT8,
        Type::Uint8Clamped => q::JS_TYPED_ARRAY_UINT8C,
        Type::Int16 => q::JS_TYPED_ARRAY_INT16,
        Type::Uint16 => q::JS_TYPED_ARRAY_UINT16,
        Type::Int32 => q::JS_TYPED_ARRAY_INT32,
        Type::Uint32 => q::JS_TYPED_ARRAY_UINT32,
        Type::Float16 => q::JS_TYPED_ARRAY_FLOAT16,
        Type::Float32 => q::JS_TYPED_ARRAY_FLOAT32,
        Type::Float64 => q::JS_TYPED_ARRAY_FLOAT64,
        Type::BigInt64 => q::JS_TYPED_ARRAY_BIG_INT64,
        Type::BigUint64 => q::JS_TYPED_ARRAY_BIG_UINT64,
        Type::MaxTypedArrayViewType | Type::Int64 | Type::Simd128 => unreachable!("not a typed array type"),
    }
}

unsafe fn set<T>(p: *mut T, v: T) {
    if !p.is_null() {
        *p = v;
    }
}

/// A non-negative integer as a JS number (out-of-range sizes make QuickJS throw a RangeError).
unsafe fn size_val(ctx: *mut q::JSContext, n: usize) -> q::JSValue {
    q::JS_NewInt64(ctx, i64::try_from(n).unwrap_or(i64::MAX))
}

unsafe fn throw_type_error(cx: &Cx, msg: &std::ffi::CStr) {
    q::JS_ThrowTypeError(cx.qctx(), c"%s".as_ptr(), msg.as_ptr());
}

unsafe fn throw_range_error(cx: &Cx, msg: &std::ffi::CStr) {
    q::JS_ThrowRangeError(cx.qctx(), c"%s".as_ptr(), msg.as_ptr());
}

/// Frees buffer contents allocated by QuickJS (`js_malloc_rt`).
unsafe extern "C" fn free_quickjs(rt: *mut q::JSRuntime, opaque: *mut c_void, ptr: *mut c_void) {
    let _ = opaque;
    q::js_free_rt(rt, ptr);
}

/// The free function and user data of an external ArrayBuffer (`NewExternalArrayBuffer`).
struct ExternalContents {
    free: root::JS::BufferContentsFreeFunc,
    user_data: *mut c_void,
}

unsafe extern "C" fn free_external(rt: *mut q::JSRuntime, opaque: *mut c_void, ptr: *mut c_void) {
    let _ = rt;
    // QuickJS calls this once with the contents (on detach, transfer or finalization) and again
    // with null when a detached buffer is finalized.
    if ptr.is_null() {
        return;
    }
    let ext = Box::from_raw(opaque as *mut ExternalContents);
    if let Some(free) = ext.free {
        free(ptr, ext.user_data);
    }
}

// ArrayBuffers

pub(crate) unsafe fn NewArrayBuffer(cx: *mut root::JSContext, nbytes: usize) -> *mut root::JSObject {
    let cx = Cx::from_raw(cx);
    let v = q::JS_NewArrayBufferCopy(cx.qctx(), ptr::null(), nbytes);
    adopt_object(cx, v)
}

pub(crate) unsafe fn NewArrayBufferWithContents(cx: *mut root::JSContext, nbytes: usize, contents: *mut ::std::os::raw::c_void) -> *mut root::JSObject {
    if contents.is_null() {
        return NewArrayBuffer(cx, 0);
    }
    let cx = Cx::from_raw(cx);
    let v = q::JS_NewArrayBufferOwned(cx.qctx(), contents as *mut u8, nbytes);
    if v.tag() == q::JS_TAG_EXCEPTION {
        // Like SpiderMonkey's glue (a UniquePtr), free the contents on failure.
        q::js_free_rt(cx.rt, contents);
    }
    adopt_object(cx, v)
}

pub(crate) unsafe fn NewArrayBufferWithUserOwnedContents(cx: *mut root::JSContext, nbytes: usize, contents: *mut ::std::os::raw::c_void) -> *mut root::JSObject {
    let cx = Cx::from_raw(cx);
    let v = q::JS_NewArrayBuffer(cx.qctx(), contents as *mut u8, nbytes, None, ptr::null_mut(), 0);
    adopt_object(cx, v)
}

pub(crate) unsafe fn NewExternalArrayBuffer(cx: *mut root::JSContext, nbytes: usize, contents: *mut ::std::os::raw::c_void, freeFunc: root::JS::BufferContentsFreeFunc, freeUserData: *mut ::std::os::raw::c_void) -> *mut root::JSObject {
    let cx = Cx::from_raw(cx);
    // Null contents are never freed (SpiderMonkey's UniquePtr deleter skips null).
    let (free_func, opaque): (q::JSFreeArrayBufferDataFunc, *mut c_void) = if contents.is_null() {
        (None, ptr::null_mut())
    } else {
        let ext = Box::new(ExternalContents { free: freeFunc, user_data: freeUserData });
        (Some(free_external), Box::into_raw(ext) as *mut c_void)
    };
    let v = q::JS_NewArrayBuffer(cx.qctx(), contents as *mut u8, nbytes, free_func, opaque, 0);
    if v.tag() == q::JS_TAG_EXCEPTION && !contents.is_null() {
        // SpiderMonkey's glue passes the contents as a UniquePtr, which frees them on failure.
        free_external(cx.rt, opaque, contents);
    }
    adopt_object(cx, v)
}

pub(crate) unsafe fn IsArrayBufferObject(obj: *mut root::JSObject) -> bool {
    array_buffer(obj).is_some()
}

pub(crate) unsafe fn IsArrayBufferObjectMaybeShared(obj: *mut root::JSObject) -> bool {
    any_buffer(obj).is_some()
}

pub(crate) unsafe fn IsSharedArrayBufferObject(obj: *mut root::JSObject) -> bool {
    shared_array_buffer(obj).is_some()
}

pub(crate) unsafe fn IsDetachedArrayBufferObject(obj: *mut root::JSObject) -> bool {
    array_buffer(obj).is_some_and(|b| b.detached())
}

pub(crate) unsafe fn ArrayBufferHasData(obj: *mut root::JSObject) -> bool {
    array_buffer(obj).is_some_and(|b| !b.detached())
}

pub(crate) unsafe fn IsMappedArrayBufferObject(obj: *mut root::JSObject) -> bool {
    let _ = obj;
    false
}

pub(crate) unsafe fn IsLargeArrayBufferMaybeShared(obj: *mut root::JSObject) -> bool {
    // QuickJS buffers are limited to INT32_MAX bytes, SpiderMonkey's small-buffer limit.
    let _ = obj;
    false
}

pub(crate) unsafe fn IsResizableArrayBufferMaybeShared(obj: *mut root::JSObject) -> bool {
    any_buffer(obj).is_some_and(|b| b.resizable())
}

pub(crate) unsafe fn UnwrapArrayBuffer(obj: *mut root::JSObject) -> *mut root::JSObject {
    if array_buffer(obj).is_some() { obj } else { ptr::null_mut() }
}

pub(crate) unsafe fn UnwrapSharedArrayBuffer(obj: *mut root::JSObject) -> *mut root::JSObject {
    if shared_array_buffer(obj).is_some() { obj } else { ptr::null_mut() }
}

pub(crate) unsafe fn UnwrapArrayBufferMaybeShared(obj: *mut root::JSObject) -> *mut root::JSObject {
    if any_buffer(obj).is_some() { obj } else { ptr::null_mut() }
}

pub(crate) unsafe fn GetArrayBufferByteLength(obj: *mut root::JSObject) -> usize {
    array_buffer(obj).map_or(0, |b| b.byte_length())
}

pub(crate) unsafe fn GetSharedArrayBufferByteLength(obj: *mut root::JSObject) -> usize {
    shared_array_buffer(obj).map_or(0, |b| b.byte_length())
}

pub(crate) unsafe fn GetArrayBufferData(obj: *mut root::JSObject, isSharedMemory: *mut bool, arg1: *const root::JS::AutoRequireNoGC) -> *mut u8 {
    let _ = arg1;
    let Some(b) = array_buffer(obj) else {
        return ptr::null_mut();
    };
    set(isSharedMemory, false);
    b.data()
}

pub(crate) unsafe fn GetSharedArrayBufferData(obj: *mut root::JSObject, isSharedMemory: *mut bool, arg1: *const root::JS::AutoRequireNoGC) -> *mut u8 {
    let _ = arg1;
    let Some(b) = shared_array_buffer(obj) else {
        return ptr::null_mut();
    };
    set(isSharedMemory, true);
    b.data()
}

pub(crate) unsafe fn GetArrayBufferMaybeSharedData(obj: *mut root::JSObject, isSharedMemory: *mut bool, arg1: *const root::JS::AutoRequireNoGC) -> *mut u8 {
    let _ = arg1;
    let Some(b) = any_buffer(obj) else {
        return ptr::null_mut();
    };
    set(isSharedMemory, b.shared());
    b.data()
}

/// Writes the length (in elements) and data of `buf`, or zeros if there is none.
unsafe fn length_and_data<T>(buf: Option<Buf>, length: *mut usize, isSharedMemory: *mut bool, data: *mut *mut T) {
    match buf {
        Some(b) => {
            set(length, b.byte_length() / size_of::<T>());
            set(isSharedMemory, b.shared());
            set(data, b.data() as *mut T);
        },
        None => {
            set(length, 0);
            set(isSharedMemory, false);
            set(data, ptr::null_mut());
        },
    }
}

pub(crate) unsafe fn GetArrayBufferLengthAndData(obj: *mut root::JSObject, length: *mut usize, isSharedMemory: *mut bool, data: *mut *mut u8) {
    length_and_data(array_buffer(obj), length, isSharedMemory, data)
}

pub(crate) unsafe fn GetSharedArrayBufferLengthAndData(obj: *mut root::JSObject, length: *mut usize, isSharedMemory: *mut bool, data: *mut *mut u8) {
    length_and_data(shared_array_buffer(obj), length, isSharedMemory, data)
}

pub(crate) unsafe fn GetArrayBufferMaybeSharedLengthAndData(obj: *mut root::JSObject, length: *mut usize, isSharedMemory: *mut bool, data: *mut *mut u8) {
    length_and_data(any_buffer(obj), length, isSharedMemory, data)
}

pub(crate) unsafe fn GetObjectAsArrayBuffer(obj: *mut root::JSObject, length: *mut usize, data: *mut *mut u8) -> *mut root::JSObject {
    let Some(b) = array_buffer(obj) else {
        return ptr::null_mut();
    };
    length_and_data(Some(b), length, ptr::null_mut(), data);
    obj
}

/// The ArrayBuffer `obj`, or a pending TypeError.
unsafe fn array_buffer_or_throw(cx: &Cx, obj: *mut root::JSObject) -> Option<Buf> {
    let b = array_buffer(obj);
    if b.is_none() {
        throw_type_error(cx, c"argument is not an ArrayBuffer");
    }
    b
}

pub(crate) unsafe fn DetachArrayBuffer(cx: *mut root::JSContext, obj: root::JS::Handle<*mut root::JSObject>) -> bool {
    let cx = Cx::from_raw(cx);
    if array_buffer_or_throw(cx, *obj.ptr).is_none() {
        return false;
    }
    q::JS_DetachArrayBuffer(cx.qctx(), obj_val(*obj.ptr));
    true
}

pub(crate) unsafe fn HasDefinedArrayBufferDetachKey(cx: *mut root::JSContext, obj: root::JS::Handle<*mut root::JSObject>, isDefined: *mut bool) -> bool {
    // Detach keys only exist for WebAssembly memories.
    let cx = Cx::from_raw(cx);
    if array_buffer_or_throw(cx, *obj.ptr).is_none() {
        return false;
    }
    *isDefined = false;
    true
}

pub(crate) unsafe fn StealArrayBufferContents(cx: *mut root::JSContext, obj: root::JS::Handle<*mut root::JSObject>) -> *mut ::std::os::raw::c_void {
    let cx = Cx::from_raw(cx);
    let mut len = 0;
    q::JS_StealArrayBuffer(cx.qctx(), &mut len, obj_val(*obj.ptr)) as *mut c_void
}

pub(crate) unsafe fn CopyArrayBuffer(cx: *mut root::JSContext, maybeArrayBuffer: root::JS::Handle<*mut root::JSObject>) -> *mut root::JSObject {
    let cx = Cx::from_raw(cx);
    let Some(b) = array_buffer_or_throw(cx, *maybeArrayBuffer.ptr) else {
        return ptr::null_mut();
    };
    if b.detached() {
        throw_type_error(cx, c"ArrayBuffer is detached");
        return ptr::null_mut();
    }
    let v = q::JS_NewArrayBufferCopy(cx.qctx(), b.data(), b.byte_length());
    adopt_object(cx, v)
}

pub(crate) unsafe fn ArrayBufferClone(cx: *mut root::JSContext, srcBuffer: root::JS::Handle<*mut root::JSObject>, srcByteOffset: usize, srcLength: usize) -> *mut root::JSObject {
    let cx = Cx::from_raw(cx);
    let Some(b) = any_buffer(*srcBuffer.ptr) else {
        throw_type_error(cx, c"argument is not an ArrayBuffer");
        return ptr::null_mut();
    };
    if b.detached() {
        throw_type_error(cx, c"ArrayBuffer is detached");
        return ptr::null_mut();
    }
    if srcByteOffset.checked_add(srcLength).is_none_or(|end| end > b.byte_length()) {
        throw_range_error(cx, c"invalid ArrayBuffer range");
        return ptr::null_mut();
    }
    let v = q::JS_NewArrayBufferCopy(cx.qctx(), b.data().add(srcByteOffset), srcLength);
    adopt_object(cx, v)
}

pub(crate) unsafe fn ArrayBufferCopyData(cx: *mut root::JSContext, toBlock: root::JS::Handle<*mut root::JSObject>, toIndex: usize, fromBlock: root::JS::Handle<*mut root::JSObject>, fromIndex: usize, count: usize) -> bool {
    let cx = Cx::from_raw(cx);
    let (Some(to), Some(from)) = (any_buffer(*toBlock.ptr), any_buffer(*fromBlock.ptr)) else {
        throw_type_error(cx, c"argument is not an ArrayBuffer");
        return false;
    };
    let in_range = |index: usize, b: &Buf| index.checked_add(count).is_some_and(|end| end <= b.byte_length());
    if !in_range(toIndex, &to) || !in_range(fromIndex, &from) {
        throw_range_error(cx, c"invalid ArrayBuffer range");
        return false;
    }
    if count > 0 {
        ptr::copy(from.data().add(fromIndex), to.data().add(toIndex), count);
    }
    true
}

pub(crate) unsafe fn EnsureNonInlineArrayBufferOrView(cx: *mut root::JSContext, obj: *mut root::JSObject) -> bool {
    // QuickJS buffer contents are never inline and never move.
    let cx = Cx::from_raw(cx);
    if Buf::of(obj).is_none() {
        throw_type_error(cx, c"argument is not an ArrayBuffer or view");
        return false;
    }
    true
}

pub(crate) unsafe fn NewSharedArrayBuffer(cx: *mut root::JSContext, nbytes: usize) -> *mut root::JSObject {
    let cx = Cx::from_raw(cx);
    let data = q::js_mallocz(cx.qctx(), nbytes.max(1)) as *mut u8;
    if data.is_null() {
        return ptr::null_mut();
    }
    let v = q::JS_NewArrayBuffer(cx.qctx(), data, nbytes, Some(free_quickjs), ptr::null_mut(), 1);
    if v.tag() == q::JS_TAG_EXCEPTION {
        q::js_free_rt(cx.rt, data as *mut c_void);
    }
    adopt_object(cx, v)
}

pub(crate) unsafe fn ContainsSharedArrayBuffer(cx: *mut root::JSContext) -> bool {
    // SharedArrayBuffer is disabled in Servo, and QuickJS does not track live ones.
    let _ = cx;
    false
}

// Views (typed arrays and DataViews)

pub(crate) unsafe fn JS_IsArrayBufferViewObject(obj: *mut root::JSObject) -> bool {
    view(obj).is_some()
}

pub(crate) unsafe fn JS_IsTypedArrayObject(obj: *mut root::JSObject) -> bool {
    typed_array(obj).is_some()
}

pub(crate) unsafe fn UnwrapArrayBufferView(obj: *mut root::JSObject) -> *mut root::JSObject {
    if view(obj).is_some() { obj } else { ptr::null_mut() }
}

pub(crate) unsafe fn JS_GetArrayBufferViewType(obj: *mut root::JSObject) -> root::JS::Scalar::Type {
    view(obj).map_or(Type::MaxTypedArrayViewType, |b| b.scalar_type())
}

pub(crate) unsafe fn JS_GetArrayBufferViewBuffer(cx: *mut root::JSContext, obj: root::JS::Handle<*mut root::JSObject>, isSharedMemory: *mut bool) -> *mut root::JSObject {
    let cx = Cx::from_raw(cx);
    let Some(b) = view(*obj.ptr) else {
        throw_type_error(cx, c"argument is not an ArrayBufferView");
        return ptr::null_mut();
    };
    set(isSharedMemory, b.shared());
    cx.retain(crate::sys::jsval::JSVal::from_qjs(b.info.buffer));
    b.buffer()
}

pub(crate) unsafe fn JS_GetArrayBufferViewByteLength(obj: *mut root::JSObject) -> usize {
    view(obj).map_or(0, |b| b.byte_length())
}

pub(crate) unsafe fn JS_GetArrayBufferViewByteOffset(obj: *mut root::JSObject) -> usize {
    view(obj).map_or(0, |b| b.byte_offset())
}

pub(crate) unsafe fn JS_GetArrayBufferViewData(obj: *mut root::JSObject, isSharedMemory: *mut bool, arg1: *const root::JS::AutoRequireNoGC) -> *mut ::std::os::raw::c_void {
    let _ = arg1;
    let Some(b) = view(obj) else {
        return ptr::null_mut();
    };
    set(isSharedMemory, b.shared());
    b.data() as *mut c_void
}

pub(crate) unsafe fn JS_GetArrayBufferViewFixedData(obj: *mut root::JSObject, buffer: *mut u8, bufSize: usize) -> *mut u8 {
    // Data is never inline, so `buffer` is not needed.
    let _ = (buffer, bufSize);
    match view(obj) {
        Some(b) if !b.shared() => b.data(),
        _ => ptr::null_mut(),
    }
}

pub(crate) unsafe fn GetArrayBufferViewLengthAndData(obj: *mut root::JSObject, length: *mut usize, isSharedMemory: *mut bool, data: *mut *mut u8) {
    length_and_data(view(obj), length, isSharedMemory, data)
}

pub(crate) unsafe fn JS_GetObjectAsArrayBufferView(obj: *mut root::JSObject, length: *mut usize, isSharedMemory: *mut bool, data: *mut *mut u8) -> *mut root::JSObject {
    let Some(b) = view(obj) else {
        return ptr::null_mut();
    };
    length_and_data(Some(b), length, isSharedMemory, data);
    obj
}

pub(crate) unsafe fn IsArrayBufferViewShared(obj: *mut root::JSObject) -> bool {
    view(obj).is_some_and(|b| b.shared())
}

pub(crate) unsafe fn IsLargeArrayBufferView(obj: *mut root::JSObject) -> bool {
    let _ = obj;
    false
}

pub(crate) unsafe fn IsResizableArrayBufferView(obj: *mut root::JSObject) -> bool {
    view(obj).is_some_and(|b| b.resizable())
}

pub(crate) unsafe fn JS_MaxMovableTypedArraySize() -> usize {
    // No typed array data moves.
    0
}

pub(crate) unsafe fn JS_GetTypedArrayLength(obj: *mut root::JSObject) -> usize {
    typed_array(obj).map_or(0, |b| b.length())
}

pub(crate) unsafe fn JS_GetTypedArrayByteLength(obj: *mut root::JSObject) -> usize {
    typed_array(obj).map_or(0, |b| b.byte_length())
}

pub(crate) unsafe fn JS_GetTypedArrayByteOffset(obj: *mut root::JSObject) -> usize {
    typed_array(obj).map_or(0, |b| b.byte_offset())
}

pub(crate) unsafe fn JS_GetTypedArraySharedness(obj: *mut root::JSObject) -> bool {
    typed_array(obj).is_some_and(|b| b.shared())
}

pub(crate) unsafe fn JS_NewDataView(cx: *mut root::JSContext, buffer: root::JS::Handle<*mut root::JSObject>, byteOffset: usize, byteLength: usize) -> *mut root::JSObject {
    let cx = Cx::from_raw(cx);
    let v = q::JS_NewDataView(cx.qctx(), obj_val(*buffer.ptr), byteOffset, byteLength);
    adopt_object(cx, v)
}

// Typed arrays of one element type

unsafe fn new_typed_array(cx: *mut root::JSContext, ty: Type, args: &mut [q::JSValue]) -> *mut root::JSObject {
    let cx = Cx::from_raw(cx);
    let v = q::JS_NewTypedArray(cx.qctx(), args.len() as c_int, args.as_mut_ptr(), qjs_type(ty));
    adopt_object(cx, v)
}

unsafe fn new_with_length(cx: *mut root::JSContext, ty: Type, nelements: usize) -> *mut root::JSObject {
    let mut args = [size_val(Cx::from_raw(cx).qctx(), nelements)];
    new_typed_array(cx, ty, &mut args)
}

unsafe fn new_from_array(cx: *mut root::JSContext, ty: Type, array: root::JS::Handle<*mut root::JSObject>) -> *mut root::JSObject {
    let other = *array.ptr;
    // QuickJS would create a view; SpiderMonkey treats a buffer as an array-like without length.
    if any_buffer(other).is_some() {
        return new_with_length(cx, ty, 0);
    }
    let mut args = [obj_val(other)];
    new_typed_array(cx, ty, &mut args)
}

unsafe fn new_with_buffer(cx: *mut root::JSContext, ty: Type, buffer: root::JS::Handle<*mut root::JSObject>, byte_offset: usize, length: i64) -> *mut root::JSObject {
    let qctx = Cx::from_raw(cx).qctx();
    let buffer = *buffer.ptr;
    if any_buffer(buffer).is_none() {
        throw_type_error(Cx::from_raw(cx), c"argument is not an ArrayBuffer");
        return ptr::null_mut();
    }
    // A length of -1 means up to the end of the buffer.
    let length = if length < 0 { q::JS_UNDEFINED } else { q::JS_NewInt64(qctx, length) };
    let mut args = [obj_val(buffer), size_val(qctx, byte_offset), length];
    new_typed_array(cx, ty, &mut args)
}

unsafe fn unwrap_typed(obj: *mut root::JSObject, ty: Type) -> *mut root::JSObject {
    if typed_array_of(obj, ty).is_some() { obj } else { ptr::null_mut() }
}

unsafe fn typed_data<T>(obj: *mut root::JSObject, ty: Type, isSharedMemory: *mut bool) -> *mut T {
    let Some(b) = typed_array_of(obj, ty) else {
        return ptr::null_mut();
    };
    set(isSharedMemory, b.shared());
    b.data() as *mut T
}

unsafe fn typed_object_as<T>(obj: *mut root::JSObject, ty: Type, length: *mut usize, isSharedMemory: *mut bool, data: *mut *mut T) -> *mut root::JSObject {
    let Some(b) = typed_array_of(obj, ty) else {
        return ptr::null_mut();
    };
    length_and_data(Some(b), length, isSharedMemory, data);
    obj
}

unsafe fn typed_length_and_data<T>(obj: *mut root::JSObject, ty: Type, length: *mut usize, isSharedMemory: *mut bool, data: *mut *mut T) {
    length_and_data(typed_array_of(obj, ty), length, isSharedMemory, data)
}

pub(crate) unsafe fn JS_NewInt8Array(cx: *mut root::JSContext, nelements: usize) -> *mut root::JSObject {
    new_with_length(cx, Type::Int8, nelements)
}
pub(crate) unsafe fn JS_NewInt8ArrayFromArray(cx: *mut root::JSContext, array: root::JS::Handle<*mut root::JSObject>) -> *mut root::JSObject {
    new_from_array(cx, Type::Int8, array)
}
pub(crate) unsafe fn JS_NewInt8ArrayWithBuffer(cx: *mut root::JSContext, arrayBuffer: root::JS::Handle<*mut root::JSObject>, byteOffset: usize, length: i64) -> *mut root::JSObject {
    new_with_buffer(cx, Type::Int8, arrayBuffer, byteOffset, length)
}
pub(crate) unsafe fn UnwrapInt8Array(maybeWrapped: *mut root::JSObject) -> *mut root::JSObject {
    unwrap_typed(maybeWrapped, Type::Int8)
}
pub(crate) unsafe fn JS_GetInt8ArrayData(maybeWrapped: *mut root::JSObject, isSharedMemory: *mut bool, arg1: *const root::JS::AutoRequireNoGC) -> *mut i8 {
    let _ = arg1;
    typed_data(maybeWrapped, Type::Int8, isSharedMemory)
}
pub(crate) unsafe fn JS_GetObjectAsInt8Array(maybeWrapped: *mut root::JSObject, length: *mut usize, isSharedMemory: *mut bool, data: *mut *mut i8) -> *mut root::JSObject {
    typed_object_as(maybeWrapped, Type::Int8, length, isSharedMemory, data)
}
pub(crate) unsafe fn GetInt8ArrayLengthAndData(obj: *mut root::JSObject, length: *mut usize, isSharedMemory: *mut bool, data: *mut *mut i8) {
    typed_length_and_data(obj, Type::Int8, length, isSharedMemory, data)
}

pub(crate) unsafe fn JS_NewUint8Array(cx: *mut root::JSContext, nelements: usize) -> *mut root::JSObject {
    new_with_length(cx, Type::Uint8, nelements)
}
pub(crate) unsafe fn JS_NewUint8ArrayFromArray(cx: *mut root::JSContext, array: root::JS::Handle<*mut root::JSObject>) -> *mut root::JSObject {
    new_from_array(cx, Type::Uint8, array)
}
pub(crate) unsafe fn JS_NewUint8ArrayWithBuffer(cx: *mut root::JSContext, arrayBuffer: root::JS::Handle<*mut root::JSObject>, byteOffset: usize, length: i64) -> *mut root::JSObject {
    new_with_buffer(cx, Type::Uint8, arrayBuffer, byteOffset, length)
}
pub(crate) unsafe fn UnwrapUint8Array(maybeWrapped: *mut root::JSObject) -> *mut root::JSObject {
    unwrap_typed(maybeWrapped, Type::Uint8)
}
pub(crate) unsafe fn JS_GetUint8ArrayData(maybeWrapped: *mut root::JSObject, isSharedMemory: *mut bool, arg1: *const root::JS::AutoRequireNoGC) -> *mut u8 {
    let _ = arg1;
    typed_data(maybeWrapped, Type::Uint8, isSharedMemory)
}
pub(crate) unsafe fn JS_GetObjectAsUint8Array(maybeWrapped: *mut root::JSObject, length: *mut usize, isSharedMemory: *mut bool, data: *mut *mut u8) -> *mut root::JSObject {
    typed_object_as(maybeWrapped, Type::Uint8, length, isSharedMemory, data)
}
pub(crate) unsafe fn GetUint8ArrayLengthAndData(obj: *mut root::JSObject, length: *mut usize, isSharedMemory: *mut bool, data: *mut *mut u8) {
    typed_length_and_data(obj, Type::Uint8, length, isSharedMemory, data)
}

pub(crate) unsafe fn JS_NewUint8ClampedArray(cx: *mut root::JSContext, nelements: usize) -> *mut root::JSObject {
    new_with_length(cx, Type::Uint8Clamped, nelements)
}
pub(crate) unsafe fn JS_NewUint8ClampedArrayFromArray(cx: *mut root::JSContext, array: root::JS::Handle<*mut root::JSObject>) -> *mut root::JSObject {
    new_from_array(cx, Type::Uint8Clamped, array)
}
pub(crate) unsafe fn JS_NewUint8ClampedArrayWithBuffer(cx: *mut root::JSContext, arrayBuffer: root::JS::Handle<*mut root::JSObject>, byteOffset: usize, length: i64) -> *mut root::JSObject {
    new_with_buffer(cx, Type::Uint8Clamped, arrayBuffer, byteOffset, length)
}
pub(crate) unsafe fn UnwrapUint8ClampedArray(maybeWrapped: *mut root::JSObject) -> *mut root::JSObject {
    unwrap_typed(maybeWrapped, Type::Uint8Clamped)
}
pub(crate) unsafe fn JS_GetUint8ClampedArrayData(maybeWrapped: *mut root::JSObject, isSharedMemory: *mut bool, arg1: *const root::JS::AutoRequireNoGC) -> *mut u8 {
    let _ = arg1;
    typed_data(maybeWrapped, Type::Uint8Clamped, isSharedMemory)
}
pub(crate) unsafe fn JS_GetObjectAsUint8ClampedArray(maybeWrapped: *mut root::JSObject, length: *mut usize, isSharedMemory: *mut bool, data: *mut *mut u8) -> *mut root::JSObject {
    typed_object_as(maybeWrapped, Type::Uint8Clamped, length, isSharedMemory, data)
}
pub(crate) unsafe fn GetUint8ClampedArrayLengthAndData(obj: *mut root::JSObject, length: *mut usize, isSharedMemory: *mut bool, data: *mut *mut u8) {
    typed_length_and_data(obj, Type::Uint8Clamped, length, isSharedMemory, data)
}

pub(crate) unsafe fn JS_NewInt16Array(cx: *mut root::JSContext, nelements: usize) -> *mut root::JSObject {
    new_with_length(cx, Type::Int16, nelements)
}
pub(crate) unsafe fn JS_NewInt16ArrayFromArray(cx: *mut root::JSContext, array: root::JS::Handle<*mut root::JSObject>) -> *mut root::JSObject {
    new_from_array(cx, Type::Int16, array)
}
pub(crate) unsafe fn JS_NewInt16ArrayWithBuffer(cx: *mut root::JSContext, arrayBuffer: root::JS::Handle<*mut root::JSObject>, byteOffset: usize, length: i64) -> *mut root::JSObject {
    new_with_buffer(cx, Type::Int16, arrayBuffer, byteOffset, length)
}
pub(crate) unsafe fn UnwrapInt16Array(maybeWrapped: *mut root::JSObject) -> *mut root::JSObject {
    unwrap_typed(maybeWrapped, Type::Int16)
}
pub(crate) unsafe fn JS_GetInt16ArrayData(maybeWrapped: *mut root::JSObject, isSharedMemory: *mut bool, arg1: *const root::JS::AutoRequireNoGC) -> *mut i16 {
    let _ = arg1;
    typed_data(maybeWrapped, Type::Int16, isSharedMemory)
}
pub(crate) unsafe fn JS_GetObjectAsInt16Array(maybeWrapped: *mut root::JSObject, length: *mut usize, isSharedMemory: *mut bool, data: *mut *mut i16) -> *mut root::JSObject {
    typed_object_as(maybeWrapped, Type::Int16, length, isSharedMemory, data)
}
pub(crate) unsafe fn GetInt16ArrayLengthAndData(obj: *mut root::JSObject, length: *mut usize, isSharedMemory: *mut bool, data: *mut *mut i16) {
    typed_length_and_data(obj, Type::Int16, length, isSharedMemory, data)
}

pub(crate) unsafe fn JS_NewUint16Array(cx: *mut root::JSContext, nelements: usize) -> *mut root::JSObject {
    new_with_length(cx, Type::Uint16, nelements)
}
pub(crate) unsafe fn JS_NewUint16ArrayFromArray(cx: *mut root::JSContext, array: root::JS::Handle<*mut root::JSObject>) -> *mut root::JSObject {
    new_from_array(cx, Type::Uint16, array)
}
pub(crate) unsafe fn JS_NewUint16ArrayWithBuffer(cx: *mut root::JSContext, arrayBuffer: root::JS::Handle<*mut root::JSObject>, byteOffset: usize, length: i64) -> *mut root::JSObject {
    new_with_buffer(cx, Type::Uint16, arrayBuffer, byteOffset, length)
}
pub(crate) unsafe fn UnwrapUint16Array(maybeWrapped: *mut root::JSObject) -> *mut root::JSObject {
    unwrap_typed(maybeWrapped, Type::Uint16)
}
pub(crate) unsafe fn JS_GetUint16ArrayData(maybeWrapped: *mut root::JSObject, isSharedMemory: *mut bool, arg1: *const root::JS::AutoRequireNoGC) -> *mut u16 {
    let _ = arg1;
    typed_data(maybeWrapped, Type::Uint16, isSharedMemory)
}
pub(crate) unsafe fn JS_GetObjectAsUint16Array(maybeWrapped: *mut root::JSObject, length: *mut usize, isSharedMemory: *mut bool, data: *mut *mut u16) -> *mut root::JSObject {
    typed_object_as(maybeWrapped, Type::Uint16, length, isSharedMemory, data)
}
pub(crate) unsafe fn GetUint16ArrayLengthAndData(obj: *mut root::JSObject, length: *mut usize, isSharedMemory: *mut bool, data: *mut *mut u16) {
    typed_length_and_data(obj, Type::Uint16, length, isSharedMemory, data)
}

pub(crate) unsafe fn JS_NewInt32Array(cx: *mut root::JSContext, nelements: usize) -> *mut root::JSObject {
    new_with_length(cx, Type::Int32, nelements)
}
pub(crate) unsafe fn JS_NewInt32ArrayFromArray(cx: *mut root::JSContext, array: root::JS::Handle<*mut root::JSObject>) -> *mut root::JSObject {
    new_from_array(cx, Type::Int32, array)
}
pub(crate) unsafe fn JS_NewInt32ArrayWithBuffer(cx: *mut root::JSContext, arrayBuffer: root::JS::Handle<*mut root::JSObject>, byteOffset: usize, length: i64) -> *mut root::JSObject {
    new_with_buffer(cx, Type::Int32, arrayBuffer, byteOffset, length)
}
pub(crate) unsafe fn UnwrapInt32Array(maybeWrapped: *mut root::JSObject) -> *mut root::JSObject {
    unwrap_typed(maybeWrapped, Type::Int32)
}
pub(crate) unsafe fn JS_GetInt32ArrayData(maybeWrapped: *mut root::JSObject, isSharedMemory: *mut bool, arg1: *const root::JS::AutoRequireNoGC) -> *mut i32 {
    let _ = arg1;
    typed_data(maybeWrapped, Type::Int32, isSharedMemory)
}
pub(crate) unsafe fn JS_GetObjectAsInt32Array(maybeWrapped: *mut root::JSObject, length: *mut usize, isSharedMemory: *mut bool, data: *mut *mut i32) -> *mut root::JSObject {
    typed_object_as(maybeWrapped, Type::Int32, length, isSharedMemory, data)
}
pub(crate) unsafe fn GetInt32ArrayLengthAndData(obj: *mut root::JSObject, length: *mut usize, isSharedMemory: *mut bool, data: *mut *mut i32) {
    typed_length_and_data(obj, Type::Int32, length, isSharedMemory, data)
}

pub(crate) unsafe fn JS_NewUint32Array(cx: *mut root::JSContext, nelements: usize) -> *mut root::JSObject {
    new_with_length(cx, Type::Uint32, nelements)
}
pub(crate) unsafe fn JS_NewUint32ArrayFromArray(cx: *mut root::JSContext, array: root::JS::Handle<*mut root::JSObject>) -> *mut root::JSObject {
    new_from_array(cx, Type::Uint32, array)
}
pub(crate) unsafe fn JS_NewUint32ArrayWithBuffer(cx: *mut root::JSContext, arrayBuffer: root::JS::Handle<*mut root::JSObject>, byteOffset: usize, length: i64) -> *mut root::JSObject {
    new_with_buffer(cx, Type::Uint32, arrayBuffer, byteOffset, length)
}
pub(crate) unsafe fn UnwrapUint32Array(maybeWrapped: *mut root::JSObject) -> *mut root::JSObject {
    unwrap_typed(maybeWrapped, Type::Uint32)
}
pub(crate) unsafe fn JS_GetUint32ArrayData(maybeWrapped: *mut root::JSObject, isSharedMemory: *mut bool, arg1: *const root::JS::AutoRequireNoGC) -> *mut u32 {
    let _ = arg1;
    typed_data(maybeWrapped, Type::Uint32, isSharedMemory)
}
pub(crate) unsafe fn JS_GetObjectAsUint32Array(maybeWrapped: *mut root::JSObject, length: *mut usize, isSharedMemory: *mut bool, data: *mut *mut u32) -> *mut root::JSObject {
    typed_object_as(maybeWrapped, Type::Uint32, length, isSharedMemory, data)
}
pub(crate) unsafe fn GetUint32ArrayLengthAndData(obj: *mut root::JSObject, length: *mut usize, isSharedMemory: *mut bool, data: *mut *mut u32) {
    typed_length_and_data(obj, Type::Uint32, length, isSharedMemory, data)
}

pub(crate) unsafe fn JS_NewFloat32Array(cx: *mut root::JSContext, nelements: usize) -> *mut root::JSObject {
    new_with_length(cx, Type::Float32, nelements)
}
pub(crate) unsafe fn JS_NewFloat32ArrayFromArray(cx: *mut root::JSContext, array: root::JS::Handle<*mut root::JSObject>) -> *mut root::JSObject {
    new_from_array(cx, Type::Float32, array)
}
pub(crate) unsafe fn JS_NewFloat32ArrayWithBuffer(cx: *mut root::JSContext, arrayBuffer: root::JS::Handle<*mut root::JSObject>, byteOffset: usize, length: i64) -> *mut root::JSObject {
    new_with_buffer(cx, Type::Float32, arrayBuffer, byteOffset, length)
}
pub(crate) unsafe fn UnwrapFloat32Array(maybeWrapped: *mut root::JSObject) -> *mut root::JSObject {
    unwrap_typed(maybeWrapped, Type::Float32)
}
pub(crate) unsafe fn JS_GetFloat32ArrayData(maybeWrapped: *mut root::JSObject, isSharedMemory: *mut bool, arg1: *const root::JS::AutoRequireNoGC) -> *mut f32 {
    let _ = arg1;
    typed_data(maybeWrapped, Type::Float32, isSharedMemory)
}
pub(crate) unsafe fn JS_GetObjectAsFloat32Array(maybeWrapped: *mut root::JSObject, length: *mut usize, isSharedMemory: *mut bool, data: *mut *mut f32) -> *mut root::JSObject {
    typed_object_as(maybeWrapped, Type::Float32, length, isSharedMemory, data)
}
pub(crate) unsafe fn GetFloat32ArrayLengthAndData(obj: *mut root::JSObject, length: *mut usize, isSharedMemory: *mut bool, data: *mut *mut f32) {
    typed_length_and_data(obj, Type::Float32, length, isSharedMemory, data)
}

pub(crate) unsafe fn JS_NewFloat64Array(cx: *mut root::JSContext, nelements: usize) -> *mut root::JSObject {
    new_with_length(cx, Type::Float64, nelements)
}
pub(crate) unsafe fn JS_NewFloat64ArrayFromArray(cx: *mut root::JSContext, array: root::JS::Handle<*mut root::JSObject>) -> *mut root::JSObject {
    new_from_array(cx, Type::Float64, array)
}
pub(crate) unsafe fn JS_NewFloat64ArrayWithBuffer(cx: *mut root::JSContext, arrayBuffer: root::JS::Handle<*mut root::JSObject>, byteOffset: usize, length: i64) -> *mut root::JSObject {
    new_with_buffer(cx, Type::Float64, arrayBuffer, byteOffset, length)
}
pub(crate) unsafe fn UnwrapFloat64Array(maybeWrapped: *mut root::JSObject) -> *mut root::JSObject {
    unwrap_typed(maybeWrapped, Type::Float64)
}
pub(crate) unsafe fn JS_GetFloat64ArrayData(maybeWrapped: *mut root::JSObject, isSharedMemory: *mut bool, arg1: *const root::JS::AutoRequireNoGC) -> *mut f64 {
    let _ = arg1;
    typed_data(maybeWrapped, Type::Float64, isSharedMemory)
}
pub(crate) unsafe fn JS_GetObjectAsFloat64Array(maybeWrapped: *mut root::JSObject, length: *mut usize, isSharedMemory: *mut bool, data: *mut *mut f64) -> *mut root::JSObject {
    typed_object_as(maybeWrapped, Type::Float64, length, isSharedMemory, data)
}
pub(crate) unsafe fn GetFloat64ArrayLengthAndData(obj: *mut root::JSObject, length: *mut usize, isSharedMemory: *mut bool, data: *mut *mut f64) {
    typed_length_and_data(obj, Type::Float64, length, isSharedMemory, data)
}

pub(crate) unsafe fn JS_NewBigInt64Array(cx: *mut root::JSContext, nelements: usize) -> *mut root::JSObject {
    new_with_length(cx, Type::BigInt64, nelements)
}
pub(crate) unsafe fn JS_NewBigInt64ArrayFromArray(cx: *mut root::JSContext, array: root::JS::Handle<*mut root::JSObject>) -> *mut root::JSObject {
    new_from_array(cx, Type::BigInt64, array)
}
pub(crate) unsafe fn JS_NewBigInt64ArrayWithBuffer(cx: *mut root::JSContext, arrayBuffer: root::JS::Handle<*mut root::JSObject>, byteOffset: usize, length: i64) -> *mut root::JSObject {
    new_with_buffer(cx, Type::BigInt64, arrayBuffer, byteOffset, length)
}
pub(crate) unsafe fn UnwrapBigInt64Array(maybeWrapped: *mut root::JSObject) -> *mut root::JSObject {
    unwrap_typed(maybeWrapped, Type::BigInt64)
}
pub(crate) unsafe fn JS_GetBigInt64ArrayData(maybeWrapped: *mut root::JSObject, isSharedMemory: *mut bool, arg1: *const root::JS::AutoRequireNoGC) -> *mut i64 {
    let _ = arg1;
    typed_data(maybeWrapped, Type::BigInt64, isSharedMemory)
}
pub(crate) unsafe fn JS_GetObjectAsBigInt64Array(maybeWrapped: *mut root::JSObject, length: *mut usize, isSharedMemory: *mut bool, data: *mut *mut i64) -> *mut root::JSObject {
    typed_object_as(maybeWrapped, Type::BigInt64, length, isSharedMemory, data)
}

pub(crate) unsafe fn JS_NewBigUint64Array(cx: *mut root::JSContext, nelements: usize) -> *mut root::JSObject {
    new_with_length(cx, Type::BigUint64, nelements)
}
pub(crate) unsafe fn JS_NewBigUint64ArrayFromArray(cx: *mut root::JSContext, array: root::JS::Handle<*mut root::JSObject>) -> *mut root::JSObject {
    new_from_array(cx, Type::BigUint64, array)
}
pub(crate) unsafe fn JS_NewBigUint64ArrayWithBuffer(cx: *mut root::JSContext, arrayBuffer: root::JS::Handle<*mut root::JSObject>, byteOffset: usize, length: i64) -> *mut root::JSObject {
    new_with_buffer(cx, Type::BigUint64, arrayBuffer, byteOffset, length)
}
pub(crate) unsafe fn UnwrapBigUint64Array(maybeWrapped: *mut root::JSObject) -> *mut root::JSObject {
    unwrap_typed(maybeWrapped, Type::BigUint64)
}
pub(crate) unsafe fn JS_GetBigUint64ArrayData(maybeWrapped: *mut root::JSObject, isSharedMemory: *mut bool, arg1: *const root::JS::AutoRequireNoGC) -> *mut u64 {
    let _ = arg1;
    typed_data(maybeWrapped, Type::BigUint64, isSharedMemory)
}
pub(crate) unsafe fn JS_GetObjectAsBigUint64Array(maybeWrapped: *mut root::JSObject, length: *mut usize, isSharedMemory: *mut bool, data: *mut *mut u64) -> *mut root::JSObject {
    typed_object_as(maybeWrapped, Type::BigUint64, length, isSharedMemory, data)
}

pub(crate) unsafe fn JS_NewFloat16Array(cx: *mut root::JSContext, nelements: usize) -> *mut root::JSObject {
    new_with_length(cx, Type::Float16, nelements)
}
pub(crate) unsafe fn JS_NewFloat16ArrayFromArray(cx: *mut root::JSContext, array: root::JS::Handle<*mut root::JSObject>) -> *mut root::JSObject {
    new_from_array(cx, Type::Float16, array)
}
pub(crate) unsafe fn JS_NewFloat16ArrayWithBuffer(cx: *mut root::JSContext, arrayBuffer: root::JS::Handle<*mut root::JSObject>, byteOffset: usize, length: i64) -> *mut root::JSObject {
    new_with_buffer(cx, Type::Float16, arrayBuffer, byteOffset, length)
}
pub(crate) unsafe fn UnwrapFloat16Array(maybeWrapped: *mut root::JSObject) -> *mut root::JSObject {
    unwrap_typed(maybeWrapped, Type::Float16)
}
pub(crate) unsafe fn JS_GetFloat16ArrayData(maybeWrapped: *mut root::JSObject, isSharedMemory: *mut bool, arg1: *const root::JS::AutoRequireNoGC) -> *mut u16 {
    let _ = arg1;
    typed_data(maybeWrapped, Type::Float16, isSharedMemory)
}
pub(crate) unsafe fn JS_GetObjectAsFloat16Array(maybeWrapped: *mut root::JSObject, length: *mut usize, isSharedMemory: *mut bool, data: *mut *mut u16) -> *mut root::JSObject {
    typed_object_as(maybeWrapped, Type::Float16, length, isSharedMemory, data)
}
