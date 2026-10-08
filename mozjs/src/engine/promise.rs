/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at http://mozilla.org/MPL/2.0/. */

//! Promises, the job (microtask) queue, interrupts and security callbacks.
//!
//! With a `JS::JobQueue` installed, QuickJS hands every promise job to the embedder (patch Q4):
//! the job becomes a native function (a "job thunk") passed to the queue's `enqueuePromiseJob`
//! trap, which runs it at its microtask checkpoint. Without one, jobs stay in QuickJS's internal
//! list and `RunJobs` drains it.

use std::ffi::{c_int, c_void};
use std::ptr;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use servo_quickjs as q;

use super::class::{function_data, new_function};
use super::{obj_val, val_obj, Cx, RealmData};
use crate::sys::jsapi as root;
use crate::sys::jsval::{JSVal, UndefinedValue};

/// Per-runtime state of this module (a field of `Cx`).
pub(crate) struct RuntimeHooks {
    job_queue: *mut JobQueue,
    rejection_tracker: root::JS::PromiseRejectionTrackerCallback,
    rejection_tracker_data: *mut c_void,
    interrupt_callbacks: Vec<root::JSInterruptCallback>,
    /// Set by `JS_RequestInterruptCallback`, possibly from another thread.
    interrupt_requested: Arc<AtomicBool>,
    security_callbacks: *const root::JSSecurityCallbacks,
}

impl Default for RuntimeHooks {
    fn default() -> Self {
        RuntimeHooks {
            job_queue: ptr::null_mut(),
            rejection_tracker: None,
            rejection_tracker_data: ptr::null_mut(),
            interrupt_callbacks: Vec::new(),
            interrupt_requested: Default::default(),
            security_callbacks: ptr::null(),
        }
    }
}

/// The runtime state from a QuickJS hook's opaque pointer (set to the `Cx`).
unsafe fn hook_cx<'a>(opaque: *mut c_void) -> &'a mut Cx {
    &mut *(opaque as *mut Cx)
}

/// Enters the realm of QuickJS context `ctx` for an embedder callback, which expects the realm
/// that is running script to be current. Returns whether a realm was pushed.
unsafe fn enter_qctx(cx: &mut Cx, ctx: *mut q::JSContext) -> bool {
    let realm = q::JS_GetContextOpaque(ctx) as *mut RealmData;
    if realm.is_null() {
        return false;
    }
    cx.enter_realm(realm);
    true
}

unsafe fn leave_qctx(cx: &mut Cx, entered: bool) {
    if entered {
        cx.leave_realm();
    }
}

unsafe fn handle<T>(ptr: *const T) -> root::JS::Handle<T> {
    root::JS::Handle::from_marked_location(ptr)
}

fn object_or_undefined(obj: *mut root::JSObject) -> q::JSValue {
    if obj.is_null() {
        q::JS_UNDEFINED
    } else {
        obj_val(obj)
    }
}

// Promises.

unsafe fn promise_state(promise: *mut root::JSObject) -> Option<q::JSPromiseStateEnum> {
    let state = q::JS_PromiseState(super::current().base_ctx, obj_val(promise));
    (state <= q::JS_PROMISE_REJECTED).then_some(state)
}

pub(crate) unsafe fn NewPromiseObject(
    cx: *mut root::JSContext,
    executor: root::JS::HandleObject,
) -> *mut root::JSObject {
    let cx = Cx::from_raw(cx);
    let qctx = cx.qctx();
    let mut funcs = [q::JS_UNDEFINED; 2];
    let promise = q::JS_NewPromiseCapability(qctx, funcs.as_mut_ptr());
    if promise.tag() == q::JS_TAG_EXCEPTION {
        return ptr::null_mut();
    }
    let mut ok = true;
    if !(*executor.ptr).is_null() {
        // The Promise constructor's steps, without looking up a possibly replaced constructor.
        let r = q::JS_Call(
            qctx,
            obj_val(*executor.ptr),
            q::JS_UNDEFINED,
            2,
            funcs.as_mut_ptr(),
        );
        if r.tag() != q::JS_TAG_EXCEPTION {
            q::JS_FreeValueRT(cx.rt, r);
        } else if q::JS_HasUncatchableException(qctx) != 0 {
            ok = false;
        } else {
            let mut error = q::JS_GetException(qctx);
            let r = q::JS_Call(qctx, funcs[1], q::JS_UNDEFINED, 1, &mut error);
            q::JS_FreeValueRT(cx.rt, error);
            ok = r.tag() != q::JS_TAG_EXCEPTION;
            q::JS_FreeValueRT(cx.rt, r);
        }
    }
    for f in funcs {
        q::JS_FreeValueRT(cx.rt, f);
    }
    if !ok {
        q::JS_FreeValueRT(cx.rt, promise);
        return ptr::null_mut();
    }
    cx.adopt(promise).to_object()
}

