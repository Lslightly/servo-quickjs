use std::ffi::{CStr, CString, c_char, c_int, c_void};
use std::sync::atomic::{AtomicUsize, Ordering};

use servo_quickjs::*;

static FINALIZED: AtomicUsize = AtomicUsize::new(0);
static EXOTIC_GETS: AtomicUsize = AtomicUsize::new(0);

unsafe fn eval(ctx: *mut JSContext, src: &str) -> JSValue {
    let src = CString::new(src).unwrap();
    let name = c"<smoke>";
    let flags = JS_EVAL_TYPE_GLOBAL as c_int;
    unsafe { JS_Eval(ctx, src.as_ptr(), src.as_bytes().len(), name.as_ptr(), flags) }
}

unsafe extern "C" fn finalizer(_rt: *mut JSRuntime, val: JSValue) {
    let _ = val;
    FINALIZED.fetch_add(1, Ordering::SeqCst);
}

unsafe extern "C" fn exotic_get_own_property(
    ctx: *mut JSContext,
    desc: *mut JSPropertyDescriptor,
    _obj: JSValue,
    prop: JSAtom,
) -> c_int {
    let name = unsafe { JS_AtomToCString(ctx, prop) };
    let is_named = unsafe { CStr::from_ptr(name) } == c"named";
    unsafe { JS_FreeCString(ctx, name as *const c_char) };
    if !is_named {
        return 0;
    }
    EXOTIC_GETS.fetch_add(1, Ordering::SeqCst);
    if !desc.is_null() {
        unsafe {
            (*desc).flags = (JS_PROP_ENUMERABLE | JS_PROP_CONFIGURABLE) as c_int;
            (*desc).value = JS_NewInt32(ctx, 42);
            (*desc).getter = JS_UNDEFINED;
            (*desc).setter = JS_UNDEFINED;
        }
    }
    1
}

#[test]
fn eval_class_exotic_gc() {
    unsafe {
        let rt = JS_NewRuntime();
        let ctx = JS_NewContext(rt);
        assert!(!ctx.is_null());

        let v = eval(ctx, "[1, 2, 3].map(x => x * 2).reduce((a, b) => a + b, 0)");
        assert_eq!(v.tag(), JS_TAG_INT);
        assert_eq!(v.int(), 12);

        let mut class_id: JSClassID = 0;
        JS_NewClassID(&mut class_id);
        let exotic = Box::leak(Box::new(JSClassExoticMethods {
            get_own_property: Some(exotic_get_own_property),
            ..Default::default()
        }));
        let def = JSClassDef {
            class_name: c"SmokeNode".as_ptr(),
            finalizer: Some(finalizer),
            exotic,
            ..Default::default()
        };
        assert_eq!(JS_NewClass(rt, class_id, &def), 0);

        let obj = JS_NewObjectClass(ctx, class_id as c_int);
        JS_SetOpaque(obj, 0x1234 as *mut c_void);
        let global = JS_GetGlobalObject(ctx);
        JS_SetPropertyStr(ctx, global, c"node".as_ptr(), obj);
        let v = eval(ctx, "node.named + ('named' in node ? 1 : 0)");
        assert_eq!(v.int(), 43, "exotic hook result");
        assert!(EXOTIC_GETS.load(Ordering::SeqCst) >= 1);

        // A cycle through a plain object; only the cycle collector can free the node.
        let v = eval(ctx, "let o = {}; o.n = node; node.o = o; node = undefined; o = undefined; 0");
        JS_FreeValue(ctx, v);
        JS_FreeValue(ctx, global);
        JS_RunGC(rt);
        assert_eq!(FINALIZED.load(Ordering::SeqCst), 1, "cycle collected");

        JS_FreeContext(ctx);
        JS_FreeRuntime(rt);
    }
}
