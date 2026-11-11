//! The model's plan, read into what the backend writes into the program.
//!
//! dovata's plan says where every tensor sits, as a region and a byte offset;
//! which ggml ops make one forward step, in order; and how the model tokenizes
//! text. This file checks the plan describes a model these kernels can run, and
//! turns it into an [`InferPlan`]: one fixed-size record per op that launches a
//! kernel, the addresses of the inputs each step fills, and the tokenizer's
//! tables as text.
//!
//! *The step size.* The plan places every short-lived tensor for a step of
//! `n_tokens` tokens. A step may run fewer: the program fills only the first
//! rows, and each kernel is told the count it runs. Every tensor that grows with
//! the step keeps its tokens outermost in memory, so the first rows of a tensor
//! planned for 512 tokens are that tensor for fewer. A record marks which
//! dimension of each tensor is the token count, the one equal to `n_tokens`
//! with the largest stride, and the program puts the real count there.

use std::collections::{BTreeMap, HashMap};

/// The words in one op record. See [`InferPlan::ops`].
pub const OP_WORDS: usize = 80;

/// The kernels in `kernels.metal`, by the number an op record names.
pub const KERNELS: &[&str] = &[
    "k_get_rows",
    "k_rms_norm",
    "k_mul",
    "k_add",
    "k_mul_mat",
    "k_rope",
    "k_cpy",
    "k_set_rows",
    "k_swiglu",
    "k_flash_attn",
    "k_mul_mm",
];

/// The kernel a `mul_mat` runs in a step of more than one token, by its place
/// in [`KERNELS`]. No op record names it; the step picks it.
pub const MUL_MM: i64 = 10;

/// How many threads an op runs, by what it computes one of. The number is
/// word 73 of its record.
pub mod threads {
    /// One per value of the destination.
    pub const DST: i64 = 0;
    /// One per row of the destination.
    pub const DST_ROWS: i64 = 1;
    /// One per value of the second source, which `set_rows` scatters.
    pub const SRC1: i64 = 2;
    /// One per query token and head, for attention.
    pub const ATTENTION: i64 = 3;
    /// For `mul_mat`: one per value of the destination, as [`DST`], in a step
    /// of one token. In a step of more, the step runs [`super::MUL_MM`]
    /// instead, with one per tile of [`TILE`] rows by [`TILE`] tokens, and
    /// [`TILE_THREADS`] threads each.
    pub const MAT: i64 = 4;
    /// The rows of weights and the tokens in one `k_mul_mm` tile. `TILE` in
    /// `kernels.metal` is the same number.
    pub const TILE: i64 = 32;
    /// The threads that compute one `k_mul_mm` tile: two SIMD groups, one
    /// threadgroup.
    pub const TILE_THREADS: i64 = 64;
    /// The threads that share each of those in `mul_mat`, `rms_norm` and
    /// `flash_attn_ext`, which read a whole row for each: one SIMD group. Word
    /// 74 of a record is this or 1.
    pub const GROUP: i64 = 32;
}

/// Purpose: This struct holds where a tensor is: a region and a byte offset.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Addr {
    pub region: i64,
    pub offset: i64,
}

