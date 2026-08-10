//! Call dispatch: argument resolution and invoking every kind of callable.
//!
//! [`call_value`] is the single entry point for calling a `VmValue` — user
//! functions and closures, bound methods, native/builtin/library functions, the
//! stateful `NativeFnId` package methods, and type constructors. [`call_fn`]
//! runs a compiled function body in a fresh register frame.

use super::*;

/// Replace zero-span placeholders from built-in error paths with the actual call-site span.
pub(crate) fn patch_builtin_span(mut e: JadeError, call_span: Span) -> JadeError {
    match &mut e {
        JadeError::ArityMismatch { span, .. }
        | JadeError::TypeError { span, .. }
        | JadeError::IoError { span, .. } => {
            if span.line == 0 {
                *span = call_span;
            }
        }
        _ => {}
    }
    e
}

/// Resolve a mix of positional and named arguments into a positional Vec by
/// matching named args against the callee's parameter list.
pub(crate) fn resolve_named_args(
    callee: &VmValue,
    positional: Vec<VmValue>,
    named: Vec<(String, VmValue)>,
    span: Span,
) -> Result<Vec<VmValue>> {
    if named.is_empty() {
        return Ok(positional);
    }
    match callee {
        VmValue::Fn(cf) => {
            let params = &cf.params;
            let mut result = vec![VmValue::Nil; params.len()];
            for (i, v) in positional.into_iter().enumerate() {
                if i < result.len() {
                    result[i] = v;
                }
            }
            for (name, v) in named {
                let pos = params.iter().position(|p| p == &name).ok_or_else(|| {
                    JadeError::TypeError { message: format!("unknown parameter '{}'", name), span }
                })?;
                result[pos] = v;
            }
            Ok(result)
        }
        _ => {
            // For native/builtin/closure callees, append named values positionally.
            let mut args = positional;
            for (_, v) in named {
                args.push(v);
            }
            Ok(args)
        }
    }
}

/// Build the VM dict value for an imported stdlib package.
///
/// This used to name `llm`, `std/array`, and `std/uhttp` by string and call a
/// bespoke override for each — so a package's own stateful functions were
/// registered here, in the VM, rather than beside the package. They now travel
/// in `Package::natives` and `vm_dict_value` handles them uniformly.
pub(crate) fn package_dict_value(pkg: &builtins::Package) -> VmValue {
    pkg.vm_dict_value()
}

