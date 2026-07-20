use jade_runtime::coll::DictObj;

use crate::{
    compiler::{tir::JadeType, type_infer::TypeContext, vm::VmValue},
    frontend::error::{JadeError, Result, Span},
};

use crate::builtins::{BuiltinFn, Package};

const ZERO: Span = Span { line: 0, col: 0 };

fn require_str_owned(args: &[VmValue], pos: usize, fn_name: &str) -> Result<String> {
    match args.get(pos) {
        Some(VmValue::Str(s)) => Ok(s.to_string()),
        Some(_) => Err(JadeError::TypeError { message: fn_name.to_string(), span: ZERO }),
        None    => Err(JadeError::ArityMismatch { expected: pos + 1, got: args.len(), span: ZERO }),
    }
}

fn extract_headers(val: Option<&VmValue>) -> Result<Vec<(String, String)>> {
    match val {
        None | Some(VmValue::Nil) => Ok(vec![]),
        Some(VmValue::Dict(map)) => {
            let mut headers = Vec::new();
            for (k, v) in map.iter() {
                match v {
                    VmValue::Str(s) => headers.push((k.clone(), s.to_string())),
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
    let mut map = DictObj::new();
    map.insert("status".to_string(), VmValue::Int(status as i64));
    map.insert("body".to_string(), VmValue::Str(body.into()));
    VmValue::Dict(map)
}

enum HttpMethod {
    Get,
    Post(String),
    Put(String),
    Delete,
    Head,
}

// Shares the curl-subprocess core with the AOT backend (jade_runtime::httpf).
fn execute(url: String, method: HttpMethod, headers: Vec<(String, String)>) -> Result<VmValue> {
    let (m, body): (&str, Option<String>) = match method {
        HttpMethod::Get        => ("GET", None),
        HttpMethod::Post(body) => ("POST", Some(body)),
        HttpMethod::Put(body)  => ("PUT", Some(body)),
        HttpMethod::Delete     => ("DELETE", None),
        HttpMethod::Head       => ("HEAD", None),
    };
    jade_runtime::httpf::request(m, &url, body.as_deref(), &headers)
        .map(|(status, body)| make_response(status as u16, body))
        .map_err(|message| JadeError::IoError { message, span: ZERO })
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