/// Purpose: This struct holds everything about a model the backend writes into
/// a program that runs it.
#[derive(Debug, Clone)]
pub struct InferPlan {
    /// The model's name in the dovata process.
    pub model: String,
    /// The plan's table hash, which the program sends when it maps the model,
    /// so dovata refuses memory these offsets do not fit.
    pub table_hash: String,
    /// How many regions the program maps.
    pub regions: usize,
    /// The most tokens one step runs.
    pub n_tokens: i64,
    /// The KV cache's length: the most tokens a prompt and its reply hold.
    pub n_ctx: i64,
    /// One record per op that launches a kernel, in run order. Word 0 is the
    /// kernel, by its place in [`KERNELS`]. Words 1 to 60 are five tensors,
    /// the four sources then the destination, twelve words each: region (-1
    /// for none), byte offset, `ne` four times, `nb` four times, ggml type, and
    /// the dimension that counts tokens (-1 for none). Words 61 to 64 are
    /// eight `f32` parameters, two to a word, low half first; words 65 to 72
    /// are eight integer parameters, of which the step sets the last two; word
    /// 73 says what the kernel computes one of, from [`threads`], and word 74
    /// how many threads share each one.
    pub ops: Vec<[i64; OP_WORDS]>,
    /// The step's inputs: token ids (i32), positions (i32), the KV cells each
    /// token is written to (i64, for K and for V), the attention mask (f16,
    /// `[n_ctx, n_tokens]`), and which row's logits come back (i32).
    pub tokens: Addr,
    pub positions: Addr,
    pub k_cells: Addr,
    pub v_cells: Addr,
    pub mask: Addr,
    pub out_ids: Addr,
    /// The logits of the last token, `n_vocab` f32 values.
    pub logits: Addr,
    pub n_vocab: i64,
    /// The token ids that end the reply.
    pub stops: Vec<i64>,
    /// The token pieces in id order, one per line.
    pub pieces: String,
    /// The merges in rank order, `a b` per line.
    pub merges: String,
    /// The added tokens, `id text` per line.
    pub added: String,
    /// Which pre-tokenizer splits text: 1 Qwen 2's, 2 Llama 3's, 3 GPT-2's.
    pub split: i64,
}

/// Purpose: This function gives a ggml type's block: how many values, and how
/// many bytes.
///
/// Inputs:
/// - `t`: the ggml type number.
///
/// Outputs: the block, or `None` for a type the kernels cannot read.
pub fn block(t: i64) -> Option<(i64, i64)> {
    Some(match t {
        0 => (1, 4),      // f32
        1 => (1, 2),      // f16
        30 => (1, 2),     // bf16
        26 => (1, 4),     // i32
        27 => (1, 8),     // i64
        8 => (32, 34),    // q8_0
        2 => (32, 18),    // q4_0
        12 => (256, 144), // q4_K
        13 => (256, 176), // q5_K
        14 => (256, 210), // q6_K
        _ => return None,
    })
}

/// Purpose: This struct holds one tensor as a kernel sees it.
#[derive(Debug, Clone, Copy)]
struct Tensor {
    addr: Addr,
    ne: [i64; 4],
    nb: [i64; 4],
    ty: i64,
    token_axis: i64,
}

