//! Exports a `HashMap`, which has no Jade form. rustc refuses it with the message
//! on jade-rs's `Type` trait, which lists the types that do cross.

use std::collections::HashMap;

#[jade::export]
pub fn counts(words: Vec<String>) -> HashMap<String, i64> {
    let mut out = HashMap::new();
    for w in words {
        *out.entry(w).or_insert(0) += 1;
    }
    out
}
