"""
Jade code processing and compilation engine.

This module contains the core logic for processing Jade source code, including:
- Buffer management for intermediate Python code
- Jade-specific token processing (prompts and dereferences)
- Line-by-line interpretation and translation
- Main compilation pipeline that converts Jade to executable Python
"""

from .. import config
from ..constants import constants
from . import heap, parser, tokenref


class Buffer:
    """
    Buffer for accumulating intermediate Python code during compilation.

    The Buffer collects translated Python code strings during the Jade
    compilation process and provides methods to write code incrementally
    and execute it when ready.

    Attributes:
        out_py (str): Accumulated Python code string
    """

    def __init__(self) -> None:
        """Initialize an empty Buffer."""
        self.out_py = ""

    def write(self, string: str) -> None:
        """
        Append a code string to the buffer.

        Args:
            string: Python code string to append
        """
        self.out_py += string

    def flush(self) -> None:
        """
        Execute the accumulated Python code and clear the buffer.

        Uses Python's exec() to run the compiled code with access to builtins.
        After execution, the buffer is cleared for potential reuse.
        """
        exec(self.out_py, {"__builtins__": __builtins__})
        self.out_py = ""


# Jade-specific token processing functions


def is_jade_line(line: parser.Chunk, heap: heap.Heap) -> bool:
    """Determine if a line requires Jade processing rather than standard Python pass-through."""
    return any(
        token.Type == tokenref.Types.PROMPT
        or token.Type == tokenref.Types.PROMPTDREF
        or (token.Type == tokenref.Types.IDENTIFIER and token.Value in heap.prompts)
        for token in line
    )


def process_1(line: parser.Chunk, heap: heap.Heap) -> str:
    """
    Process a prompt declaration and store it in the heap.

    Terminal operation — returns a Python code string.

    Example: `prompt my_var "Generate a greeting"` -> `__p__my_var = "Generate a greeting"`

    Args:
        tokens: Token list containing a prompt declaration
        heap: Heap instance for storing prompt information

    Returns:
        Python code string that creates a placeholder variable
    """
    prompt = ""
    variable_name = ""
    i = 0
    while i < len(line):
        if line[i].Type == tokenref.Types.IDENTIFIER:
            variable_name = line[i].Value
            line[i] = tokenref.Token(f"__p__{variable_name}", line[i].Pos, type=tokenref.Types.IDENTIFIER)
            i += 1
        elif line[i].Type == tokenref.Types.STRING:
            prompt = line[i].Value
            i += 1
        elif line[i].Type == tokenref.Types.PROMPT:
            del line[i]
            del line[i]
        else:
            i += 1
    heap.add(variable_name, prompt)
    return line


def process_2(line: parser.Chunk, heap: heap.Heap) -> parser.Chunk:
    """
    Process prompt dereferences by releasing from the heap and replacing tokens.

    Replaces each ?var pair with a single token containing the LLM response.

    Args:
        tokens: Token list containing prompt dereferences
        heap: Heap instance containing stored prompts

    Returns:
        Modified token list with dereferences resolved
    """
    new_line = parser.Chunk("", line.Pos)
    i = 0
    while i < len(line):
        if line[i].Type == tokenref.Types.PROMPTDREF:
            if i + 1 < len(line):
                response = heap.release(line[i + 1].Value)
                new_line.append(tokenref.Token(f'\"\"\"{response.Clean}\"\"\"', line[i].Pos, type=tokenref.Types.TRIPLEQ))
                i += 2
            else:
                raise IndexError(f"PROMPTDREF at position {i} has no following identifier")
        else:
            new_line.append(line[i])
            i += 1
    return new_line


def process_3(line: parser.Chunk, heap: heap.Heap) -> parser.Chunk:
    """
    Rewrite bare prompt identifiers to their __p__ prefixed variable names.

    No LLM call — just a variable name substitution.

    Example: `print(jade)` tokens -> `print(__p__jade)` tokens

    Args:
        tokens: Token list containing implicit prompt references
        heap: Heap instance containing stored prompts

    Returns:
        Modified token list with identifiers rewritten
    """
    new_line = parser.Chunk("", pos=line.Pos)
    for token in line:
        if token.Type == tokenref.Types.IDENTIFIER and token.Value in heap.prompts:
            new_line.append(tokenref.Token(f"__p__{token.Value}", token.Pos, type=tokenref.Types.IDENTIFIER))
        else:
            new_line.append(token)
    return new_line


