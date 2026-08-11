//! Prompt dereference and token-stream draining — the VM's LLM machinery.
//!
//! `?p` / `?p |> Type` lower to `vm_prompt_deref`, which sends a request to the
//! inference backend, optionally constrains sampling with a grammar, and coerces
//! the reply. The streaming variants drain a live token channel (with optional
//! anchor-based muting) into a string while echoing tokens as they arrive.

use super::*;

pub(crate) async fn vm_prompt_deref(
    prompt_text: String,
    output_type: Option<&str>,
    grammar_override: Option<String>,
    grammar_anchor: Option<String>,
    grammar_stop: Option<String>,
    state: &mut VmState,
    span: Span,
) -> Result<VmValue> {
    // Cache check — skip inference entirely on a repeated (prompt, type) pair.
    let cache_key = (prompt_text.clone(), output_type.map(str::to_owned));
    if let Some(cached) = state.prompt_cache.get(&cache_key).cloned() {
        return match output_type {
            None => Ok(VmValue::Str(cached.into())),
            Some(type_name) => {
                let struct_defs = state.struct_defs.clone();
                let v = coerce(cached.trim(), type_name, &struct_defs).map_err(|_| {
                    JadeError::PromptOverflow { name: "<prompt>".to_string(), attempts: 1, span }
                })?;
                apply_struct_decorators(v, type_name, state, span).await
            }
        };
    }

    // Clone the Arc so we don't hold a borrow of state across .await points.
    let backend =
        state.inference_backend.as_ref().ok_or(JadeError::NoInferenceBackend { span })?.clone();

    // Grammar priority: user-supplied override > auto-generated from output type.
    let grammar = grammar_override.or_else(|| {
        output_type.and_then(|tn| crate::compiler::gbnf::grammar_for(tn, &state.struct_defs))
    });

    // Stateless call — no conversation history is sent or recorded.
    // Conversational memory is the JadeLang program's responsibility.
    let initial_resp = backend
        .infer(
            llm::InferenceRequest {
                prompt: prompt_text.clone(),
                grammar: grammar.clone(),
                anchor: grammar_anchor.clone(),
                stop_anchor: grammar_stop.clone(),
            },
            span,
        )
        .await?;

    let Some(type_name) = output_type else {
        state.prompt_cache.insert(cache_key, initial_resp.text.clone());
        return Ok(VmValue::Str(initial_resp.text.into()));
    };

    // Single-shot coercion. Grammar-constrained sampling already forces the reply
    // into a shape the target type accepts, so a failure here is a genuine
    // mismatch to surface rather than something to re-ask for. Any retry policy
    // belongs to the provider package, which is the layer that knows what a
    // retry costs.
    let struct_defs = state.struct_defs.clone();
    match coerce(initial_resp.text.trim(), type_name, &struct_defs) {
        Ok(v) => {
            state.prompt_cache.insert(cache_key, initial_resp.text);
            apply_struct_decorators(v, type_name, state, span).await
        }
        Err(_) => {
            Err(JadeError::PromptOverflow { name: "<prompt>".to_string(), attempts: 1, span })
        }
    }
}

/// Build the lazy stream an unconstrained or grammar-constrained `?p` produces.
///
/// A cache hit short-circuits to a `Str`; every drain path accepts either, so
/// callers do not branch on it.
///
/// The backend is checked *here* rather than at the drain, so a program with no
/// provider installed reports the error at the `?p` that needs one instead of
/// wherever the value happened to be consumed.
pub(crate) fn vm_prompt_deref_stream(
    prompt_text: String,
    grammar: Option<&jade_runtime::grammarf::GrammarObj>,
    state: &VmState,
    span: Span,
) -> Result<VmValue> {
    let cache_key = (prompt_text.clone(), None::<String>);
    if let Some(cached) = state.prompt_cache.get(&cache_key).cloned() {
        return Ok(VmValue::Str(cached.into()));
    }
    state.inference_backend.as_ref().ok_or(JadeError::NoInferenceBackend { span })?;
    Ok(VmValue::TokenStream(Arc::new(match grammar {
        Some(g) => JadeTokenStream::constrained(prompt_text, g),
        None => JadeTokenStream::lazy(prompt_text),
    })))
}