#[async_recursion::async_recursion]
pub(crate) async fn call_value(
    callee: VmValue,
    args: Vec<VmValue>,
    state: &mut VmState,
    span: Span,
) -> Result<VmValue> {
    match callee {
        VmValue::Fn(cf) => call_fn(&cf, args, state, span).await,
        VmValue::Closure(cf, captured) => {
            // Temporarily inject captured variables into globals so the closure body
            // sees them via GetGlobal. Save any displaced values and restore after.
            let mut saved: Vec<(String, Option<VmValue>)> = Vec::new();
            for (k, v) in captured.iter() {
                let old = state.globals.insert(k.clone(), v.clone());
                saved.push((k.clone(), old));
            }
            let result = call_fn(&cf, args, state, span).await;
            for (k, old) in saved {
                match old {
                    Some(v) => {
                        state.globals.insert(k, v);
                    }
                    None => {
                        state.globals.remove(&k);
                    }
                }
            }
            result
        }
        VmValue::BoundMethod(bm) => {
            let method = Arc::clone(&bm.method);
            let mut full_args = Vec::with_capacity(args.len() + 1);
            full_args.push(VmValue::Struct(Arc::clone(&bm.receiver)));
            full_args.extend(args);
            call_fn(&method, full_args, state, span).await
        }
        VmValue::BuiltinFn(bf) => (bf.vm_impl)(&args).map_err(|e| patch_builtin_span(e, span)),
        VmValue::NativeLibFn(nfn) => {
            // Any call can be the one a library calls back from, so once this
            // VM has handed out a Jade function, every native call serves.
            // Without that second condition the rest of this machinery is
            // invisible: `ares_process` passes no function, would take the
            // direct path on the interpreter's own thread, and a callback fired
            // from it would find nobody draining.
            //
            // A program that never passes a callback keeps the direct path and
            // pays for no worker thread.
            if state.callbacks.has_live() || args.iter().any(crate::native::is_callable) {
                call_native_with_callbacks(&nfn, args, state, span).await
            } else {
                nfn.call(&args, span)
            }
        }
        VmValue::NativeBoundMethod(nbm) => {
            // Checked here rather than inside each implementation, which reads
            // the arguments it wants and never sees the rest. The compiler
            // rejects a surplus argument, so accepting it here made the two
            // engines disagree about whether the program was valid at all.
            if let Some(want) = builtins::primitive_method_arity(nbm.method.name) {
                if args.len() != want {
                    return Err(JadeError::ArityMismatch { expected: want, got: args.len(), span });
                }
            }
            let mut full_args = Vec::with_capacity(args.len() + 1);
            full_args.push(nbm.receiver.clone());
            full_args.extend(args);
            (nbm.method.vm_impl)(&full_args).map_err(|e| patch_builtin_span(e, span))
        }
        VmValue::NativeFn(nf) => match nf {
            NativeFnId::Print => {
                if args.is_empty() || args.len() > 2 {
                    return Err(JadeError::ArityMismatch { expected: 1, got: args.len(), span });
                }
                let mut iter = args.into_iter();
                let val = iter.next().unwrap();
                // Optional `end` kwarg (arrives positionally for native callees).
                // Default "\n" matches Python's print() behaviour.
                let end = match iter.next() {
                    None | Some(VmValue::Nil) => "\n".to_owned(),
                    Some(VmValue::Str(s)) => s.to_string(),
                    Some(other) => {
                        return Err(JadeError::TypeError {
                            message: format!(
                                "print() end= must be str, got {}",
                                value_type_name(&other)
                            ),
                            span,
                        });
                    }
                };
                match val {
                    VmValue::TokenStream(ts) => {
                        // The mute spec rides on the stream, put there by a
                        // `?p |> g` stage. `print` no longer needs to be told.
                        let (sm, rs, rp) =
                            (ts.start_muted, ts.region_start.clone(), ts.region_stop.clone());
                        vm_drain_token_stream_printing(ts, state, span, end == "\n", sm, &rs, &rp)
                            .await?;
                        if end != "\n" && !end.is_empty() {
                            crate::stdio::write_str_flush(&end);
                        }
                    }
                    other => {
                        crate::stdio::write_str_flush(&format!(
                            "{}{}",
                            value_to_display(&other),
                            end
                        ));
                    }
                }
                Ok(VmValue::Nil)
            }
            NativeFnId::Route => {
                if args.is_empty() || args.len() > 2 {
                    return Err(JadeError::ArityMismatch { expected: 2, got: args.len(), span });
                }
                let mut iter = args.into_iter();
                let obj = iter.next().unwrap();
                // If `on` is omitted, try route_configs for this struct's type.
                let on = iter.next().unwrap_or_else(|| {
                    if let VmValue::Struct(ref s) = obj {
                        let type_name = s.lock().type_name().to_string();
                        if let Some(field_name) = state.route_configs.get(&type_name) {
                            let fields = s.lock();
                            return fields.get_field(field_name).cloned().unwrap_or(VmValue::Nil);
                        }
                    }
                    VmValue::Nil
                });
                match on {
                    VmValue::Nil => Ok(obj),
                    VmValue::Str(method_name) => {
                        // Prefer the struct's own extend methods; fall back to globals.
                        let fn_val = if let VmValue::Struct(ref s) = obj {
                            let type_name = s.lock().type_name().to_string();
                            state
                                .extend_methods
                                .get(&type_name)
                                .and_then(|m| m.get(method_name.as_str()))
                                .map(|cf| VmValue::Fn(Arc::clone(cf)))
                                .or_else(|| state.globals.get(method_name.as_str()).cloned())
                        } else {
                            state.globals.get(method_name.as_str()).cloned()
                        };
                        match fn_val {
                            Some(f) => call_value(f, vec![obj], state, span).await,
                            None => Err(JadeError::Exception {
                                message: format!(
                                    "route(): no method or function named {:?}",
                                    method_name
                                ),
                                span,
                            }),
                        }
                    }
                    other => Err(JadeError::TypeError {
                        message: format!(
                            "route(): expected string method name, got {}",
                            value_to_display(&other)
                        ),
                        span,
                    }),
                }
            }
            NativeFnId::ArrayMap => {
                // array.map(arr, fn) → new array of fn(elem) for each element.
                if args.len() != 2 {
                    return Err(JadeError::ArityMismatch { expected: 2, got: args.len(), span });
                }
                let elems = match &args[0] {
                    VmValue::Array(arc) => arc.lock().clone(),
                    other => {
                        return Err(JadeError::TypeError {
                            message: format!(
                                "array.map: first argument must be an array, got {}",
                                value_type_name(other)
                            ),
                            span,
                        });
                    }
                };
                let f = args[1].clone();
                let mut out = Vec::with_capacity(elems.len());
                for e in elems {
                    out.push(call_value(f.clone(), vec![e], state, span).await?);
                }
                Ok(VmValue::Array(Arc::new(Mutex::new(ArrayObj::from_vec(out)))))
            }
            NativeFnId::ArrayFilter => {
                // array.filter(arr, fn) → elements for which fn(elem) is true.
                if args.len() != 2 {
                    return Err(JadeError::ArityMismatch { expected: 2, got: args.len(), span });
                }
                let elems = match &args[0] {
                    VmValue::Array(arc) => arc.lock().clone(),
                    other => {
                        return Err(JadeError::TypeError {
                            message: format!(
                                "array.filter: first argument must be an array, got {}",
                                value_type_name(other)
                            ),
                            span,
                        });
                    }
                };
                let f = args[1].clone();
                let mut out = Vec::new();
                for e in elems {
                    match call_value(f.clone(), vec![e.clone()], state, span).await? {
                        VmValue::Bool(true) => out.push(e),
                        VmValue::Bool(false) => {}
                        other => {
                            return Err(JadeError::TypeError {
                                message: format!(
                                    "array.filter: predicate must return a bool, got {}",
                                    value_type_name(&other)
                                ),
                                span,
                            });
                        }
                    }
                }
                Ok(VmValue::Array(Arc::new(Mutex::new(ArrayObj::from_vec(out)))))
            }
            NativeFnId::UhttpStream => {
                use crate::uhttp::{self, StreamEvent};
                if args.len() < 2 || args.len() > 3 {
                    return Err(JadeError::ArityMismatch { expected: 2, got: args.len(), span });
                }
                let url = match &args[0] {
                    VmValue::Str(s) => s.clone(),
                    other => {
                        return Err(JadeError::TypeError {
                            message: format!(
                                "uhttp.stream() url must be a str, got {}",
                                value_type_name(other)
                            ),
                            span,
                        });
                    }
                };
                let handler = args[1].clone();
                if !matches!(
                    handler,
                    VmValue::Fn(_) | VmValue::Closure(_, _) | VmValue::BoundMethod(_)
                ) {
                    return Err(JadeError::TypeError {
                        message: format!(
                            "uhttp.stream() handler must be a function, got {}",
                            value_type_name(&handler)
                        ),
                        span,
                    });
                }
                let headers =
                    uhttp::extract_headers(args.get(2)).map_err(|e| patch_builtin_span(e, span))?;

                let mut rx =
                    uhttp::open_stream(&url, headers).map_err(|e| patch_builtin_span(e, span))?;
                let mut status: i64 = 0;
                while let Some(ev) = rx.recv().await {
                    match ev {
                        StreamEvent::Status(s) => status = s as i64,
                        StreamEvent::Line(line) => {
                            let r = call_value(
                                handler.clone(),
                                vec![VmValue::Str(line.into())],
                                state,
                                span,
                            )
                            .await?;
                            // A handler returning `false` stops the stream early;
                            // dropping `rx` closes the socket on the worker side.
                            if matches!(r, VmValue::Bool(false)) {
                                break;
                            }
                        }
                        StreamEvent::Error(e) => {
                            return Err(patch_builtin_span(uhttp::uhttp_io_error(&e), span));
                        }
                    }
                }
                Ok(VmValue::Int(status))
            }
        },
        VmValue::TypeRef(type_name) => {
            if args.len() != 1 {
                return Err(JadeError::ArityMismatch { expected: 1, got: args.len(), span });
            }
            vm_type_call(type_name, args.into_iter().next().unwrap(), state, span)
        }
        _ => Err(JadeError::NotCallable { span }),
    }
}

