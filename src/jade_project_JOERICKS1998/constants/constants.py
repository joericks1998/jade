SUPPORTED_MODELS = {
    "deepseek-chat": "https://api.deepseek.com",
    "deepseek-reasoner": "https://api.deepseek.com",
}

PYTHON_BUILTINS = {
    # Basic functions
    "print",
    "len",
    "type",
    "id",
    "hash",
    "repr",
    "str",
    "int",
    "float",
    "bool",
    # Collections
    "list",
    "dict",
    "set",
    "tuple",
    "range",
    "enumerate",
    "zip",
    # Math
    "abs",
    "min",
    "max",
    "sum",
    "round",
    "pow",
    # I/O
    "input",
    "open",
    "close",
    # Object oriented
    "isinstance",
    "issubclass",
    "super",
    # Functional programming
    "map",
    "filter",
    "reduce",
    # Attributes
    "getattr",
    "setattr",
    "hasattr",
    # Execution
    "eval",
    "exec",
    # Iterators
    "iter",
    "next",
    # Sorting
    "sorted",
    "reversed",
    # Files
    "dir",
    "help",
}

MASTER_TOKEN_PATTERN = r"""
        \s+               |
        \"[^\"]*\"        | # Double quoted strings: "hello world"
        '[^']*'          | # Single quoted strings: 'hello world'

        # Numbers (improved pattern)
        \b0[xX][0-9a-fA-F]+\b | # Hex: 0xFF, 0x1a
        \b0[oO][0-7]+\b       | # Octal: 0o777
        \b0[bB][01]+\b        | # Binary: 0b1010
        \b\d+[eE][+-]?\d+\b   | # Scientific: 1e10, 2.5e-3
        (?:\.\d+|\d+(?:\.\d*)?) | # Numbers: 123, 45.67, .89, 3.14

        \b[a-zA-Z_]\w*\b | # Identifiers: variable_name, function_name

        # Multi-character operators (longest first)
        \*\*             | # **
        //              | # //
        ==              | # ==
        !=              | # !=
        <=              | # <=
        >=              | # >=
        \+=             | # +=
        -=              | # -=
        \*=             | # *=
        /=              | # /=
        %=              | # %=
        &=              | # &=
        \|=             | # |=
        \^=             | # ^=
        <<=             | # <<=
        >>=             | # >>=
        <<              | # <<
        >>              | # >>
        ->              | # ->
        \.\.\.          | # ...

        # Single character operators and punctuation
        [=+\-*/%&|^<>!?:.,;(){}\[\]]
    """