pub(crate) unsafe fn IsPromiseObject(obj: root::JS::HandleObject) -> bool {
    promise_state(*obj.ptr).is_some()
}

pub(crate) unsafe fn GetPromiseState(promise: root::JS::HandleObject) -> root::JS::PromiseState {
    match promise_state(*promise.ptr) {
        Some(q::JS_PROMISE_FULFILLED) => root::JS::PromiseState::Fulfilled,
        Some(q::JS_PROMISE_REJECTED) => root::JS::PromiseState::Rejected,
        _ => root::JS::PromiseState::Pending,
    }
}

pub(crate) unsafe fn JS_GetPromiseResult(
    promise: root::JS::HandleObject,
    dest: root::JS::MutableHandleValue,
) {
    let cx = super::current();
    let result = q::JS_PromiseResult(cx.base_ctx, obj_val(*promise.ptr));
    *dest.ptr = cx.adopt(result);
}

pub(crate) unsafe fn GetPromiseIsHandled(promise: root::JS::HandleObject) -> bool {
    q::JS_GetPromiseIsHandled(obj_val(*promise.ptr)) != 0
}

pub(crate) unsafe fn SetAnyPromiseIsHandled(
    cx: *mut root::JSContext,
    promise: root::JS::HandleObject,
) -> bool {
    let cx = Cx::from_raw(cx);
    let p = obj_val(*promise.ptr);
    let Some(state) = promise_state(*promise.ptr) else {
        q::JS_ThrowTypeError(cx.qctx(), c"not a promise".as_ptr());
        return false;
    };
    q::JS_SetPromiseIsHandled(p, 1);
    // Like SpiderMonkey, a settled promise is reported as handled to the rejection tracker.
    if state != q::JS_PROMISE_PENDING {
        let qctx = cx.qctx();
        notify_rejection_tracker(
            cx,
            qctx,
            *promise.ptr,
            root::JS::PromiseRejectionHandlingState::Handled,
        );
    }
    true
}

unsafe fn settle(
    cx: *mut root::JSContext,
    promise: root::JS::HandleObject,
    value: root::JS::HandleValue,
    reject: bool,
) -> bool {
    let cx = Cx::from_raw(cx);
    q::JS_SettlePromise(
        cx.qctx(),
        obj_val(*promise.ptr),
        (*value.ptr).to_qjs(),
        reject as c_int,
    ) >= 0
}

pub(crate) unsafe fn ResolvePromise(
    cx: *mut root::JSContext,
    promiseObj: root::JS::HandleObject,
    resolutionValue: root::JS::HandleValue,
) -> bool {
    settle(cx, promiseObj, resolutionValue, false)
}

pub(crate) unsafe fn RejectPromise(
    cx: *mut root::JSContext,
    promiseObj: root::JS::HandleObject,
    rejectionValue: root::JS::HandleValue,
) -> bool {
    settle(cx, promiseObj, rejectionValue, true)
}

unsafe fn original_resolve(
    cx: *mut root::JSContext,
    value: root::JS::HandleValue,
    reject: bool,
) -> *mut root::JSObject {
    let cx = Cx::from_raw(cx);
    let p = q::JS_PromiseResolve(cx.qctx(), (*value.ptr).to_qjs(), reject as c_int);
    super::class::adopt_object(cx, p)
}

pub(crate) unsafe fn CallOriginalPromiseResolve(
    cx: *mut root::JSContext,
    resolutionValue: root::JS::HandleValue,
) -> *mut root::JSObject {
    original_resolve(cx, resolutionValue, false)
}