/// Drain a `TokenStream` silently into a `VmValue::Str`, updating the cache.
pub(crate) async fn vm_drain_token_stream(
    ts: Arc<JadeTokenStream>,
    state: &mut VmState,
    span: Span,
) -> Result<VmValue> {
    // Already drained? A stream is a buffer, so hand back what it holds.
    if let Some(t) = ts.text.lock().clone() {
        return Ok(VmValue::Str(t.into()));
    }
    start_inference(&ts, state, span).await?;
    let rx_opt = ts.rx.lock().take();
    let Some(mut rx) = rx_opt else {
        // Racing readers of the same stream: the other one is mid-drain. Its
        // result lands in `text`, which the check above will find next time.
        return Ok(VmValue::Str(String::new().into()));
    };
    let mut text = String::new();
    while let Some(token) = rx.recv().await {
        text.push_str(&token);
    }
    let h_opt = ts.tokens_handle.lock().take();
    if let Some(h) = h_opt {
        match h.await {
            Ok(Ok(())) => {}
            Ok(Err(e)) => return Err(e),
            Err(e) => {
                return Err(JadeError::AsyncPanic {
                    message: format!("token stream task panicked: {e}"),
                    span,
                });
            }
        }
    }
    state.prompt_cache.insert(ts.prompt_key.clone(), text.clone());
    *ts.text.lock() = Some(text.clone());
    Ok(VmValue::Str(text.into()))
}

/// Start the inference behind a lazy stream, applying the constraints the
/// stream carries. Idempotent: a stream whose inference already started has no
/// `lazy_prompt` left to take.
///
/// This is the single place a `?p` request is built. It used to exist three
/// times — once per drain path plus once in `stream()` — and the copies drifted:
/// one sent a Grammar's bare pattern where another sent the wrapped GBNF, so the
/// same Grammar constrained the model differently depending on how it was used.
async fn start_inference(ts: &Arc<JadeTokenStream>, state: &mut VmState, span: Span) -> Result<()> {
    let lazy = ts.lazy_prompt.lock().take();
    let Some(prompt_text) = lazy else { return Ok(()) };
    let backend =
        state.inference_backend.as_ref().ok_or(JadeError::NoInferenceBackend { span })?.clone();
    let (rx, handle) = backend
        .infer_stream(
            llm::InferenceRequest {
                prompt: prompt_text,
                grammar: ts.grammar.clone(),
                anchor: ts.region_start.first().cloned(),
                stop_anchor: ts.region_stop.first().cloned(),
            },
            span,
        )
        .await?;
    *ts.rx.lock() = Some(rx);
    *ts.tokens_handle.lock() = Some(handle);
    Ok(())
}

/// Find the earliest occurrence of any needle in `hay`, as `(offset, len)`.
pub(crate) fn first_match(needles: &[String], hay: &str) -> Option<(usize, usize)> {
    needles
        .iter()
        .filter_map(|n| hay.find(n.as_str()).map(|pos| (pos, n.len())))
        .min_by_key(|&(pos, _)| pos)
}

/// Whether `buf` is the beginning of some needle — i.e. it may yet grow into a
/// match, so nothing in it can be released yet.
///
/// This asks about the *whole* buffer, which is only meaningful because the
/// scan loop retreats one character at a time: text that cannot be part of a
/// needle is released character by character until what remains is either a
/// genuine partial match or empty.
pub(crate) fn is_partial_match(needles: &[String], buf: &str) -> bool {
    needles.iter().any(|n| n.starts_with(buf))
}

/// Drop the first character of `buf`, returning it.
pub(crate) fn pop_first_char(buf: &mut String) -> Option<char> {
    let c = buf.chars().next()?;
    buf.drain(..c.len_utf8());
    Some(c)
}