/// Purpose: This function reads a plan into what the backend writes.
///
/// Inputs:
/// - `model`: the model's name in dovata.
/// - `plan`: the plan's JSON, as dovata gives it.
///
/// Outputs: the plan, or a message naming what these kernels cannot run.
pub fn read(model: &str, plan: &serde_json::Value) -> Result<InferPlan, String> {
    let fail = |m: String| format!("model {model}: {m}");
    let step = plan.get("step").ok_or_else(|| fail("the plan has no step".into()))?;
    let opts = &step["options"];
    let n_tokens = opts["n_tokens"].as_i64().unwrap_or(0);
    let n_ctx = opts["n_ctx"].as_i64().unwrap_or(0);
    if opts["n_outputs"].as_i64() != Some(1) {
        return Err(fail(format!(
            "the plan returns logits for {} tokens a step, and a program reads one; add the \
             model to dovata with one output",
            opts["n_outputs"]
        )));
    }
    let entries: HashMap<&str, &serde_json::Value> = plan["entries"]
        .as_array()
        .ok_or_else(|| fail("the plan has no address table".into()))?
        .iter()
        .filter_map(|e| Some((e["tensor"].as_str()?, e)))
        .collect();
    let regions = plan["regions"].as_array().map_or(0, Vec::len);

    let tensor = |name: &str| -> Result<Tensor, String> {
        let e =
            entries.get(name).ok_or_else(|| fail(format!("tensor {name} is not in the plan")))?;
        let ty = e["ggml_type"].as_i64().unwrap_or(-1);
        let (blk, bytes) = block(ty).ok_or_else(|| {
            fail(format!("tensor {name} has ggml type {ty}, which these kernels do not read"))
        })?;
        let dims: Vec<i64> = e["dims"]
            .as_array()
            .map(|d| d.iter().filter_map(|x| x.as_i64()).collect())
            .unwrap_or_default();
        let mut ne = [1i64; 4];
        for (i, d) in dims.iter().take(4).enumerate() {
            ne[i] = *d;
        }
        let role = &e["role"];
        let nb = match role["nb"].as_array() {
            Some(v) if role["kind"] == "view" => {
                let mut nb = [0i64; 4];
                for (i, x) in v.iter().take(4).enumerate() {
                    nb[i] = x.as_i64().unwrap_or(0);
                }
                nb
            }
            _ => {
                let row = ne[0] / blk * bytes;
                [bytes, row, row * ne[1], row * ne[1] * ne[2]]
            }
        };
        let kind = role["kind"].as_str().unwrap_or("");
        let grows = !matches!(kind, "weight" | "kv") && n_tokens > 1;
        let token_axis = if grows {
            (0..4).filter(|&i| ne[i] == n_tokens).max_by_key(|&i| nb[i]).map_or(-1, |i| i as i64)
        } else {
            -1
        };
        Ok(Tensor {
            addr: Addr {
                region: e["region"].as_i64().unwrap_or(-1),
                offset: e["offset"].as_i64().unwrap_or(0),
            },
            ne,
            nb,
            ty,
            token_axis,
        })
    };

    let mut ops = Vec::new();
    for op in step["ops"].as_array().ok_or_else(|| fail("the plan's step has no ops".into()))? {
        let kind = op["op"].as_str().unwrap_or("");
        let inputs: Vec<&str> = op["inputs"]
            .as_array()
            .map(|a| a.iter().filter_map(|x| x.as_str()).collect())
            .unwrap_or_default();
        let output = op["outputs"][0].as_str().unwrap_or("");
        let params = &op["params"];
        let p = |k: &str| params[k].as_f64().unwrap_or(0.0);
        let (kernel, rule, width) = match kind {
            // These move no data: their tensors are views the plan placed.
            "view" | "reshape" | "permute" | "transpose" => continue,
            "get_rows" => (0, threads::DST, 1),
            "rms_norm" => (1, threads::DST_ROWS, threads::GROUP),
            "mul" => (2, threads::DST, 1),
            "add" => (3, threads::DST, 1),
            "mul_mat" => (4, threads::MAT, threads::GROUP),
            "rope" => (5, threads::DST, 1),
            "cpy" | "cont" => (6, threads::DST, 1),
            "set_rows" => (7, threads::SRC1, 1),
            "swiglu" => (8, threads::DST, 1),
            "flash_attn_ext" => (9, threads::ATTENTION, threads::GROUP),
            other => {
                return Err(fail(format!(
                    "its step uses the op `{other}`, which the Metal kernels do not run yet"
                )));
            }
        };
        let mut rec = [0i64; OP_WORDS];
        rec[0] = kernel;
        for slot in 0..5 {
            rec[1 + slot * 12] = -1;
        }
        let mut put = |slot: usize, t: Tensor| {
            let b = 1 + slot * 12;
            rec[b] = t.addr.region;
            rec[b + 1] = t.addr.offset;
            rec[b + 2..b + 6].copy_from_slice(&t.ne);
            rec[b + 6..b + 10].copy_from_slice(&t.nb);
            rec[b + 10] = t.ty;
            rec[b + 11] = t.token_axis;
        };
        for (i, name) in inputs.iter().enumerate().take(4) {
            put(i, tensor(name)?);
        }
        put(4, tensor(output)?);
        let mut f = [0f32; 8];
        let mut ints = [0i64; 8];
        match kind {
            "rms_norm" => f[0] = p("eps") as f32,
            // The kernel decodes the weights and reads the other side as floats.
            "mul_mat" if ![0, 1, 30].contains(&rec[1 + 12 + 10]) => {
                return Err(fail(format!(
                    "its mul_mat {output} multiplies by {}, of ggml type {}, and the Metal kernels \
                     multiply weights by f32, f16 or bf16 values only",
                    inputs.get(1).unwrap_or(&""),
                    rec[1 + 12 + 10]
                )));
            }
            "rope" => {
                if p("ext_factor") != 0.0 {
                    return Err(fail(
                        "its rope uses YaRN scaling, which the Metal kernels do not run yet".into(),
                    ));
                }
                f[0] = p("freq_base") as f32;
                f[1] = p("freq_scale") as f32;
                f[2] = p("attn_factor") as f32;
                ints[0] = params["n_dims"].as_i64().unwrap_or(0);
                ints[1] = params["mode"].as_i64().unwrap_or(0);
                if ints[1] != 0 && ints[1] != 2 {
                    return Err(fail(format!(
                        "its rope mode {} is not one the kernels run",
                        ints[1]
                    )));
                }
            }
            "flash_attn_ext" => {
                if p("max_bias") != 0.0 || p("logit_softcap") != 0.0 {
                    return Err(fail("its attention uses ALiBi or a soft cap, which the Metal kernels do not run yet".into()));
                }
                if rec[1 + 2 * 12 + 2] > 256 {
                    return Err(fail("its attention heads are wider than 256".into()));
                }
                f[0] = p("scale") as f32;
            }
            _ => {}
        }
        for k in 0..4 {
            let lo = f[2 * k].to_bits() as i64;
            let hi = f[2 * k + 1].to_bits() as i64;
            rec[61 + k] = lo | (hi << 32);
        }
        rec[65..73].copy_from_slice(&ints);
        rec[73] = rule;
        rec[74] = width;
        ops.push(rec);
    }

    // The inputs a step fills. Anything else the layout names as an input,
    // such as the rotation a quantized cache takes, is not one this program
    // knows how to fill.
    let known =
        ["inp_tokens", "inp_pos", "inp_k_idxs", "inp_v_idxs", "attn_inp_kq_mask", "inp_out_ids"];
    for (name, e) in &entries {
        if e["role"]["kind"] == "input" && !known.contains(name) {
            return Err(fail(format!(
                "its step takes the input {name}, which a program does not fill yet; a quantized \
                 KV cache needs one, so add the model to dovata with an f16 cache"
            )));
        }
    }
    let at = |name: &str, ty: i64| -> Result<Addr, String> {
        let t = tensor(name)?;
        if t.ty != ty {
            return Err(fail(format!("input {name} has ggml type {}, not {ty}", t.ty)));
        }
        Ok(t.addr)
    };
    let tokens = at("inp_tokens", 26)?;
    let positions = at("inp_pos", 26)?;
    let k_cells = at("inp_k_idxs", 27)?;
    let v_cells = at("inp_v_idxs", 27)?;
    let mask = at("attn_inp_kq_mask", 1)?;
    let out_ids = at("inp_out_ids", 26)?;
    let mask_t = tensor("attn_inp_kq_mask")?;
    if mask_t.ne[0] != n_ctx {
        return Err(fail(format!(
            "the attention mask spans {} cells, not the cache's {n_ctx}",
            mask_t.ne[0]
        )));
    }
    let out_name = step["outputs"][0].as_str().unwrap_or("result_output");
    let out = tensor(out_name)?;
    if out.ty != 0 {
        return Err(fail(format!("its logits, {out_name}, are not f32")));
    }

    let tok = read_tokenizer(model, plan)?;
    Ok(InferPlan {
        model: model.to_string(),
        table_hash: plan["table_hash"].as_str().unwrap_or("").to_string(),
        regions,
        n_tokens,
        n_ctx,
        ops,
        tokens,
        positions,
        k_cells,
        v_cells,
        mask,
        out_ids,
        logits: out.addr,
        n_vocab: out.ne[0],
        stops: tok.stops,
        pieces: tok.pieces,
        merges: tok.merges,
        added: tok.added,
        split: tok.split,
    })
}