/// How many nested Jade calls `call_fn` allows before it raises rather than
/// recursing further.
///
/// Matches the AOT backend's `JRT_RECUR_MAX_DEPTH` (`src/runtime_aot/common.c`)
/// so a call chain fails at the same depth under `jade run` and a compiled
/// binary — see `JadeError::RecursionLimitExceeded` and TOOLCHAIN-BUGS #10.
/// 10,000 is where the compiled engine already ran clean, and measurement
/// showed the interpreter's own stack (see `vm::chunk::VM_STACK_SIZE`) has
/// comfortable headroom past it: a release build overflowed its native stack
/// at ~673 frames on an 8 MiB stack (~12.4 KB/frame); a debug build, with its
/// larger unoptimized async state machines, overflowed at ~61 frames on the
/// same 8 MiB (~137 KB/frame). Either way the point of this counter is that
/// it trips *before* the native stack would — the OS should never be the
/// thing that stops a runaway recursion.
pub(crate) const MAX_CALL_DEPTH: u32 = 10_000;

/// Standalone version of `call_value` that owns its `VmState`, suitable for
/// passing to `tokio::spawn` where borrowed state cannot cross thread boundaries.
/// Always returns `(result, raised_exception)` so the parent can propagate the
/// exception value (struct/string) through try/catch rather than losing it.
#[async_recursion::async_recursion]