/// Core streaming-mute loop — separated from I/O so it can be tested directly.
///
/// Reads tokens from `rx`, writing non-muted bytes to `out`, and returns the
/// full accumulated text (printed + muted combined) so callers can inspect the
/// complete response.
///
/// Mute semantics (all use prefix-aware buffering):
///
///   `start_muted` — if true, suppression begins immediately from the first token.
///       Used when a Grammar has no `anchor` (the entire response is structured
///       output — suppress from generation start).
///
///   `region_start` — strings that enter muted mode on match. Once matched, ALL
///       subsequent tokens are suppressed until a `region_stop` match (or EOS).
///       Used when Grammar has an explicit `anchor` value.
///
///   `region_stop` — strings that exit muted mode on match (match itself is
///       suppressed). If empty, muting is permanent once entered.
///       Used for `stop_anchor` values.
///
/// While muted, partial `region_stop` prefixes at end-of-stream are discarded
/// (handles daemon stopping mid-token on stop_anchor detection).
pub(crate) async fn drain_tokens_with_mute<W: std::io::Write + Send>(
    rx: &mut tokio::sync::mpsc::Receiver<String>,
    start_muted: bool,
    region_start: &[String],
    region_stop: &[String],
    out: &mut W,
    newline: bool,
) -> String {
    let mut text = String::new();
    // A byte buffer, NOT a list of tokens. An anchor routinely straddles a token
    // boundary — a real model emits a few characters at a time, so `<tool_call>`
    // almost never arrives whole — and the scan has to be able to hold back the
    // tail of one token while releasing its head. This buffered by token and
    // released a whole token at a time, which discarded the start of any anchor
    // that shared a token with visible text, so muting silently did nothing.
    let mut pending = String::new();
    let mut muted = start_muted;

    while let Some(token) = rx.recv().await {
        text.push_str(&token);

        // Fast path: permanent mute (no stop anchor).
        if muted && region_stop.is_empty() {
            continue;
        }
        // Fast path: nothing to check while not muted.
        if !muted && region_start.is_empty() {
            let _ = out.write_all(token.as_bytes());
            let _ = out.flush();
            continue;
        }

        pending.push_str(&token);

        while !pending.is_empty() {
            if muted {
                // Scanning for region_stop to exit muted mode.
                if let Some((pos, len)) = first_match(region_stop, &pending) {
                    pending.drain(..pos + len); // the stop literal is suppressed too
                    muted = false;
                    continue;
                }
                // Might still grow into a stop anchor — hold and wait.
                if is_partial_match(region_stop, &pending) {
                    break;
                }
                pop_first_char(&mut pending); // still muted: discard
            } else {
                // Scanning for region_start to enter muted mode.
                if let Some((pos, len)) = first_match(region_start, &pending) {
                    if pos > 0 {
                        let _ = out.write_all(&pending.as_bytes()[..pos]);
                        let _ = out.flush();
                    }
                    pending.drain(..pos + len);
                    muted = true;
                    continue;
                }
                if is_partial_match(region_start, &pending) {
                    break;
                }
                if let Some(c) = pop_first_char(&mut pending) {
                    let mut b = [0u8; 4];
                    let _ = out.write_all(c.encode_utf8(&mut b).as_bytes());
                    let _ = out.flush();
                }
            }
        }
    }

    // End of stream.
    if muted {
        // Inside unclosed region — discard remaining (handles partial stop tags).
    } else {
        if !pending.is_empty() && !is_partial_match(region_start, &pending) {
            let _ = out.write_all(pending.as_bytes());
            let _ = out.flush();
        }
    }
    if newline {
        let _ = writeln!(out);
    }
    text
}

