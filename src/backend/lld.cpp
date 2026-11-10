// The linker, as a function: a C entry point into LLD.
//
// `jade build` with the static backend compiles in-process through LLVM, and
// this lets it link in-process too, with LLVM's own linker, instead of running
// the system `cc`. LLD is built from the same LLVM install the compiler links,
// as the static libraries `liblldMachO.a` and `liblldELF.a`; `build.rs` compiles
// this file and links them. See `link.rs` for what is passed.
//
// LLD's library entry point is C++, so this file is the only C++ in the
// compiler. It does three things: picks the driver from the program name in
// `argv[0]` (`ld64.lld` for Mach-O, `ld.lld` for ELF), collects what the linker
// prints, and hands back the exit code. One link runs at a time, since LLD
// keeps global state.

#include "lld/Common/Driver.h"
#include "llvm/Support/raw_ostream.h"

#include <algorithm>
#include <cstring>
#include <mutex>
#include <string>

LLD_HAS_DRIVER(elf)
LLD_HAS_DRIVER(macho)

// Purpose: This function runs LLD on a command line.
//
// Inputs:
// - `argc`, `argv`: the command line; `argv[0]` names the flavor.
// - `msg`, `msg_len`: where to write what the linker printed, NUL-terminated
//   and cut to fit.
//
// Outputs: LLD's exit code: 0 on success. A link that crashed LLD leaves it
// unable to run again, and later calls answer 1 with a message saying so.
extern "C" int jade_lld_link(int argc, const char **argv, char *msg, size_t msg_len) {
  static std::mutex lock;
  static bool broken = false;
  std::lock_guard<std::mutex> guard(lock);

  std::string text;
  int code = 1;
  if (broken) {
    text = "the linker crashed on an earlier link in this process and cannot run again";
  } else {
    std::string out, err;
    llvm::raw_string_ostream out_os(out), err_os(err);
    lld::Result r = lld::lldMain(llvm::ArrayRef<const char *>(argv, argc), out_os, err_os,
                                 {{lld::Gnu, &lld::elf::link}, {lld::Darwin, &lld::macho::link}});
    out_os.flush();
    err_os.flush();
    code = r.retCode;
    broken = !r.canRunAgain;
    text = err.empty() ? out : err;
  }
  if (msg && msg_len) {
    size_t n = std::min(text.size(), msg_len - 1);
    std::memcpy(msg, text.data(), n);
    msg[n] = '\0';
  }
  return code;
}
