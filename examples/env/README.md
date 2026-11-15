# `examples/env/`: `std::env`

## What this subtree is

The fixture for `std::env`: environment variables, the program's arguments and its working directory. Since 2.0.8 the package is written in Jade, `compiler/src/lib/env.jde`, one hook a function.

- *`vars/`* reads a variable that is not set, sets one, sees that a command started afterwards inherits it, reads the arguments and the working directory, and shows which are trusted: a variable's value is tainted, so `sh.exec` refuses it, while the arguments and the working directory are the program's own. A value that is not UTF-8 reads as nil, as Rust's `env::var` gave. To choose its own arguments and environment, the fixture runs itself as a child through the shell.

## Why it was built

Until 2.0.8 no fixture covered `std::env` beyond reading the first argument, and the trust rule is the part a program depends on: an environment variable is set by whoever started the program. Its `.expected` was recorded from the backend version before the package moved into Jade.

## Who uses it

`compiler/src/scripts/golden.sh` builds it and compares its output with `vars.expected`. `examples/functions/exit_call_error.jde` uses `env.args()` too.
