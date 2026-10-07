Tests of mozjs 0.23.1 (servo/mozjs 95cacf65a1, `mozjs/tests/`), unmodified. `../tests/` holds the
copies that run against the shim, with these changes:

- `jsvalue.rs`: imports `JSVal` from `mozjs` (the shim has no `mozjs_sys` crate).
- `callback.rs`: throws with `throw_internal_error_safe` instead of the variadic `JS_ReportErrorASCII`.

Known failures against the shim:

- `capture_stack.rs`: QuickJS numbers lines from 1 when the compile options say 0, and places a call at its `(`.
- `custom_auto_rooter.rs`, `custom_auto_rooter_macro.rs`: QuickJS never traces roots, so custom rooters' `trace` is not called.
