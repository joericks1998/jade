//! Asking the dovata process for a model's plan, at build time.
//!
//! The dovata process holds models in memory, one process per user. Its socket
//! is `$DOVATA_RUN_DIR/dovata.sock`, or `~/.dovata/run/dovata.sock`, and each
//! request is a new connection carrying one line of JSON, answered with one
//! line, as in `{"protocol":2,"op":"plan","name":"qwen3-0.6b"}`. The compiler
//! needs two answers: which models the process holds, and one model's plan.
//! The compiled program speaks the same protocol to map the model's memory;
//! `src/backend/infer.rs` has that half.
//!
//! The protocol is dovata's own. This file and the program's half speak
//! version 2, which dovata 3.1 and 3.2 answer; another version is refused by
//! both sides, so a mismatch says so rather than misreading a reply.

use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::time::Duration;

/// The version of dovata's messages this file speaks.
pub const PROTOCOL: u32 = 2;

/// Purpose: This function gives the path of the dovata process's socket.
///
/// Inputs: none.
///
/// Outputs: `$DOVATA_RUN_DIR/dovata.sock`, or `~/.dovata/run/dovata.sock`.
pub fn socket_path() -> PathBuf {
    let dir = match std::env::var_os("DOVATA_RUN_DIR") {
        Some(d) if !d.is_empty() => PathBuf::from(d),
        _ => PathBuf::from(std::env::var_os("HOME").unwrap_or_default()).join(".dovata/run"),
    };
    dir.join("dovata.sock")
}

/// The advice every failure to reach dovata ends with.
const START: &str = "a program that dereferences a prompt runs a model the dovata process \
                     holds; install it with `curl -fsSL \
                     https://raw.githubusercontent.com/joericks1998/jade/main/dovata/install.sh \
                     | sh`, start it with `dovata start`, and add a model with `dovata add \
                     <model>`";

/// Purpose: This function sends one request to the dovata process and reads
/// its answer.
///
/// Inputs:
/// - `op`: the request's fields besides `protocol`, such as `{"op": "status"}`.
///
/// Outputs: the reply's JSON, or a message saying why there is none, including
/// the process's own refusal.
pub fn ask(op: serde_json::Value) -> Result<serde_json::Value, String> {
    let path = socket_path();
    let sock = UnixStream::connect(&path)
        .map_err(|e| format!("cannot reach dovata at {} ({e}); {START}", path.display()))?;
    let _ = sock.set_read_timeout(Some(Duration::from_secs(60)));
    let mut request = op;
    request["protocol"] = PROTOCOL.into();
    let mut line = serde_json::to_string(&request).map_err(|e| e.to_string())?;
    line.push('\n');
    (&sock).write_all(line.as_bytes()).map_err(|e| format!("dovata: {e}"))?;
    let mut answer = String::new();
    BufReader::new(&sock).read_line(&mut answer).map_err(|e| format!("dovata: {e}"))?;
    let reply: serde_json::Value = serde_json::from_str(&answer)
        .map_err(|e| format!("dovata answered something that is not JSON ({e})"))?;
    if let Some(why) = reply.get("error").and_then(|e| e.as_str()) {
        return Err(format!("dovata: {why}"));
    }
    Ok(reply)
}

/// Purpose: This function asks dovata for a model's plan.
///
/// Inputs:
/// - `name`: the model to use, from `jade.toml`'s `[model]`, or `None` for the
///   one model the process holds.
///
/// Outputs: the model's name and its plan's JSON, or a message saying which
/// models there are to choose from.
pub fn model_plan(name: Option<&str>) -> Result<(String, serde_json::Value), String> {
    let name = match name {
        Some(n) => n.to_string(),
        None => {
            let status = ask(serde_json::json!({ "op": "status" }))?;
            let models: Vec<String> = status["status"]["models"]
                .as_array()
                .map(|ms| {
                    ms.iter().filter_map(|m| m["name"].as_str().map(str::to_string)).collect()
                })
                .unwrap_or_default();
            match models.as_slice() {
                [one] => one.clone(),
                [] => return Err(format!("dovata holds no model; {START}")),
                many => {
                    return Err(format!(
                        "dovata holds {} models, {}; name the one this program runs in \
                         jade.toml:\n    [model]\n    name = \"{}\"",
                        many.len(),
                        many.join(", "),
                        many[0]
                    ));
                }
            }
        }
    };
    let reply = ask(serde_json::json!({ "op": "plan", "name": name }))?;
    let plan = reply.get("plan").cloned().ok_or("dovata's answer to plan holds no plan")?;
    Ok((name, plan))
}