pub(crate) unsafe fn CallOriginalPromiseReject(
    cx: *mut root::JSContext,
    rejectionValue: root::JS::HandleValue,
) -> *mut root::JSObject {
    original_resolve(cx, rejectionValue, true)
}

pub(crate) unsafe fn AddPromiseReactions(
    cx: *mut root::JSContext,
    promise: root::JS::HandleObject,
    onFulfilled: root::JS::HandleObject,
    onRejected: root::JS::HandleObject,
) -> bool {
    let cx = Cx::from_raw(cx);
    let r = q::JS_PerformPromiseThen(
        cx.qctx(),
        obj_val(*promise.ptr),
        object_or_undefined(*onFulfilled.ptr),
        object_or_undefined(*onRejected.ptr),
        0,
    );
    r.tag() != q::JS_TAG_EXCEPTION
}

pub(crate) unsafe fn SetPromiseUserInputEventHandlingState(
    promise: root::JS::HandleObject,
    state: root::JS::PromiseUserInputEventHandlingState,
) -> bool {
    // Not tracked: jobs are enqueued without their promise, so the state is never read back.
    let _ = (promise, state);
    true
}

pub(crate) unsafe fn GetPromiseUserInputEventHandlingState(
    promise: root::JS::HandleObject,
) -> root::JS::PromiseUserInputEventHandlingState {
    let _ = promise;
    root::JS::PromiseUserInputEventHandlingState::DontCare
}

pub(crate) unsafe fn SetPromiseRejectionTrackerCallback(
    cx: *mut root::JSContext,
    callback: root::JS::PromiseRejectionTrackerCallback,
    data: *mut ::std::os::raw::c_void,
) {
    let cx = Cx::from_raw(cx);
    cx.hooks.rejection_tracker = callback;
    cx.hooks.rejection_tracker_data = data;
    let opaque = cx as *mut Cx as *mut c_void;
    q::JS_SetHostPromiseRejectionTracker(cx.rt, callback.map(|_| track_rejection as _), opaque);
}

unsafe extern "C" fn track_rejection(
    ctx: *mut q::JSContext,
    promise: q::JSValue,
    reason: q::JSValue,
    is_handled: c_int,
    opaque: *mut c_void,
) {
    let _ = reason;
    let state = if is_handled != 0 {
        root::JS::PromiseRejectionHandlingState::Handled
    } else {
        root::JS::PromiseRejectionHandlingState::Unhandled
    };
    notify_rejection_tracker(hook_cx(opaque), ctx, val_obj(promise), state);
}

unsafe fn notify_rejection_tracker(
    cx: &mut Cx,
    ctx: *mut q::JSContext,
    promise: *mut root::JSObject,
    state: root::JS::PromiseRejectionHandlingState,
) {
    let Some(callback) = cx.hooks.rejection_tracker else {
        return;
    };
    let data = cx.hooks.rejection_tracker_data;
    let muted = super::error::current_script_muted(cx);
    let entered = enter_qctx(cx, ctx);
    callback(cx.as_raw(), muted, handle(&promise), state, data);
    leave_qctx(cx, entered);
}

// The job queue.

/// What a `JS::JobQueue*` points to: the embedder's traps (mozjs's `RustJobQueue`).
struct JobQueue {
    traps: crate::glue::JobQueueTraps,
    queue: *const c_void,
    interrupt_queues: *mut c_void,
}

pub(crate) unsafe fn CreateJobQueue(
    aTraps: *const crate::glue::JobQueueTraps,
    aQueue: *const ::std::os::raw::c_void,
    aInterruptQueues: *mut ::std::os::raw::c_void,
) -> *mut root::JS::JobQueue {
    let queue = Box::new(JobQueue {
        traps: *aTraps,
        queue: aQueue,
        interrupt_queues: aInterruptQueues,
    });
    Box::into_raw(queue) as *mut root::JS::JobQueue
}

pub(crate) unsafe fn DeleteJobQueue(queue: *mut root::JS::JobQueue) {
    // Like SpiderMonkey, the runtime must not run jobs with a deleted queue still installed.
    let queue = Box::from_raw(queue as *mut JobQueue);
    if let Some(drop_interrupt_queues) = queue.traps.dropInterruptQueues {
        drop_interrupt_queues(queue.interrupt_queues);
    }
}

