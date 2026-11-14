//! The model's plan, from dovata, read into what the backend writes into the
//! program.
//!
//! The compiler links dovata's library and calls it: `dovata::host::connect`
//! finds the dovata process, and the process's records say which models it
//! holds, where each region of a model's memory is, and the model's plan. The
//! plan says where every tensor sits, as a region and a byte offset; which ggml
//! ops make one forward step, in order; and how the model tokenizes text. This
//! file checks the plan describes a model these kernels can run, and turns it
//! into an [`InferPlan`]: one fixed-size record per op that launches a kernel,
//! the addresses of the inputs each step fills, the tokenizer's tables as text,
//! and the name of the file or shared memory object each region is in, so the
//! program opens and maps every region by itself.
//!
//! *The step size.* The plan places every short-lived tensor for a step of
//! `n_tokens` tokens. A step may run fewer: the program fills only the first
//! rows, and each kernel is told the count it runs. Every tensor that grows with
//! the step keeps its tokens outermost in memory, so the first rows of a tensor
//! planned for 512 tokens are that tensor for fewer. A record marks which
//! dimension of each tensor is the token count, the one equal to `n_tokens`
//! with the largest stride, and the program puts the real count there.

use std::collections::{BTreeMap, HashMap};
use std::path::Path;

use dovata::host::{HeldModel, HostError, Open};
use dovata::layout::Param;
use dovata::memplan::{AddressEntry, Plan, Role};

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

/// Purpose: This struct holds where one region of the model's memory is, as
/// the program opens and maps it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RegionOpen {
    /// The part file's path, or the shared memory object's name.
    pub name: String,
    /// Whether `name` is a shared memory object, opened with `shm_open`, rather
    /// than a file.
    pub shared: bool,
    /// Where the region starts in the file or object, in bytes.
    pub offset: i64,
    /// The region's size in bytes.
    pub bytes: i64,
    /// Whether the program may write it: allocations yes, weights never.
    pub writable: bool,
    /// For a part file: its size, and when it was last changed, in seconds and
    /// nanoseconds, as dovata loaded it. The program refuses a file that
    /// differs, since a file changed in place changes the bytes under it.
    pub file_bytes: i64,
    pub modified_sec: i64,
    pub modified_nsec: i64,
}