/// Drain a `TokenStream`, printing each token to stdout as it arrives.
/// Returns the accumulated text so `stream()` can return it as a `Str`.
///
/// `mute_patterns` lists the plain-text anchor strings extracted from Grammar
/// values passed via `mute_on=`. Tokens at or after the anchor are suppressed
/// from stdout; the full text is still returned for downstream parsing.
pub(crate) async fn vm_drain_token_stream_printing(
    ts: Arc<JadeTokenStream>,
    state: &mut VmState,
    span: Span,
    newline: bool,
    start_muted: bool,
    region_start: &[String],
    region_stop: &[String],
) -> Result<String> {
    // Already drained? Print what the buffer holds — with the muted span
    // suppressed, so printing a stream twice looks the same both times.
    if let Some(t) = ts.text.lock().clone() {
        let mut shown = apply_mute(&t, start_muted, region_start, region_stop);
        if newline {
            shown.push('\n');
        }
        // Routed the same way the live loop routes its output, so a test that
        // captures stdout sees a replayed stream too.
        #[cfg(test)]
        if let Some(buf) = &state.test_stdout {
            use std::io::Write;
            let _ = std::sync::Arc::clone(buf).lock().unwrap().write_all(shown.as_bytes());
            return Ok(t);
        }
        crate::stdio::write_str_flush(&shown);
        return Ok(t);
    }
    start_inference(&ts, state, span).await?;
    let rx_opt = ts.rx.lock().take();
    let Some(mut rx) = rx_opt else {
        return Ok(String::new());
    };

    #[cfg(test)]
    let text = if let Some(buf) = &state.test_stdout {
        let mut w = TestWriter(std::sync::Arc::clone(buf));
        drain_tokens_with_mute(&mut rx, start_muted, region_start, region_stop, &mut w, newline)
            .await
    } else {
        drain_tokens_with_mute(
            &mut rx,
            start_muted,
            region_start,
            region_stop,
            &mut std::io::stdout(),
            newline,
        )
        .await
    };
    #[cfg(not(test))]
    let text = drain_tokens_with_mute(
        &mut rx,
        start_muted,
        region_start,
        region_stop,
        &mut std::io::stdout(),
        newline,
    )
    .await;

    let h_opt = ts.tokens_handle.lock().take();
    if let Some(h) = h_opt {
        match h.await {
            Ok(Ok(())) => {}
            Ok(Err(e)) => return Err(e),
            Err(e) => {
                return Err(JadeError::AsyncPanic {
                    message: format!("token stream task panicked: {e}"),
                    span,
                });
            }
        }
    }
    state.prompt_cache.insert(ts.prompt_key.clone(), text.clone());
    *ts.text.lock() = Some(text.clone());
    Ok(text)
}

/// Apply a mute spec to already-complete text.
///
/// The live path suppresses tokens as they arrive; this is the same rule over a
/// buffer, for a stream that was already drained. Both have to agree, or
/// printing a stream twice would show different things.
pub(crate) fn apply_mute(
    text: &str,
    start_muted: bool,
    region_start: &[String],
    region_stop: &[String],
) -> String {
    if !start_muted && region_start.is_empty() {
        return text.to_string();
    }
    let mut out = String::new();
    let mut rest = text;
    let mut muted = start_muted;
    loop {
        let needles: &[String] = if muted { region_stop } else { region_start };
        match first_match(needles, rest) {
            Some((pos, len)) => {
                if !muted {
                    out.push_str(&rest[..pos]);
                }
                // The matching anchor is suppressed either way, matching the
                // live loop: an anchor is a marker, not content.
                rest = &rest[pos + len..];
                muted = !muted;
            }
            None => {
                if !muted {
                    out.push_str(rest);
                }
                return out;
            }
        }
    }
}

/// Drain a `TokenStream` to `Str` if the value is one; pass everything else through.
pub(crate) async fn vm_maybe_drain(v: VmValue, state: &mut VmState, span: Span) -> Result<VmValue> {
    match v {
        VmValue::TokenStream(ts) => vm_drain_token_stream(ts, state, span).await,
        other => Ok(other),
    }
}