pub(crate) async fn call_fn(
    cf: &CompiledFn,
    args: Vec<VmValue>,
    state: &mut VmState,
    span: Span,
) -> Result<VmValue> {
    // Every call counts against the recursion limit, checked before any of
    // the (comparatively expensive) frame setup below runs. `state.call_depth`
    // is what the AOT backend's per-function `jrt_recur_enter`/`_restore` pair
    // mirrors — see `MAX_CALL_DEPTH` for why the two engines share one number.
    if state.call_depth >= state.max_call_depth {
        return Err(JadeError::RecursionLimitExceeded { span });
    }
    state.call_depth += 1;
    let result = call_fn_body(cf, args, state, span).await;
    state.call_depth -= 1;
    result
}

/// The actual body of a call: arity/defaults, frame setup, running the
/// compiled body, and generator buffering. Split out of `call_fn` so the
/// depth-counter increment in that function has exactly one place to
/// decrement from — a `?` anywhere in here still runs the caller's
/// decrement, because it returns to `call_fn` rather than out of it.
async fn call_fn_body(
    cf: &CompiledFn,
    args: Vec<VmValue>,
    state: &mut VmState,
    span: Span,
) -> Result<VmValue> {
    // For bound methods `self` has already been prepended to `args`.
    // Fill trailing defaults for any omitted optional parameters.
    let mut args = args;
    if args.len() < cf.params.len() {
        let missing_start = args.len();
        for i in missing_start..cf.params.len() {
            match cf.defaults.get(i).and_then(|d| d.as_ref()) {
                Some(default) => args.push(default.clone()),
                None => {
                    return Err(JadeError::ArityMismatch {
                        expected: cf.params.len(),
                        got: missing_start,
                        span,
                    });
                }
            }
        }
    } else if args.len() > cf.params.len() {
        return Err(JadeError::ArityMismatch { expected: cf.params.len(), got: args.len(), span });
    }
    // Build the frame: params occupy slots 0..params.len(); rest start as Nil.
    let n = (cf.n_slots as usize).max(cf.params.len());
    let mut frame = vec![VmValue::Nil; n];
    for (i, v) in args.into_iter().enumerate() {
        frame[i] = vm_maybe_drain(v, state, span).await?;
    }
    let saved_scope = state.active_module_scope.clone();
    if let Some(scope) = &cf.module_scope {
        state.active_module_scope = Some(Arc::clone(scope));
    }
    // A generator runs its body to completion into a fresh buffer and hands the
    // buffer back instead of the body's value. This is the "a stream is a
    // buffer" model in one place: nothing is lazy, nothing is one-shot, and the
    // caller can read the result as many times as it likes.
    //
    // The buffer is pushed *before* the body runs and popped after, so a
    // generator that calls another generator keeps their yields separate.
    if cf.is_generator {
        state.yield_stack.push(Arc::new(Mutex::new(Vec::new())));
    }
    let result = execute_chunk(&cf.chunk, &mut frame, state).await.map_err(|e| {
        if cf.source_file.is_empty() || matches!(e, JadeError::InFile { .. }) {
            e
        } else {
            JadeError::InFile { file: cf.source_file.clone(), cause: Box::new(e) }
        }
    });
    state.active_module_scope = saved_scope;
    if cf.is_generator {
        // Pop even on the error path, or a raise inside a generator would leave
        // its buffer on the stack and the next `yield` anywhere would land in it.
        let buf = state.yield_stack.pop();
        result?;
        return Ok(VmValue::Stream(buf.unwrap_or_default()));
    }
    Ok(result?.unwrap_or(VmValue::Nil))
}