/// Purpose: This struct holds everything about a model the backend writes into
/// a program that runs it.
#[derive(Debug, Clone)]
pub struct InferPlan {
    /// The model's name in the dovata process.
    pub model: String,
    /// Every region the program maps, in id order. The shared memory names
    /// include the plan's table hash, so a program compiled against another
    /// plan finds no memory to map rather than memory its offsets do not fit.
    pub regions: Vec<RegionOpen>,
    /// The directory the program registers in once it maps the model, so
    /// `dovata status` lists it.
    pub clients: String,
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

/// The advice every failure to find a model in dovata ends with.
const START: &str = "a program that dereferences a prompt runs a model the dovata process \
                     holds; install it with `curl -fsSL \
                     https://raw.githubusercontent.com/joericks1998/jade/main/dovata/install.sh \
                     | sh`, start it with `dovata start`, and add a model with `dovata add \
                     <model>`";

/// Purpose: This function asks dovata for the model a program runs and reads
/// its plan.
///
/// Inputs:
/// - `name`: the model to use, from `jade.toml`'s `[model]`, or `None` for the
///   one model the process holds.
///
/// Outputs: the plan, or a message: dovata is not running, holds no model or
/// several, or holds a model these kernels cannot run.
pub fn from_dovata(name: Option<&str>) -> Result<InferPlan, String> {
    let dovata = dovata::host::connect().map_err(|e| match e {
        HostError::NotRunning => format!("dovata is not running; {START}"),
        e => format!("dovata: {e}"),
    })?;
    let held = match name {
        Some(n) => dovata.model(n).map_err(|e| format!("dovata: {e}"))?,
        None => {
            let mut models = dovata.models().map_err(|e| format!("dovata: {e}"))?;
            match models.len() {
                1 => models.remove(0),
                0 => return Err(format!("dovata holds no model; {START}")),
                n => {
                    let names: Vec<&str> = models.iter().map(|m| m.name.as_str()).collect();
                    return Err(format!(
                        "dovata holds {n} models, {}; name the one this program runs in \
                         jade.toml:\n    [model]\n    name = \"{}\"",
                        names.join(", "),
                        names[0]
                    ));
                }
            }
        }
    };
    let plan = dovata.plan(&held.name).map_err(|e| format!("dovata: {e}"))?;
    read(&held, &plan, &dovata.clients_dir(&held.name))
}

/// Purpose: This function gives an op parameter as a float.
///
/// Inputs:
/// - `params`: the op's parameters.
/// - `key`: the parameter's name.
///
/// Outputs: its value, or 0 when the op has no such number.
fn float(params: &BTreeMap<String, Param>, key: &str) -> f64 {
    match params.get(key) {
        Some(Param::Float(f)) => *f,
        Some(Param::Int(i)) => *i as f64,
        _ => 0.0,
    }
}

/// Purpose: This function gives an op parameter as an integer.
///
/// Inputs:
/// - `params`: the op's parameters.
/// - `key`: the parameter's name.
///
/// Outputs: its value, or 0 when the op has no such integer.
fn int(params: &BTreeMap<String, Param>, key: &str) -> i64 {
    match params.get(key) {
        Some(Param::Int(i)) => *i,
        _ => 0,
    }
}

/// Purpose: This function says where each region of a model's memory is, from
/// dovata's record of it.
///
/// Inputs:
/// - `held`: the record.
/// - `plan`: the model's plan, whose regions the record must match.
///
/// Outputs: one [`RegionOpen`] per region, in id order, or a message saying
/// the record and the plan disagree.
fn regions(held: &HeldModel, plan: &Plan) -> Result<Vec<RegionOpen>, String> {
    let again = "add the model to dovata again";
    if held.table_hash != plan.table_hash || held.regions.len() != plan.regions.len() {
        return Err(format!("dovata's record of {} and its plan disagree; {again}", held.name));
    }
    let mut out = Vec::new();
    for (i, r) in held.regions.iter().enumerate() {
        if usize::from(r.id) != i {
            return Err(format!("dovata lists region {} of {} at {i}; {again}", r.id, held.name));
        }
        let text = |p: &Path| {
            p.to_str().filter(|t| !t.contains('\0')).map(str::to_string).ok_or_else(|| {
                format!("{} is not a path a program can carry; move the model to a plain path and {again}", p.display())
            })
        };
        let open = match &r.open {
            Open::File { .. } => {
                let f = held
                    .file_of(r)
                    .ok_or_else(|| format!("region {i} of {} names no file; {again}", held.name))?;
                RegionOpen {
                    name: text(&f.path)?,
                    shared: false,
                    offset: r.offset as i64,
                    bytes: r.bytes as i64,
                    writable: false,
                    file_bytes: f.bytes as i64,
                    modified_sec: f.modified_sec,
                    modified_nsec: f.modified_nsec,
                }
            }
            Open::Shared { name } => RegionOpen {
                name: name.clone(),
                shared: true,
                offset: r.offset as i64,
                bytes: r.bytes as i64,
                writable: r.writable,
                file_bytes: 0,
                modified_sec: 0,
                modified_nsec: 0,
            },
        };
        out.push(open);
    }
    Ok(out)
}

/// Purpose: This function reads a plan into what the backend writes.
///
/// Inputs:
/// - `held`: dovata's record of the model: its name and where its memory is.
/// - `plan`: the model's plan.
/// - `clients`: the directory a program registers in once it maps the model.
///
/// Outputs: the plan, or a message naming what these kernels cannot run.
pub fn read(held: &HeldModel, plan: &Plan, clients: &Path) -> Result<InferPlan, String> {
    let model = held.name.as_str();
    let fail = |m: String| format!("model {model}: {m}");
    let step = plan.step.as_ref().ok_or_else(|| fail("the plan has no step".into()))?;
    let opts = step.options;
    let n_tokens = i64::from(opts.n_tokens);
    let n_ctx = i64::from(opts.n_ctx);
    if opts.n_outputs != 1 {
        return Err(fail(format!(
            "the plan returns logits for {} tokens a step, and a program reads one; add the \
             model to dovata with one output",
            opts.n_outputs
        )));
    }
    let entries: HashMap<&str, &AddressEntry> =
        plan.entries.iter().map(|e| (e.tensor.as_str(), e)).collect();
    let regions = regions(held, plan).map_err(fail)?;
    let clients = clients
        .to_str()
        .filter(|t| !t.contains('\0'))
        .ok_or_else(|| fail(format!("{} is not a path a program can carry", clients.display())))?
        .to_string();

    let tensor = |name: &str| -> Result<Tensor, String> {
        let e =
            entries.get(name).ok_or_else(|| fail(format!("tensor {name} is not in the plan")))?;
        let ty = i64::from(e.ggml_type);
        let (blk, bytes) = block(ty).ok_or_else(|| {
            fail(format!("tensor {name} has ggml type {ty}, which these kernels do not read"))
        })?;
        let mut ne = [1i64; 4];
        for (i, d) in e.dims.iter().take(4).enumerate() {
            ne[i] = *d as i64;
        }
        let nb = match &e.role {
            Role::View { nb, .. } => nb.map(|x| x as i64),
            _ => {
                let row = ne[0] / blk * bytes;
                [bytes, row, row * ne[1], row * ne[1] * ne[2]]
            }
        };
        let grows = !matches!(e.role, Role::Weight | Role::Kv) && n_tokens > 1;
        let token_axis = if grows {
            (0..4).filter(|&i| ne[i] == n_tokens).max_by_key(|&i| nb[i]).map_or(-1, |i| i as i64)
        } else {
            -1
        };
        Ok(Tensor {
            addr: Addr { region: i64::from(e.region), offset: e.offset as i64 },
            ne,
            nb,
            ty,
            token_axis,
        })
    };

    let mut ops = Vec::new();
    for op in &step.ops {
        let kind = op.op.as_str();
        let inputs: Vec<&str> = op.inputs.iter().map(String::as_str).collect();
        let output = op.outputs.first().map_or("", String::as_str);
        let params = &op.params;
        let p = |k: &str| float(params, k);
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
                ints[0] = int(params, "n_dims");
                ints[1] = int(params, "mode");
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
                for slot in [1, 2] {
                    let ty = rec[1 + slot * 12 + 10];
                    if ![0, 1, 30].contains(&ty) {
                        return Err(fail(format!(
                            "its attention reads a KV cache of ggml type {ty}, and the Metal \
                             kernels read f32, f16 or bf16 caches only; add the model to dovata \
                             with an f16 cache, the default"
                        )));
                    }
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
        if e.role == Role::Input && !known.contains(name) {
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
    let out_name = step.outputs.first().map_or("result_output", String::as_str);
    let out = tensor(out_name)?;
    if out.ty != 0 {
        return Err(fail(format!("its logits, {out_name}, are not f32")));
    }

    let tok = read_tokenizer(model, plan)?;
    Ok(InferPlan {
        model: model.to_string(),
        regions,
        clients,
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
/// - `plan`: the plan.
///
/// Outputs: the tables, or a message naming a tokenizer the prelude cannot run.
fn read_tokenizer(model: &str, plan: &Plan) -> Result<Tokenizer, String> {
    let fail = |m: String| format!("model {model}: {m}");
    let t = plan.tokenizer.as_ref().ok_or_else(|| {
        fail("the plan carries no tokenizer; dovata 3.2 or later plans with one".into())
    })?;
    if t.kind != "gpt2" {
        return Err(fail(format!(
            "its tokenizer is {:?}, and a program runs byte-level BPE (gpt2) only so far",
            t.kind
        )));
    }
    // The model's tokenizer.json, as Hugging Face writes it.
    let def: serde_json::Value = serde_json::from_str(&t.definition)
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

    let mut stops: Vec<i64> = t.stop.iter().map(|&id| i64::from(id)).collect();
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
