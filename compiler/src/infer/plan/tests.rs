use super::*;
use serde_json::json;

/// Purpose: This function builds a tiny plan: token embeddings, a norm and the
/// logits, with the inputs a step fills and a three-token tokenizer.
fn plan(ops: serde_json::Value, extra_entries: Vec<serde_json::Value>) -> serde_json::Value {
    let entry =
        |name: &str, kind: &str, ty: i64, dims: serde_json::Value, region: i64, offset: i64| {
            json!({"tensor": name, "role": {"kind": kind}, "ggml_type": ty, "dims": dims,
               "region": region, "offset": offset})
        };
    let mut entries = vec![
        entry("token_embd.weight", "weight", 0, json!([4, 3]), 0, 0),
        entry("inp_tokens", "input", 26, json!([8]), 1, 0),
        entry("inp_pos", "input", 26, json!([8]), 1, 64),
        entry("inp_k_idxs", "input", 27, json!([8]), 1, 128),
        entry("inp_v_idxs", "input", 27, json!([8]), 1, 256),
        entry("attn_inp_kq_mask", "input", 1, json!([16, 8]), 1, 512),
        entry("inp_out_ids", "input", 26, json!([1]), 1, 1024),
        entry("inpL", "scratch", 0, json!([4, 8]), 1, 2048),
        entry("result_output", "output", 0, json!([3]), 1, 4096),
    ];
    entries.extend(extra_entries);
    let definition = json!({
        "normalizer": {"type": "NFC"},
        "pre_tokenizer": {"type": "Sequence", "pretokenizers": [
            {"type": "Split", "pattern": {"Regex": "(?i:'s|'t|'re|'ve|'m|'ll|'d)|[^\\r\\n\\p{L}\\p{N}]?\\p{L}+|\\p{N}| ?[^\\s\\p{L}\\p{N}]+[\\r\\n]*|\\s*[\\r\\n]+|\\s+(?!\\S)|\\s+"}},
            {"type": "ByteLevel", "use_regex": false}]},
        "model": {"vocab": {"a": 0, "b": 1, "ab": 2}, "merges": ["a b"]},
        "added_tokens": [{"id": 3, "content": "<|endoftext|>"}],
    });
    json!({
        "table_hash": "abc",
        "regions": [{"id": 0}, {"id": 1}],
        "entries": entries,
        "step": {"options": {"n_tokens": 8, "n_ctx": 16, "n_outputs": 1},
                 "outputs": ["result_output"], "ops": ops},
        "tokenizer": {"kind": "gpt2", "stop": [2], "definition": definition.to_string()},
    })
}

#[test]
fn a_plan_becomes_op_records_and_tables() {
    let p = plan(
        json!([{"op": "get_rows", "inputs": ["token_embd.weight", "inp_tokens"], "outputs": ["inpL"], "params": {}},
               {"op": "reshape", "inputs": ["inpL"], "outputs": ["inpL"], "params": {}}]),
        Vec::new(),
    );
    let r = read("m", &p).unwrap();
    assert_eq!(r.ops.len(), 1, "a reshape moves nothing, so it launches nothing");
    let rec = &r.ops[0];
    assert_eq!(rec[0], 0);
    // The embeddings' second dimension is the vocabulary, not tokens.
    assert_eq!(rec[1 + 11], -1);
    // The destination's tokens are its outer dimension.
    assert_eq!(rec[1 + 4 * 12 + 11], 1);
    assert_eq!(r.tokens, Addr { region: 1, offset: 0 });
    assert_eq!(r.n_vocab, 3);
    assert_eq!(r.pieces, "a\nb\nab\n<|endoftext|>\n");
    assert_eq!(r.merges, "a b\n");
    assert_eq!(r.added, "3 <|endoftext|>\n");
    assert_eq!(r.split, 1);
    assert_eq!(r.stops, vec![2, 3], "<|endoftext|> ends a raw completion too");
}