/// Purpose: This struct holds the tokenizer's tables, as the prelude reads them.
struct Tokenizer {
    stops: Vec<i64>,
    pieces: String,
    merges: String,
    added: String,
    split: i64,
}

/// Purpose: This function reads the plan's tokenizer into the prelude's tables.
///
/// Inputs:
/// - `model`: the model's name, for messages.
/// - `plan`: the plan's JSON.
///
/// Outputs: the tables, or a message naming a tokenizer the prelude cannot run.
fn read_tokenizer(model: &str, plan: &serde_json::Value) -> Result<Tokenizer, String> {
    let fail = |m: String| format!("model {model}: {m}");
    let t = plan.get("tokenizer").filter(|t| !t.is_null()).ok_or_else(|| {
        fail("the plan carries no tokenizer; dovata 3.2 or later plans with one".into())
    })?;
    if t["kind"] != "gpt2" {
        return Err(fail(format!(
            "its tokenizer is {}, and a program runs byte-level BPE (gpt2) only so far",
            t["kind"]
        )));
    }
    let def: serde_json::Value = serde_json::from_str(t["definition"].as_str().unwrap_or(""))
        .map_err(|e| fail(format!("its tokenizer.json does not read ({e})")))?;

    let mut by_id: BTreeMap<i64, String> = BTreeMap::new();
    if let Some(vocab) = def["model"]["vocab"].as_object() {
        for (piece, id) in vocab {
            if let Some(id) = id.as_i64() {
                by_id.insert(id, piece.clone());
            }
        }
    }
    let mut added = String::new();
    let mut endoftext = None;
    for a in def["added_tokens"].as_array().into_iter().flatten() {
        let (Some(id), Some(text)) = (a["id"].as_i64(), a["content"].as_str()) else { continue };
        by_id.insert(id, text.to_string());
        added.push_str(&format!("{id} {text}\n"));
        if text == "<|endoftext|>" {
            endoftext = Some(id);
        }
    }
    let max = by_id.keys().next_back().copied().unwrap_or(-1);
    let mut pieces = String::new();
    for id in 0..=max {
        let p = by_id.get(&id).map(String::as_str).unwrap_or("");
        if p.contains('\n') {
            return Err(fail(format!(
                "token {id} holds a line break, which the table cannot carry"
            )));
        }
        pieces.push_str(p);
        pieces.push('\n');
    }
    let mut merges = String::new();
    for m in def["model"]["merges"].as_array().into_iter().flatten() {
        let line = match m {
            serde_json::Value::String(s) => s.clone(),
            serde_json::Value::Array(pair) => format!(
                "{} {}",
                pair.first().and_then(|x| x.as_str()).unwrap_or(""),
                pair.get(1).and_then(|x| x.as_str()).unwrap_or("")
            ),
            _ => continue,
        };
        merges.push_str(&line);
        merges.push('\n');
    }

    let normalizer = &def["normalizer"];
    let fine = normalizer.is_null() || normalizer["type"] == "NFC";
    if !fine {
        return Err(fail(format!(
            "its tokenizer normalizes text with {}, which the prelude does not do yet",
            normalizer["type"]
        )));
    }
    let split = pre_tokenizer(&def["pre_tokenizer"]).ok_or_else(|| {
        fail("its tokenizer splits text with a pattern the prelude does not know".into())
    })?;

    let mut stops: Vec<i64> = t["stop"]
        .as_array()
        .map(|s| s.iter().filter_map(|x| x.as_i64()).collect())
        .unwrap_or_default();
    // A raw completion ends at `<|endoftext|>`, which the GGUF does not list.
    if let Some(id) = endoftext
        && !stops.contains(&id)
    {
        stops.push(id);
    }
    Ok(Tokenizer { stops, pieces, merges, added, split })
}

