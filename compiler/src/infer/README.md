# `src/infer/`: compiling a prompt into a program that runs a model

## What this subtree is

This is how `?p` works in Jade 2.0. A program that dereferences a prompt runs a language model on the GPU, over memory the dovata process holds and hands out, with no runtime library and no model server in between:

```jade
set_max_tokens(16)
prompt p = "The capital of France is"
print(?p)                   //  Paris. The capital of France is also the capital of …
```

Building that program asks the dovata process for the model's *plan*: where every tensor sits, which ggml ops make one forward step, and how the model tokenizes text. The compiler writes GPU kernels for those ops and a table of them into the program. Running it maps the model's memory from dovata, compiles the kernels, and runs each step as one Metal command buffer.

```sh
dovata start
dovata add https://huggingface.co/unsloth/Qwen3-0.6B-GGUF --quant Q4_K_M
jade run app.jde
```

It runs on Apple Silicon Macs for now. The kernels are deliberately plain, one thread per output value, because the tuning goes to the CUDA and Linux kernels first; these exist so a program on a Mac runs its prompts at all. Qwen3 0.6B makes a token in about a fifth of a second.

## How a prompt becomes a reply

*Before checking*, `rewrite.rs` adds `prelude.jde` to the merged program, in a namespace of its own like any imported module, and turns each dereference into a call into it: `?p` becomes `reply(__infer.text(p))`, and `?p |> int` becomes `reply_int(…)`. The checker then checks the prelude's functions at the types they are called with, and MIR lowers them like any others. A prompt is its text from here on.

*The prelude* is Jade. It tokenizes text with byte-level BPE, the kind Qwen, Llama 3 and GPT-2 use: it splits the text into words by the model's pre-tokenizer pattern, writes each word's bytes in the byte alphabet, merges pairs by rank, and looks the pieces up. Added tokens such as `<|im_start|>` are matched first. Decoding maps pieces back to bytes. Its tables come from the plan through `__infer.data`, built once when the program starts, so a task that dereferences a prompt only reads them, and the task check accepts it. The typed forms read the first number, the first yes or no, or the first char out of the reply.

*The backend half*, `src/backend/infer.rs`, answers what the prelude asks of `__infer`: the tables, the Unicode classes the splitter needs, mapping the model, setting up Metal, and `generate`, which runs the prompt's tokens through as many steps as they take and then picks the likeliest token, greedily, until a stop token or `max_tokens()`.

## Why it is built this way

*Inference is compiled, not served.* The kernels and the op table are part of the program, and the model's memory is mapped into it, so a step crosses no process boundary. dovata holds the memory and checks the plan's table hash when a program asks for it, so a program compiled against one plan cannot run on memory planned another way.

*The tokenizer is Jade source.* The program has no runtime to link a tokenizer from, and writing byte-level BPE in Jade is short and checked like any program. What Jade cannot say is which Unicode class a char is in, so that one question is the backend's, answered from `unicode.rs`'s tables.

*A step may run fewer tokens than the plan's step size.* dovata plans a step of `n_tokens`, 512 by default, and every short-lived tensor is placed for that many. Each tensor that grows with the step keeps its tokens outermost in memory, so its first rows are that tensor for fewer tokens. The op table marks which dimension of each tensor counts tokens, and each step puts the real count there. Decoding one token at a time then costs one token's work, not 512's.

*One lock covers the model.* The KV cache holds one conversation at a time, so two tasks dereferencing prompts take turns.

## What each file does

- *`mod.rs`* registers `__infer`, the prelude's own package. It is not documented for programs; its name keeps it out of their way.
- *`rewrite.rs`* finds every `?p` and turns it into a call into the prelude, which it adds to the program. A dereference into a class or through a grammar is left for MIR to name as not compiled yet.
- *`prelude.jde`* is the tokenizer, the decoder and the functions dereferences become.
- *`dovata.rs`* speaks dovata's protocol at build time: which models the process holds, and one model's plan. It is version 2 of the protocol, which dovata 3.1 and 3.2 answer; the plan carries the tokenizer from dovata 3.2.
- *`plan.rs`* checks the plan describes a model the kernels can run and turns it into an `InferPlan`: one fixed-size record per op that launches a kernel, the addresses of the inputs each step fills, the logits, the stop tokens and the tokenizer's tables. Views, reshapes and permutes launch nothing, since the plan already placed their tensors. An op, a weight type, an input or a tokenizer it does not know is refused by name.
- *`kernels.metal`* holds the kernels, one per op: `get_rows`, `rms_norm`, `mul`, `add`, `mul_mat`, `rope`, `cpy`, `set_rows`, `swiglu` and `flash_attn_ext`. They read f32, f16, bf16, q8_0, q4_0, q4_K, q5_K and q6_K weights, dequantizing each value as it is read.
- *`unicode.rs`* holds the letter, number and white space classes the pre-tokenizer splits by.
- *`tests.rs`* compares a program's tokens and its greedy reply with llama.cpp's `llama-tokenize` and `llama-simple` on the same GGUF file. They skip, saying why, without a Mac, a running dovata process holding one model, or llama.cpp installed.

## Who uses it

*Used by:* `aot/mod.rs`, which runs the rewrite after imports are merged and reads the plan when a program that runs a model is built; `backend/infer.rs`, which writes the plan into the program; and `backend/link.rs`, which links such a program against the Objective-C runtime and Metal as well as the C library.

*Depends on:* the dovata process at build time and at run time, and on nothing else of dovata's: the compiler and the program speak its protocol themselves.

## What it does not do yet

- *Other machines.* Linux and CUDA come next; a program that dereferences a prompt is refused elsewhere.
- *Decoding into a class*, `?p |> City`, and *grammar-constrained* prompts. Both stop with an error naming them.
- *Sampling.* Replies are greedy, so the same prompt gives the same reply.
- *A quantized KV cache*, which takes Hadamard rotations as step inputs. Add the model to dovata with an f16 cache, the default.
- *NFC normalization* of the prompt. Text that is already composed, as nearly all is, tokenizes the same either way.
- *Speed.* Every value is dequantized where it is read, index math is 64-bit, and attention keeps its sums in thread memory.

## Building and testing

```sh
cargo test infer::        # the plan reader always; the model checks when dovata and llama.cpp are there
```
