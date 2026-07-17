use std::collections::HashMap;

use crate::{
    compiler::{tir::JadeType, type_infer::TypeContext, vm::VmValue},
    frontend::error::{JadeError, Result, Span},
};

use crate::builtins::{BuiltinFn, Package};

const ZERO: Span = Span { line: 0, col: 0 };

fn http_err(detail: &str) -> JadeError {
    JadeError::IoError { message: format!("http: {}", detail), span: ZERO }
}

fn require_str_owned(args: &[VmValue], pos: usize, fn_name: &str) -> Result<String> {
    match args.get(pos) {
        Some(VmValue::Str(s)) => Ok(s.clone()),
        Some(_) => Err(JadeError::TypeError { message: fn_name.to_string(), span: ZERO }),
        None    => Err(JadeError::ArityMismatch { expected: pos + 1, got: args.len(), span: ZERO }),
    }
}

fn extract_headers(val: Option<&VmValue>) -> Result<Vec<(String, String)>> {
    match val {
        None | Some(VmValue::Nil) => Ok(vec![]),
        Some(VmValue::Dict(map)) => {
            let mut headers = Vec::new();
            for (k, v) in map {
                match v {
                    VmValue::Str(s) => headers.push((k.clone(), s.clone())),
                    _ => return Err(JadeError::TypeError {
                        message: "http header value must be str".to_string(),
                        span: ZERO,
                    }),
                }
            }
            Ok(headers)
        }
        Some(_) => Err(JadeError::TypeError { message: "http headers must be a dict".to_string(), span: ZERO }),
    }
}

fn make_response(status: u16, body: String) -> VmValue {
    let mut map = HashMap::new();
    map.insert("status".to_string(), VmValue::Int(status as i64));
    map.insert("body".to_string(), VmValue::Str(body));
    VmValue::Dict(map)
}

enum HttpMethod {
    Get,
    Post(String),
    Put(String),
    Delete,
    Head,
}

// Runs the request on a fresh OS thread to avoid nesting inside tokio runtimes.
fn execute(url: String, method: HttpMethod, headers: Vec<(String, String)>) -> Result<VmValue> {
    match std::thread::spawn(move || -> std::result::Result<(u16, String), String> {
        let client = reqwest::blocking::Client::builder()
            .timeout(std::time::Duration::from_secs(30))
            .build()
            .map_err(|e| e.to_string())?;

        let rb = match method {
            HttpMethod::Get        => client.get(&url),
            HttpMethod::Post(body) => client.post(&url).body(body),
            HttpMethod::Put(body)  => client.put(&url).body(body),
            HttpMethod::Delete     => client.delete(&url),
            HttpMethod::Head       => client.head(&url),
        };

        let rb = headers.iter().try_fold(rb, |rb, (k, v)| {
            let name = reqwest::header::HeaderName::from_bytes(k.as_bytes())
                .map_err(|e| e.to_string())?;
            let value = reqwest::header::HeaderValue::from_str(v)
                .map_err(|e| e.to_string())?;
            Ok::<reqwest::blocking::RequestBuilder, String>(rb.header(name, value))
        })?;

        let resp = rb.send().map_err(|e| e.to_string())?;
        let status = resp.status().as_u16();
        let body = resp.text().unwrap_or_default();
        Ok((status, body))
    })
    .join() {
        Ok(Ok((status, body))) => Ok(make_response(status, body)),
        Ok(Err(e))             => Err(http_err(&e)),
        Err(_)                 => Err(http_err("request thread panicked")),
    }
}

fn http_get(args: &[VmValue]) -> Result<VmValue> {
    if args.is_empty() || args.len() > 2 {
        return Err(JadeError::ArityMismatch { expected: 1, got: args.len(), span: ZERO });
    }
    let url = require_str_owned(args, 0, "http.get")?;
    let headers = extract_headers(args.get(1))?;
    execute(url, HttpMethod::Get, headers)
}

fn http_post(args: &[VmValue]) -> Result<VmValue> {
    if args.len() < 2 || args.len() > 3 {
        return Err(JadeError::ArityMismatch { expected: 2, got: args.len(), span: ZERO });
    }
    let url  = require_str_owned(args, 0, "http.post")?;
    let body = require_str_owned(args, 1, "http.post")?;
    let headers = extract_headers(args.get(2))?;
    execute(url, HttpMethod::Post(body), headers)
}

fn http_put(args: &[VmValue]) -> Result<VmValue> {
    if args.len() < 2 || args.len() > 3 {
        return Err(JadeError::ArityMismatch { expected: 2, got: args.len(), span: ZERO });
    }
    let url  = require_str_owned(args, 0, "http.put")?;
    let body = require_str_owned(args, 1, "http.put")?;
    let headers = extract_headers(args.get(2))?;
    execute(url, HttpMethod::Put(body), headers)
}

fn http_delete(args: &[VmValue]) -> Result<VmValue> {
    if args.is_empty() || args.len() > 2 {
        return Err(JadeError::ArityMismatch { expected: 1, got: args.len(), span: ZERO });
    }
    let url = require_str_owned(args, 0, "http.delete")?;
    let headers = extract_headers(args.get(1))?;
    execute(url, HttpMethod::Delete, headers)
}

fn http_head(args: &[VmValue]) -> Result<VmValue> {
    if args.is_empty() || args.len() > 2 {
        return Err(JadeError::ArityMismatch { expected: 1, got: args.len(), span: ZERO });
    }
    let url = require_str_owned(args, 0, "http.head")?;
    let headers = extract_headers(args.get(1))?;
    execute(url, HttpMethod::Head, headers)
}

static HTTP_PKG_FNS: &[BuiltinFn] = &[
    BuiltinFn { name: "get",    vm_impl: http_get },
    BuiltinFn { name: "post",   vm_impl: http_post },
    BuiltinFn { name: "put",    vm_impl: http_put },
    BuiltinFn { name: "delete", vm_impl: http_delete },
    BuiltinFn { name: "head",   vm_impl: http_head },
];

fn register_http_pkg_types(ctx: &mut TypeContext) {
    ctx.define("http".to_string(), JadeType::Unknown);
}

pub static HTTP_PKG: Package = Package {
    import_name: "std/http",
    global_name: "http",
    fns: HTTP_PKG_FNS,
    register_types: register_http_pkg_types,
};

#[cfg(test)]
mod tests;
