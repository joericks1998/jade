---
id: quickstart
title: Quick Start
sidebar_label: Quick Start
---

Create a file named `hello.jde`:

```jade
let x = 10
let y = 32
let answer = x + y
print(answer)
```

Run it:

```bash
jade hello.jde
```

Output:

```
42
```

You can also use `--verbose` to print every variable's final value after execution:

```bash
jade hello.jde --verbose
```

Output:

```
answer = 42
x = 10
y = 32
```

:::note
Variables are printed in alphabetical order when using `--verbose`. The `print` built-in function prints a single value to stdout and can be called anywhere in the program.
:::