pub(crate) unsafe fn SetJobQueue(cx: *mut root::JSContext, queue: *mut root::JS::JobQueue) {
    let cx = Cx::from_raw(cx);
    cx.hooks.job_queue = queue as *mut JobQueue;
    let hook: q::JSEnqueueJobHook = if queue.is_null() {
        None
    } else {
        Some(enqueue_job)
    };
    q::JS_SetEnqueueJobHook(cx.rt, hook, cx as *mut Cx as *mut c_void);
}

pub(crate) unsafe fn UseInternalJobQueues(cx: *mut root::JSContext) -> bool {
    SetJobQueue(cx, ptr::null_mut());
    true
}

pub(crate) unsafe fn RunJobs(cx: *mut root::JSContext) {
    let cx = Cx::from_raw(cx);
    let queue = cx.hooks.job_queue;
    if !queue.is_null() {
        if let Some(run_jobs) = (*queue).traps.runJobs {
            run_jobs((*queue).queue, cx.as_raw());
        }
        return;
    }
    loop {
        let mut ctx = ptr::null_mut();
        let r = q::JS_ExecutePendingJob(cx.rt, &mut ctx);
        if r == 0 {
            break;
        }
        if r < 0 {
            // SpiderMonkey's internal queue reports and clears job exceptions.
            let e = q::JS_GetException(cx.base_ctx);
            q::JS_FreeValueRT(cx.rt, e);
        }
    }
}

pub(crate) unsafe fn JobQueueIsEmpty(cx: *mut root::JSContext) {
    let _ = cx;
}

pub(crate) unsafe fn JobQueueMayNotBeEmpty(cx: *mut root::JSContext) {
    let _ = cx;
}

/// Slots of a job thunk: the QuickJS job function, and an array of the job's arguments.
const JOB_FUNC_SLOT: usize = 0;
const JOB_ARGS_SLOT: usize = 1;

/// Patch Q4's hook: wraps a QuickJS promise job in a job thunk and passes it to the embedder.
unsafe extern "C" fn enqueue_job(
    ctx: *mut q::JSContext,
    job_func: q::JSJobFunc,
    argc: c_int,
    argv: *mut q::JSValue,
    opaque: *mut c_void,
) -> c_int {
    let cx = hook_cx(opaque);
    let queue = cx.hooks.job_queue;
    assert!(!queue.is_null());
    let realm = q::JS_GetContextOpaque(ctx) as *mut RealmData;
    let entered = enter_qctx(cx, ctx);
    let result = enqueue_thunk(cx, &*queue, realm, job_func, argc, argv);
    leave_qctx(cx, entered);
    result
}

unsafe fn enqueue_thunk(
    cx: &mut Cx,
    queue: &JobQueue,
    realm: *mut RealmData,
    job_func: q::JSJobFunc,
    argc: c_int,
    argv: *mut q::JSValue,
) -> c_int {
    let qctx = cx.qctx();
    let args = q::JS_NewArray(qctx);
    if args.tag() == q::JS_TAG_EXCEPTION {
        return -1;
    }
    for i in 0..argc as u32 {
        let arg = q::JS_DupValueRT(cx.rt, *argv.add(i as usize));
        if q::JS_SetPropertyUint32(qctx, args, i, arg) < 0 {
            q::JS_FreeValueRT(cx.rt, args);
            return -1;
        }
    }
    let thunk = new_function(cx, Some(run_job), ptr::null(), 0, false, 0);
    if thunk.tag() == q::JS_TAG_EXCEPTION {
        q::JS_FreeValueRT(cx.rt, args);
        return -1;
    }
    let thunk = cx.adopt(thunk).to_object();
    let data = function_data(thunk).unwrap();
    // The job runs in the context that enqueued it (the base context has no realm).
    let created_in = std::mem::replace(&mut data.realm, super::hold_realm(realm));
    super::release_realm(created_in);
    data.slots[JOB_FUNC_SLOT] =
        q::JSValue::mkptr(q::JS_TAG_FLOAT64, job_func.unwrap() as *mut c_void);
    data.slots[JOB_ARGS_SLOT] = args;

    let mut host_defined_data: *mut root::JSObject = ptr::null_mut();
    if let Some(get_host_defined_data) = queue.traps.getHostDefinedData {
        let out = root::JS::MutableHandle::from_marked_location(&mut host_defined_data);
        if !get_host_defined_data(queue.queue, cx.as_raw(), out) {
            return -1;
        }
    }
    let enqueue = queue
        .traps
        .enqueuePromiseJob
        .expect("JobQueueTraps without enqueuePromiseJob");
    let null: *mut root::JSObject = ptr::null_mut();
    let ok = enqueue(
        queue.queue,
        cx.as_raw(),
        handle(&null),
        handle(&thunk),
        handle(&null),
        handle(&host_defined_data),
    );
    if !ok {
        if q::JS_HasException(cx.qctx()) == 0 {
            q::JS_ThrowInternalError(cx.qctx(), c"failed to enqueue a promise job".as_ptr());
        }
        return -1;
    }
    0
}

