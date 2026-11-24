//! Unresolvable references throw a ReferenceError even when the global's prototype chain has an
//! exotic object with a `get_property` hook, such as Servo's WindowProperties proxy.

use std::ffi::{c_int, CString};

use servo_quickjs::*;

unsafe fn eval_to_string(ctx: *mut JSContext, src: &str) -> String {
    let src = CString::new(src).unwrap();
    let flags = JS_EVAL_TYPE_GLOBAL as c_int;
    unsafe {
        let v = JS_Eval(
            ctx,
            src.as_ptr(),
            src.as_bytes().len(),
            c"<test>".as_ptr(),
            flags,
        );
        assert_ne!(v.tag(), JS_TAG_EXCEPTION);
        let s = JS_ToCString(ctx, v);
        let out = std::ffi::CStr::from_ptr(s).to_string_lossy().into_owned();
        JS_FreeCString(ctx, s);
        JS_FreeValue(ctx, v);
        out
    }
}

#[test]
fn unresolvable_reference_with_proxy_on_global_chain() {
    unsafe {
        let rt = JS_NewRuntime();
        let ctx = JS_NewContext(rt);
        let result = eval_to_string(
            ctx,
            r#"
            const proto = Object.getPrototypeOf(globalThis);
            Object.setPrototypeOf(globalThis, new Proxy(Object.create(proto), {}));
            globalThis.defined = 1;
            let r = [];
            try { undefinedName; r.push("no error"); } catch (e) { r.push(e.name); }
            r.push(typeof undefinedName, defined, typeof Object);
            r.join(",")
            "#,
        );
        assert_eq!(result, "ReferenceError,undefined,1,function");
        JS_FreeContext(ctx);
        JS_FreeRuntime(rt);
    }
}
