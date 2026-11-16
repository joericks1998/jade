# `src/link/`: object files to a binary

## What this subtree is

This is the last step of `jade build`. [`src/codegen/`](../codegen/README.md) writes object files through LLVM: machine code that cannot run yet, because each call into another object or into the C library is a hole, a name with no address. The linker fills in those addresses and writes the file the operating system runs, Mach-O on macOS and ELF on Linux. The driver in [`src/build/`](../build/README.md) runs codegen, then hands the list of objects here.

```
program.o + cached std library objects + a Rust package's .a + the C library → LLD → binary
```

Jade files a program imports are not linked: `mir/imports.rs` merges their source into one program before it is checked, so they compile together. What gets linked is the program's object, the std library's cached objects, a Rust package's staticlib when the program calls one, and the C library, which is not copied in: the binary records that it needs it, and the operating system loads it when the program starts.

## Why it was built

A program is linked in-process by LLD, the linker LLVM ships, built from the same LLVM 18 install. The only thing a machine needs to link a program is the C library itself: `libSystem` on macOS, whose symbols `mod.rs` lists in a stub it writes, and on Linux glibc's `libc.so.6`, which every glibc system has, or on a system without glibc, such as Alpine, musl's loader, which is its C library too. So `jade build` works with no `cc`, no Xcode tools and no development packages; a binary is ad hoc signed by LLD, as Apple Silicon requires. A program that calls a Rust package is the exception: its staticlib imports from whatever its `-sys` crates link, so on macOS it links against Apple's SDK stubs, which cargo already needed to build the package, and on Linux each library rustc's `native-static-libs` note names is found beside the C library.

Until 2.0.11 this was `link.rs` inside the code generator, which called it at the end of every build. It is its own directory now, so each step of a build is one directory and codegen never reaches ahead into linking.

## What each file does

- *`mod.rs`* links the objects into a program with LLD, in-process, the program's first and the cached library objects after it, with the Rust packages' staticlib and the system libraries rustc names for it when the program calls one. On macOS it writes a stub the linker reads for each system library the objects import from, an import being what one object leaves undefined and none defines, `libSystem` always and for a program that runs a model the Objective-C runtime and Metal, listing exactly the symbols it uses, so no Apple SDK is needed. On Linux it links against glibc's runtime `libc.so.6`, or musl's loader where there is no glibc, and the program's `_start` comes from codegen, `codegen/start.rs`, so no development package is needed either.
- *`lld.cpp`* is the C entry point into LLD's Mach-O and ELF drivers, and the only C++ in the compiler.

## Who uses it

*Used by:* `build::binary`, after codegen has written the objects.

*Depends on:* LLD, LLVM's linker, which `compiler/build.rs` compiles into the compiler from the same LLVM 18 install, and `rustpkg::Link` for a Rust package's staticlib and system libraries.

## Gotchas

*The program's object goes first.* It defines the shared state the cached library objects declare, and on macOS the stubs are written beside it.

*A link failure with cached objects is retried by the driver, not here.* `build::binary` compiles once more in one module when a link with an object from the cache fails, since a damaged object is the one thing a cache adds.

## Building and testing

```sh
cargo test link::
```