/// The native of job thunks: runs the job once.
unsafe extern "C" fn run_job(cx: *mut root::JSContext, argc: u32, vp: *mut JSVal) -> bool {
    let _ = argc;
    let cx = Cx::from_raw(cx);
    let data = function_data((*vp).to_object()).expect("job thunk without function data");
    let args = std::mem::replace(&mut data.slots[JOB_ARGS_SLOT], q::JS_UNDEFINED);
    *vp = UndefinedValue();
    if args.tag() != q::JS_TAG_OBJECT {
        return true;
    }
    let job_func: q::JSJobFunc = std::mem::transmute(data.slots[JOB_FUNC_SLOT].ptr());
    let ctx = if data.realm.is_null() {
        cx.base_ctx
    } else {
        (*data.realm).qctx
    };
    let mut len = 0u32;
    let len_val = q::JS_GetPropertyStr(ctx, args, c"length".as_ptr());
    q::JS_ToUint32(ctx, &mut len, len_val);
    q::JS_FreeValueRT(cx.rt, len_val);
    let mut argv: Vec<q::JSValue> = (0..len)
        .map(|i| q::JS_GetPropertyUint32(ctx, args, i))
        .collect();
    q::JS_FreeValueRT(cx.rt, args);
    let result = job_func.unwrap()(ctx, argv.len() as c_int, argv.as_mut_ptr());
    for v in argv {
        q::JS_FreeValueRT(cx.rt, v);
    }
    if result.tag() == q::JS_TAG_EXCEPTION {
        return false;
    }
    q::JS_FreeValueRT(cx.rt, result);
    true
}

// Interrupts.

pub(crate) unsafe fn JS_AddInterruptCallback(
    cx: *mut root::JSContext,
    callback: root::JSInterruptCallback,
) -> bool {
    let cx = Cx::from_raw(cx);
    cx.hooks.interrupt_callbacks.push(callback);
    q::JS_SetInterruptHandler(cx.rt, Some(interrupt_handler), cx as *mut Cx as *mut c_void);
    true
}

/// Thread-safe: only touches the shared request flag.
unsafe fn request_interrupt(cx: *mut root::JSContext) {
    let hooks = ptr::addr_of!((*(cx as *const Cx)).hooks.interrupt_requested);
    (*hooks).store(true, Ordering::SeqCst);
}

pub(crate) unsafe fn JS_RequestInterruptCallback(cx: *mut root::JSContext) {
    request_interrupt(cx);
}

pub(crate) unsafe fn JS_RequestInterruptCallbackCanWait(cx: *mut root::JSContext) {
    request_interrupt(cx);
}

/// QuickJS polls this while running script; a non-zero result terminates the script with an
/// uncatchable exception.
unsafe extern "C" fn interrupt_handler(rt: *mut q::JSRuntime, opaque: *mut c_void) -> c_int {
    let _ = rt;
    let cx = hook_cx(opaque);
    if !cx.hooks.interrupt_requested.swap(false, Ordering::SeqCst) {
        return 0;
    }
    let mut entered = false;
    if cx.realm_stack.is_empty() {
        let realm = super::error::innermost_script_realm(cx);
        if !realm.is_null() {
            entered = enter_qctx(cx, realm);
        }
    }
    let callbacks = cx.hooks.interrupt_callbacks.clone();
    let mut keep_running = true;
    for callback in callbacks.into_iter().flatten() {
        keep_running &= callback(cx.as_raw());
    }
    leave_qctx(cx, entered);
    (!keep_running) as c_int
}