/// Purpose: This function works out which split a pre-tokenizer makes.
///
/// Inputs:
/// - `p`: tokenizer.json's `pre_tokenizer`.
///
/// Outputs: 1 for Qwen 2's pattern, 2 for Llama 3's, 3 for GPT-2's, or `None`.
fn pre_tokenizer(p: &serde_json::Value) -> Option<i64> {
    let mut patterns = Vec::new();
    let mut byte_level_regex = false;
    let mut stack = vec![p];
    while let Some(v) = stack.pop() {
        if let Some(list) = v["pretokenizers"].as_array() {
            stack.extend(list);
        }
        if v["type"] == "Split"
            && let Some(r) = v["pattern"]["Regex"].as_str()
        {
            patterns.push(r.to_string());
        }
        if v["type"] == "ByteLevel" && v["use_regex"].as_bool().unwrap_or(true) {
            byte_level_regex = true;
        }
    }
    match patterns.as_slice() {
        [r] if r.contains(r"\p{N}{1,3}") => Some(2),
        [r] if r.contains(r"[^\r\n\p{L}\p{N}]?\p{L}+") && r.contains(r"\p{N}") => Some(1),
        [] if byte_level_regex => Some(3),
        [r] if r.contains(r"'s|'t|'re|'ve|'m|'ll|'d| ?\p{L}+| ?\p{N}+") => Some(3),
        _ => None,
    }
}

#[cfg(test)]
mod tests;
