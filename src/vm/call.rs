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
            if span.line == 0 { *span = call_span; }
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
                if i < result.len() { result[i] = v; }
            }
            for (name, v) in named {
                let pos = params.iter().position(|p| p == &name)
                    .ok_or_else(|| JadeError::TypeError {
                        message: format!("unknown parameter '{}'", name),
                        span,
                    })?;
                result[pos] = v;
            }
            Ok(result)
        }
        _ => {
            // For native/builtin/closure callees, append named values positionally.
            let mut args = positional;
            for (_, v) in named { args.push(v); }
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
                    Some(v) => { state.globals.insert(k, v); }
                    None    => { state.globals.remove(&k); }
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
        VmValue::NativeLibFn(nfn) => nfn.call(&args, span),
        VmValue::NativeBoundMethod(nbm) => {
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
                    Some(VmValue::Str(s))     => s.to_string(),
                    Some(other) => return Err(JadeError::TypeError {
                        message: format!("print() end= must be str, got {}", value_type_name(&other)),
                        span,
                    }),
                };
                match val {
                    VmValue::TokenStream(ts) => {
                        vm_drain_token_stream_printing(ts, state, span, end == "\n", false, &[], &[]).await?;
                        if end != "\n" && !end.is_empty() {
                            crate::stdio::write_str_flush(&end);
                        }
                    }
                    other => {
                        crate::stdio::write_str_flush(&format!("{}{}", value_to_display(&other), end));
                    }
                }
                Ok(VmValue::Nil)
            }
            NativeFnId::Stream => {
                if args.is_empty() {
                    return Err(JadeError::ArityMismatch { expected: 1, got: 0, span });
                }
                let mut iter = args.into_iter();
                let val = iter.next().unwrap();
                // Build VM-side mute spec AND daemon inference constraints.
                //
                // Mute semantics:
                //   No anchor  → start muted immediately (from first token).
                //   Anchor     → enter muted mode when anchor string appears.
                //   Stop_anchor → exit muted mode when stop string appears.
                //   No stop_anchor → stay muted until end of stream.
                let mut start_muted = false;
                let mut region_start: Vec<String> = Vec::new();
                let mut region_stop: Vec<String> = Vec::new();
                let mut infer_grammar: Option<String> = None;
                let mut infer_anchor: Option<String> = None;
                let mut infer_stop: Option<String> = None;
                match iter.next() {
                    None | Some(VmValue::Nil) => {}
                    Some(VmValue::Array(arr)) => {
                        for v in arr.lock().iter() {
                            if let VmValue::Grammar(g) = v {
                                if infer_grammar.is_none() {
                                    // `to_gbnf()`, not `.pattern` — this site used to send
                                    // the bare pattern, so `stream(?p, mute_on=[g])` and
                                    // `?p |> g` constrained the model differently with the
                                    // same Grammar value.
                                    infer_grammar = Some(g.to_gbnf());
                                    infer_anchor = g.anchor.clone();
                                    infer_stop = g.stop.clone();
                                }
                                if let Some(a) = &g.anchor {
                                    if !region_start.contains(a) { region_start.push(a.clone()); }
                                    if let Some(s) = &g.stop {
                                        if !region_stop.contains(s) { region_stop.push(s.clone()); }
                                    }
                                } else {
                                    // No anchor → mute from the very start of generation.
                                    start_muted = true;
                                    if let Some(s) = &g.stop {
                                        if !region_stop.contains(s) { region_stop.push(s.clone()); }
                                    }
                                }
                            }
                        }
                    }
                    Some(other) => return Err(JadeError::TypeError {
                        message: format!("stream() mute_on= must be an array of grammars, got {}", value_type_name(&other)),
                        span,
                    }),
                };
                match val {
                    VmValue::TokenStream(ts) => {
                        // Start lazy inference with grammar constraints so jade-tree
                        // receives stop_anchor and stops before the model can loop.
                        {
                            let lazy = ts.lazy_prompt.lock().take();
                            if let Some(prompt_text) = lazy {
                                let backend = state.inference_backend.as_ref()
                                    .ok_or(JadeError::MissingApiKey { span })?.clone();
                                let (rx, handle) = backend.infer_stream(llm::InferenceRequest {
                                    prompt: prompt_text,
                                    grammar: infer_grammar,
                                    anchor: infer_anchor,
                                    stop_anchor: infer_stop, ..Default::default()
                                }, span).await?;
                                *ts.rx.lock() = Some(rx);
                                *ts.tokens_handle.lock() = Some(handle);
                            }
                        }
                        let text = vm_drain_token_stream_printing(
                            ts, state, span, true,
                            start_muted, &region_start, &region_stop,
                        ).await?;
                        Ok(VmValue::Str(text.into()))
                    }
                    other => {
                        let s = value_to_display(&other);
                        crate::stdio::write_str_flush(&format!("{s}\n"));
                        Ok(VmValue::Str(s.into()))
                    }
                }
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
                            return fields.get_field(field_name)
                                .cloned()
                                .unwrap_or(VmValue::Nil);
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
                            state.extend_methods
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
                                message: format!("route(): no method or function named {:?}", method_name),
                                span,
                            }),
                        }
                    }
                    other => Err(JadeError::TypeError {
                        message: format!("route(): expected string method name, got {}", value_to_display(&other)),
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
                    other => return Err(JadeError::TypeError {
                        message: format!("array.map: first argument must be an array, got {}", value_type_name(other)),
                        span,
                    }),
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
                    other => return Err(JadeError::TypeError {
                        message: format!("array.filter: first argument must be an array, got {}", value_type_name(other)),
                        span,
                    }),
                };
                let f = args[1].clone();
                let mut out = Vec::new();
                for e in elems {
                    match call_value(f.clone(), vec![e.clone()], state, span).await? {
                        VmValue::Bool(true)  => out.push(e),
                        VmValue::Bool(false) => {}
                        other => return Err(JadeError::TypeError {
                            message: format!("array.filter: predicate must return a bool, got {}", value_type_name(&other)),
                            span,
                        }),
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
                    other => return Err(JadeError::TypeError {
                        message: format!("uhttp.stream() url must be a str, got {}", value_type_name(other)),
                        span,
                    }),
                };
                let handler = args[1].clone();
                if !matches!(handler,
                    VmValue::Fn(_) | VmValue::Closure(_, _) | VmValue::BoundMethod(_)) {
                    return Err(JadeError::TypeError {
                        message: format!("uhttp.stream() handler must be a function, got {}", value_type_name(&handler)),
                        span,
                    });
                }
                let headers = uhttp::extract_headers(args.get(2))
                    .map_err(|e| patch_builtin_span(e, span))?;

                let mut rx = uhttp::open_stream(&url, headers)
                    .map_err(|e| patch_builtin_span(e, span))?;
                let mut status: i64 = 0;
                while let Some(ev) = rx.recv().await {
                    match ev {
                        StreamEvent::Status(s) => status = s as i64,
                        StreamEvent::Line(line) => {
                            let r = call_value(handler.clone(), vec![VmValue::Str(line.into())], state, span).await?;
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
    // For bound methods `self` has already been prepended to `args`.
    // Fill trailing defaults for any omitted optional parameters.
    let mut args = args;
    if args.len() < cf.params.len() {
        let missing_start = args.len();
        for i in missing_start..cf.params.len() {
            match cf.defaults.get(i).and_then(|d| d.as_ref()) {
                Some(default) => args.push(default.clone()),
                None => return Err(JadeError::ArityMismatch {
                    expected: cf.params.len(),
                    got: missing_start,
                    span,
                }),
            }
        }
    } else if args.len() > cf.params.len() {
        return Err(JadeError::ArityMismatch {
            expected: cf.params.len(),
            got: args.len(),
            span,
        });
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
    let result = execute_chunk(&cf.chunk, &mut frame, state).await
        .map_err(|e| {
            if cf.source_file.is_empty() || matches!(e, JadeError::InFile { .. }) {
                e
            } else {
                JadeError::InFile { file: cf.source_file.clone(), cause: Box::new(e) }
            }
        });
    state.active_module_scope = saved_scope;
    Ok(result?.unwrap_or(VmValue::Nil))
}

// ── Prompt deref ──────────────────────────────────────────────────────────────