#[test]
fn what_the_kernels_cannot_run_is_named() {
    let p = plan(
        json!([{"op": "ssm_conv", "inputs": ["inpL"], "outputs": ["inpL"], "params": {}}]),
        Vec::new(),
    );
    assert!(read("m", &p).unwrap_err().contains("ssm_conv"));

    let rot = json!({"tensor": "attn_inp_k_rot", "role": {"kind": "input"}, "ggml_type": 0,
                     "dims": [64, 64], "region": 1, "offset": 8192});
    let p = plan(json!([]), vec![rot]);
    assert!(read("m", &p).unwrap_err().contains("attn_inp_k_rot"));
}

#[test]
fn rope_parameters_ride_in_the_record() {
    let pos_rope = json!([{"op": "rope", "inputs": ["inpL", "inp_pos"], "outputs": ["inpL"],
        "params": {"freq_base": 10000.0, "freq_scale": 1.0, "attn_factor": 1.0, "ext_factor": 0.0,
                   "n_dims": 4, "mode": 2}}]);
    let r = read("m", &plan(pos_rope, Vec::new())).unwrap();
    let rec = &r.ops[0];
    assert_eq!(f32::from_bits((rec[61] & 0xffff_ffff) as u32), 10000.0);
    assert_eq!(rec[65], 4);
    assert_eq!(rec[66], 2);
}

#[test]
fn row_kernels_give_each_output_a_simd_group() {
    let p = plan(
        json!([{"op": "rms_norm", "inputs": ["inpL"], "outputs": ["inpL"], "params": {"eps": 1e-6}},
               {"op": "mul_mat", "inputs": ["token_embd.weight", "inpL"], "outputs": ["inpL"], "params": {}},
               {"op": "add", "inputs": ["inpL", "inpL"], "outputs": ["inpL"], "params": {}}]),
        Vec::new(),
    );
    let r = read("m", &p).unwrap();
    let widths: Vec<i64> = r.ops.iter().map(|rec| rec[74]).collect();
    assert_eq!(widths, vec![threads::GROUP, threads::GROUP, 1]);
    // A mul_mat over many tokens runs as tiles, through a kernel the step picks.
    assert_eq!(r.ops[1][73], threads::MAT);
    assert_eq!(KERNELS[MUL_MM as usize], "k_mul_mm");
}

#[test]
fn a_mul_mat_by_quantized_values_is_refused() {
    let q = json!({"tensor": "blk.0.q", "role": {"kind": "weight"}, "ggml_type": 8,
                   "dims": [32, 4], "region": 0, "offset": 64});
    let p = plan(
        json!([{"op": "mul_mat", "inputs": ["token_embd.weight", "blk.0.q"], "outputs": ["inpL"], "params": {}}]),
        vec![q],
    );
    let e = read("m", &p).unwrap_err();
    assert!(e.contains("mul_mat inpL") && e.contains("blk.0.q"), "{e}");
}

#[test]
fn attention_over_a_quantized_cache_is_refused() {
    let tensor = |name: &str, ty: i64, dims: serde_json::Value, offset: i64| {
        json!({"tensor": name, "role": {"kind": "kv"}, "ggml_type": ty, "dims": dims,
               "region": 1, "offset": offset})
    };
    let entries = vec![
        tensor("q", 0, json!([64, 8, 2]), 8192),
        tensor("k", 8, json!([64, 16, 2]), 16384),
        tensor("v", 1, json!([64, 16, 2]), 24576),
    ];
    let p = plan(
        json!([{"op": "flash_attn_ext", "inputs": ["q", "k", "v", "attn_inp_kq_mask"],
                "outputs": ["inpL"], "params": {"scale": 0.125}}]),
        entries,
    );
    let e = read("m", &p).unwrap_err();
    assert!(e.contains("KV cache of ggml type 8") && e.contains("f16 cache"), "{e}");
}
