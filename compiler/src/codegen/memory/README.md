# `src/codegen/memory/`: layouts and reference counting

## What this subtree is

This directory decides how each value sits in memory, and when a value on the heap is freed.

Jade has no garbage collector. Every string, bytes, array, dict, class object and future is a heap object that starts with a reference count. Taking another reference adds one, giving one up subtracts one, and the last one frees the object and gives up what it holds. MIR writes each of those steps out as an instruction, `retain` or `release`, and `rc.rs` turns them into LLVM:

```jade
fn greet(name) {
    let a = "hi " + name   // a new string, count 1
    let b = a              // retain: count 2
    print(b)
}                          // release a: count 1; release b: count 0, so the string is freed
```

## Why it is a directory

`layout.rs` and `rc.rs` sat at the top of `src/codegen/` until 2.0.13. They answer the same question from two sides: where a value's parts are, and when they are freed.

## What each file does

- *`layout.rs`* maps each Jade type to the LLVM type that holds it, holds the byte offsets inside heap objects, such as `LEN`, `ARR_DATA` and `TYPE_ID`, and reads and makes optionals. `DROPPED` is the bit in a class object's type id word that says its destructors have run.
- *`rc.rs`* writes retain and release. Retain is the same for every heap object: add one to the count, atomically, since tasks share one heap. Release differs by type, because the last release frees what the object holds: a string frees itself, an array of strings releases each string first, and a class object runs its destructors and releases each field that needs it. So each heap type gets its own `jade.release.<type>`, generated the first time a program releases one. A count below zero marks an immortal object, such as a string literal, which retain and release skip.

## Who uses it

*Used by:* `../func/`, which writes a retain or release for every MIR instruction that asks for one, and nearly every other file in codegen, since support code that builds or drops a value counts its references the same way. `layout.rs` is used everywhere a heap object's field is read.

*Depends on:* `../libcache/shared.rs`, because releasing a class object reads the program's classes, so a library object calls the program's `jade.release.struct`, and `../tasks/` for the offset of a future's free function.

## Gotchas

*A destructor runs once.* A class's release holds a reference while its destructors run, then gives it back, and frees the object only when that was the last one. A destructor that stored `self` keeps the object alive, and `DROPPED` stops its destructors from running again when it is finally freed. Every read of a type id masks the bit off.

## Building and testing

```sh
cargo test codegen::
```
