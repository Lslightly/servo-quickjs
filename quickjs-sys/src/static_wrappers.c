#include "quickjs.h"

// Static wrappers

JSValue __JS_NewFloat64__sys(JSContext *ctx, double d) { return __JS_NewFloat64(ctx, d); }
int JS_VALUE_IS_NAN__sys(JSValue v) { return JS_VALUE_IS_NAN(v); }
JSValue __JS_NewShortBigInt__sys(JSContext *ctx, int64_t d) { return __JS_NewShortBigInt(ctx, d); }
const char * JS_AtomToCString__sys(JSContext *ctx, JSAtom atom) { return JS_AtomToCString(ctx, atom); }
JSValue JS_NewBool__sys(JSContext *ctx, int val) { return JS_NewBool(ctx, val); }
JSValue JS_NewInt32__sys(JSContext *ctx, int32_t val) { return JS_NewInt32(ctx, val); }
JSValue JS_NewCatchOffset__sys(JSContext *ctx, int32_t val) { return JS_NewCatchOffset(ctx, val); }
JSValue JS_NewInt64__sys(JSContext *ctx, int64_t val) { return JS_NewInt64(ctx, val); }
JSValue JS_NewUint32__sys(JSContext *ctx, uint32_t val) { return JS_NewUint32(ctx, val); }
JSValue JS_NewFloat64__sys(JSContext *ctx, double d) { return JS_NewFloat64(ctx, d); }
int JS_IsNumber__sys(JSValue v) { return JS_IsNumber(v); }
int JS_IsBigInt__sys(JSContext *ctx, JSValue v) { return JS_IsBigInt(ctx, v); }
int JS_IsBool__sys(JSValue v) { return JS_IsBool(v); }
int JS_IsNull__sys(JSValue v) { return JS_IsNull(v); }
int JS_IsUndefined__sys(JSValue v) { return JS_IsUndefined(v); }
int JS_IsException__sys(JSValue v) { return JS_IsException(v); }
int JS_IsUninitialized__sys(JSValue v) { return JS_IsUninitialized(v); }
int JS_IsString__sys(JSValue v) { return JS_IsString(v); }
int JS_IsSymbol__sys(JSValue v) { return JS_IsSymbol(v); }
int JS_IsObject__sys(JSValue v) { return JS_IsObject(v); }
void JS_FreeValue__sys(JSContext *ctx, JSValue v) { JS_FreeValue(ctx, v); }
void JS_FreeValueRT__sys(JSRuntime *rt, JSValue v) { JS_FreeValueRT(rt, v); }
JSValue JS_DupValue__sys(JSContext *ctx, JSValue v) { return JS_DupValue(ctx, v); }
JSValue JS_DupValueRT__sys(JSRuntime *rt, JSValue v) { return JS_DupValueRT(rt, v); }
int JS_ToUint32__sys(JSContext *ctx, uint32_t *pres, JSValue val) { return JS_ToUint32(ctx, pres, val); }
JSValue JS_NewString__sys(JSContext *ctx, const char *str) { return JS_NewString(ctx, str); }
const char * JS_ToCStringLen__sys(JSContext *ctx, size_t *plen, JSValue val1) { return JS_ToCStringLen(ctx, plen, val1); }
const char * JS_ToCString__sys(JSContext *ctx, JSValue val1) { return JS_ToCString(ctx, val1); }
JSValue JS_GetProperty__sys(JSContext *ctx, JSValue this_obj, JSAtom prop) { return JS_GetProperty(ctx, this_obj, prop); }
int JS_SetProperty__sys(JSContext *ctx, JSValue this_obj, JSAtom prop, JSValue val) { return JS_SetProperty(ctx, this_obj, prop, val); }
JSValue JS_NewCFunction__sys(JSContext *ctx, JSCFunction *func, const char *name, int length) { return JS_NewCFunction(ctx, func, name, length); }
JSValue JS_NewCFunctionMagic__sys(JSContext *ctx, JSCFunctionMagic *func, const char *name, int length, JSCFunctionEnum cproto, int magic) { return JS_NewCFunctionMagic(ctx, func, name, length, cproto, magic); }