// Security callbacks.

pub(crate) unsafe fn JS_SetSecurityCallbacks(
    cx: *mut root::JSContext,
    callbacks: *const root::JSSecurityCallbacks,
) {
    let cx = Cx::from_raw(cx);
    cx.hooks.security_callbacks = callbacks;
    let checks = !callbacks.is_null() && (*callbacks).contentSecurityPolicyAllows.is_some();
    let hook: q::JSCanCompileStringsHook = if checks {
        Some(can_compile_strings)
    } else {
        None
    };
    q::JS_SetCanCompileStringsHook(cx.rt, hook, cx as *mut Cx as *mut c_void);
}

pub(crate) unsafe fn JS_GetSecurityCallbacks(
    cx: *mut root::JSContext,
) -> *const root::JSSecurityCallbacks {
    Cx::from_raw(cx).hooks.security_callbacks
}

pub(crate) unsafe fn JS_SetTrustedPrincipals(
    cx: *mut root::JSContext,
    prin: *mut root::JSPrincipals,
) {
    // There are no system compartments to select.
    let _ = (cx, prin);
}

/// Patch Q5's hook: SpiderMonkey's `contentSecurityPolicyAllows` for eval and `Function`.
unsafe extern "C" fn can_compile_strings(
    ctx: *mut q::JSContext,
    kind: q::JSCompileKindEnum,
    code: q::JSValue,
    argc: c_int,
    arg_strings: *mut q::JSValue,
    args: *mut q::JSValue,
    opaque: *mut c_void,
) -> c_int {
    let cx = hook_cx(opaque);
    let Some(allows) = (*cx.hooks.security_callbacks).contentSecurityPolicyAllows else {
        return 1;
    };
    let entered = enter_qctx(cx, ctx);
    let code_string = cx.retain(JSVal::from_qjs(code)).to_string();
    let mut parameter_strings: Vec<*mut root::JSString> = Vec::new();
    let mut parameter_args: Vec<JSVal> = Vec::new();
    let (compilation_type, body_string, body_arg) = if kind == q::JS_COMPILE_FUNCTION {
        let params = (argc as usize).saturating_sub(1);
        for i in 0..params {
            parameter_strings.push(cx.retain(JSVal::from_qjs(*arg_strings.add(i))).to_string());
            parameter_args.push(JSVal::from_qjs(*args.add(i)));
        }
        let (body_string, body_arg) = if argc > 0 {
            let body = cx
                .retain(JSVal::from_qjs(*arg_strings.add(params)))
                .to_string();
            (body, JSVal::from_qjs(*args.add(params)))
        } else {
            let empty = cx
                .adopt(q::JS_NewStringLen(ctx, c"".as_ptr(), 0))
                .to_string();
            (empty, UndefinedValue())
        };
        (root::JS::CompilationType::Function, body_string, body_arg)
    } else {
        let compilation_type = if kind == q::JS_COMPILE_DIRECT_EVAL {
            root::JS::CompilationType::DirectEval
        } else {
            root::JS::CompilationType::IndirectEval
        };
        (compilation_type, code_string, JSVal::from_qjs(code))
    };
    let mut can_compile = false;
    let ok = allows(
        cx.as_raw(),
        root::JS::RuntimeCode::JS,
        handle(&code_string),
        compilation_type,
        handle(&parameter_strings as *const Vec<_> as *const _),
        handle(&body_string),
        handle(&parameter_args as *const Vec<_> as *const _),
        handle(&body_arg),
        &mut can_compile,
    );
    leave_qctx(cx, entered);
    if !ok {
        if q::JS_HasException(ctx) == 0 {
            q::JS_ThrowInternalError(ctx, c"script terminated".as_ptr());
            q::JS_SetUncatchableException(ctx, 1);
        }
        return -1;
    }
    can_compile as c_int
}
