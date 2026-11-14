//! Checks against a real model, compared with llama.cpp.
//!
//! These build programs that run the model the dovata process holds, and
//! compare what they print with llama.cpp's own tools run on the same GGUF
//! file. They need a Mac with Metal, a running dovata process holding one
//! model, and `llama-tokenize` and `llama-simple` on `PATH`; anywhere else
//! they say what is missing and pass, the way dovata's quantize tests skip
//! without `llama-quantize`.

use std::path::{Path, PathBuf};
use std::process::Command;

/// Purpose: This function finds what the checks need, or says what is missing.
///
/// Outputs: the model file dovata holds, or `None` after printing why the
/// checks are skipped.
fn model_file() -> Option<PathBuf> {
    if !cfg!(all(target_os = "macos", target_arch = "aarch64")) {
        eprintln!("skipped: inference runs on Apple Silicon only so far");
        return None;
    }
    for tool in ["llama-tokenize", "llama-simple"] {
        if Command::new(tool).arg("--help").output().is_err() {
            eprintln!("skipped: {tool} is not installed (brew install llama.cpp)");
            return None;
        }
    }
    let models = match dovata::host::connect().and_then(|p| p.models()) {
        Ok(m) => m,
        Err(e) => {
            eprintln!("skipped: {e}");
            return None;
        }
    };
    if models.len() != 1 {
        eprintln!(
            "skipped: dovata holds {} models, and these checks use exactly one",
            models.len()
        );
        return None;
    }
    Some(models[0].files.first()?.path.clone())
}

/// Purpose: This function builds a program and runs it.
///
/// Inputs:
/// - `tag`: names the scratch directory.
/// - `src`: the program.
///
/// Outputs: what it printed.
fn run(tag: &str, src: &str) -> String {
    let dir = std::env::temp_dir().join(format!("jade_infer_{tag}_{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("main.jde");
    std::fs::write(&file, src).unwrap();
    let tokens = crate::frontend::lexer::tokenize(src).unwrap();
    let ast = crate::frontend::parser::parse(tokens).unwrap();
    let tp = crate::typechecker::type_infer::infer(ast).unwrap();
    let bin = dir.join("main");
    crate::aot::compile(tp, Some(&file), &bin, crate::aot::Emit::Binary).unwrap();
    let out = Command::new(&bin).output().unwrap();
    let _ = std::fs::remove_dir_all(&dir);
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    String::from_utf8_lossy(&out.stdout).into_owned()
}

/// Purpose: This function asks llama.cpp for a text's token ids.
fn llama_ids(model: &Path, text: &str) -> String {
    let out = Command::new("llama-tokenize")
        .arg("-m")
        .arg(model)
        .args(["--ids", "--log-disable", "--prompt", text])
        .output()
        .unwrap();
    String::from_utf8_lossy(&out.stdout).lines().last().unwrap_or("").trim().to_string()
}

#[test]
fn the_prelude_tokenizes_as_llama_cpp_does() {
    let Some(model) = model_file() else { return };
    let texts = [
        "Hello, world! It's 2025: naïve café 中文 123456 x",
        "  two  spaces\n\nnew lines\tand tabs <|im_start|>user\nhi<|im_end|>",
        "I'VE seen 3.14159 and   -42, ok?!  ",
    ];
    // The prelude's own functions, called by name: its source pasted in front.
    let mut src = super::rewrite::PRELUDE.to_string();
    for t in texts {
        let lit = serde_json::to_string(t).unwrap();
        src.push_str(&format!("\nprint(encode({lit}))"));
    }
    src.push_str("\nprint(decode(encode(\"round trip: héllo 🚀 ok\")))\n");
    let got = run("tokenize", &src);
    let lines: Vec<&str> = got.lines().collect();
    for (i, t) in texts.iter().enumerate() {
        assert_eq!(lines[i], llama_ids(&model, t), "tokenizing {t:?}");
    }
    assert_eq!(lines[texts.len()], "round trip: héllo 🚀 ok");
}

#[test]
fn a_reply_matches_llama_cpps_greedy_one() {
    let Some(model) = model_file() else { return };
    let prompt = "The capital of France is";
    let got = run("generate", &format!("set_max_tokens(16)\nprompt p = \"{prompt}\"\nprint(?p)\n"));
    let out = Command::new("llama-simple")
        .arg("-m")
        .arg(&model)
        .args(["-n", "16", prompt])
        .output()
        .unwrap();
    let theirs = String::from_utf8_lossy(&out.stdout);
    let theirs = theirs.trim_start().strip_prefix(prompt).unwrap_or(&theirs).trim_end();
    assert_eq!(got.trim_end(), theirs);
}