def line_interpreter(line_of_tokens: parser.Chunk, heap: heap.Heap) -> str:
    """
    Recursively interpret a line containing Jade-specific syntax.

    Extracts the token list and applies transformations in priority order,
    recursing after each pass until no Jade operations remain:
    1. Release: ?var dereferences -> invoke LLM, replace with response tokens
    2. Variable lookup: bare identifier matching heap -> rewrite to __p__ prefix
    3. Declaration: prompt keyword -> store in heap (terminal)

    Args:
        line_of_tokens: Tokenized line to interpret
        heap: Heap instance for prompt storage and retrieval

    Returns:
        Python code string representing the translated line
    """
    return _resolve_tokens(line_of_tokens, heap)


def _resolve_tokens(line: parser.Chunk, heap: heap.Heap) -> str:
    """Recursive resolution of Jade tokens by priority order."""
    if config.verbose:
        print(f"{line.Pos}: {any(t.Type == tokenref.Types.IDENTIFIER and t.Value in heap.prompts for t in line.Tokens)}: {line.AllValues}")
    # Priority 1: Release — handle ?var dereferences
    if tokenref.Types.PROMPTDREF in line.AllTypes:
        return _resolve_tokens(process_2(line, heap), heap)

    # Priority 2: Variable lookup — rewrite bare identifiers matching heap
    if any(t.Type == tokenref.Types.IDENTIFIER and t.Value in heap.prompts for t in line.Tokens):

        return _resolve_tokens(process_3(line, heap), heap)

    # Priority 3: Declaration — store prompt in heap (terminal)
    if tokenref.Types.PROMPT in line.AllTypes:
        return _resolve_tokens(process_1(line, heap), heap)

    # Base case: no jade operations remain
    return "".join(line.AllValues)


_CONTINUATION_KEYWORDS = (
    'elif ', 'elif:', 'else:', 'else ', 'except:', 'except ',
    'finally:', 'finally ',
)

_INCOMPLETE_MSGS = (
    'expected an indented block',  # Python 3.10+ compound header without body
    'unexpected eof',              # Python 3.9 fallback
    'eof while',                   # Python 3.9 fallback
    'was never closed',            # unclosed bracket/paren/string
)


def _is_incomplete(e: SyntaxError) -> bool:
    """Return True if a SyntaxError represents incomplete (not yet wrong) input."""
    msg = (e.msg if hasattr(e, 'msg') else str(e)).lower()
    return any(pat in msg for pat in _INCOMPLETE_MSGS)


def machine(jade_code_string: str, heap: heap.Heap) -> None:
    """
    Main compilation pipeline that converts Jade source code to executable Python.

    Preprocesses the source, tokenizes into chunks, translates each chunk from
    Jade to Python, and executes incrementally using exec-mode compilation.

    Compilation is attempted only at unindented, non-blank, non-continuation
    lines so that multi-branch compound statements (if/elif/else, try/except)
    and function/class bodies containing blank lines accumulate fully before
    being executed.

    Args:
        jade_code_string: Raw Jade source code as a string
        heap: Heap for managing LLM prompts and responses
    """
    preprocessed = jade_code_string
    for k, v in constants.SPACE_ENCODINGS.items():
        preprocessed = preprocessed.replace(k, v)

    try:
        token_block = parser.Block(preprocessed)
    except Exception as e:
        print(f"Error tokenizing Jade code: {e}")
        return

    namespace = {"__builtins__": __builtins__}
    pending_py = ""
    all_py = ""

    for chunk in token_block:
        try:
            jade_output = line_interpreter(chunk, heap)
            py_line = parser.decode_encodings(jade_output, constants.SPACE_ENCODINGS)
        except Exception as e:
            print(f"Error processing line {chunk.Pos}: {e}")
            continue

        pending_py += py_line
        all_py += py_line

        stripped = py_line.strip()
        is_indented = py_line.startswith((' ', '\t'))
        is_blank = not stripped
        is_continuation = stripped.startswith(_CONTINUATION_KEYWORDS)

        # Only attempt compilation at top-level, non-blank, non-continuation lines.
        # - Indented lines belong to a block body still being built.
        # - Blank lines may appear inside compound statements (if/elif branches,
        #   function bodies) — deferring avoids splitting them prematurely.
        # - Continuation keywords (elif/else/except/finally) extend the current
        #   compound statement and must not trigger early execution.
        if not is_indented and not is_blank and not is_continuation:
            try:
                code = compile(pending_py, "<jade>", "exec")
                exec(code, namespace)
                pending_py = ""
            except SyntaxError as e:
                if not _is_incomplete(e):
                    print(f"SyntaxError at line {chunk.Pos}: {e}")
                    pending_py = ""
                # else: incomplete compound statement — keep accumulating

    if pending_py.strip():
        try:
            exec(compile(pending_py, "<jade>", "exec"), namespace)
        except Exception as e:
            print(f"Error executing remaining code: {e}")

    if config.show_python:
        print("Generated Python code:")
        print(all_py)