// ── Prompt deref ──────────────────────────────────────────────────────────────

// ── Native calls that pass a Jade function ────────────────────────────────────

/// Run a native call that hands the library a Jade callback.
///
/// The VM cannot be re-entered from a C frame. Calling a Jade function needs
/// `VmState` and an async context, and during a native call the C library holds
/// the stack — so there is no way to answer a callback from where it arrives.
///
/// The inversion: the C call runs on a worker thread, and each callback *posts*
/// its arguments here and waits. The interpreter stays on its own thread, picks
/// the request up in the loop below, and answers. That is the same shape
/// `uhttp.stream` uses to drive a Jade handler, run in both directions.
///
/// Callbacks are therefore serviced only while this call is in flight. A library
/// that stores one and invokes it later finds nobody listening, and its
/// `invoke` reports failure rather than blocking forever — a registration
/// outliving the call is not supported, and could not be without keeping the
/// interpreter available indefinitely.
async fn call_native_with_callbacks(
    nfn: &Arc<crate::native::NativeLibFn>,
    args: Vec<VmValue>,
    state: &mut VmState,
    span: Span,
) -> Result<VmValue> {
    // The bus belongs to the VM, not to this call. A library that stores a
    // callback invokes it from some later call entirely, so the loop below
    // serves requests raised by *any* registration this VM has handed out, not
    // only the ones this call is passing.
    let bus = Arc::clone(&state.callbacks);
    let marshalled = crate::native::marshal_with_callbacks(&args, &bus, span);

    // Counted for the duration of the loop. A callback fired while nothing is
    // draining gets the neutral answer instead of blocking on a channel that no
    // longer closes when a call ends.
    let _serving = bus.serving();

    let fn_ptr = nfn.fn_ptr();
    let mut call = Box::pin(tokio::task::spawn_blocking(move || {
        let args = marshalled;
        let mut out = crate::native::JadeVal::nil();
        let status = unsafe { (fn_ptr)(args.argv.len(), args.argv.as_ptr(), &mut out) };
        crate::native::NativeCallResult { args, out, status }
    }));

    // Serve callbacks until the native call finishes. `biased` polls the
    // callback arm first so a request that arrived alongside completion is
    // still answered rather than dropped.
    let done = loop {
        tokio::select! {
            biased;
            // The receiver's lock is released inside `recv`, before any Jade
            // code runs — a callback may itself call a native function, and
            // that call has to be able to serve its own callbacks or it hangs
            // waiting for a receiver this loop is holding.
            Some(req) = bus.recv() => {
                let result = call_value(req.callee, req.args, state, req.span).await;
                // A closed receiver means the worker gave up waiting; there is
                // nothing to do about it and nothing to report.
                let _ = req.reply.send(result);
            }
            done = &mut call => {
                break done.map_err(|e| JadeError::IoError {
                    message: format!("native fn '{}' panicked: {e}", nfn.name),
                    span,
                })?;
            }
        }
    };

    // The JadeFn wrappers are the bus's now, and are released with the VM.
    // Nothing in C says when a library is finished with a stored callback, so
    // there is no moment at which freeing one here would be safe.
    crate::native::finish_native_call(&nfn.name, &done.args.argv, &done.out, done.status, span)
}
