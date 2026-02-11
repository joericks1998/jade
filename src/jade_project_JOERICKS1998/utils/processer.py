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


def is_jade_line(line: parser.Line, heap: heap.Heap) -> bool:
    """Determine if a line requires Jade processing rather than standard Python pass-through."""
    return any(
        token.Type == tokenref.Types.PROMPT
        or token.Type == tokenref.Types.PROMPTDREF
        or (token.Type == tokenref.Types.IDENTIFIER and token.Value in heap.prompts)
        for token in line
    )


def process_1(line: parser.Line, heap: heap.Heap) -> str:
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
            line[i] = tokenref.Token(f"__p__{variable_name}", line[i].Pos)
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


def process_2(line: tokenref.Line, heap: heap.Heap) -> tokenref.Line:
    """
    Process prompt dereferences by releasing from the heap and replacing tokens.

    Replaces each ?var pair with a single token containing the LLM response.

    Args:
        tokens: Token list containing prompt dereferences
        heap: Heap instance containing stored prompts

    Returns:
        Modified token list with dereferences resolved
    """
    new_line = parser.Line("", line.Pos)
    i = 0
    while i < len(line):
        if line[i].Type == tokenref.Types.PROMPTDREF:
            if i + 1 < len(line):
                response = heap.release(line[i + 1].Value)
                new_line.append(tokenref.Token(f'\"\"\"{response.Clean}\"\"\"', line[i].Pos))
                i += 2
            else:
                raise IndexError(f"PROMPTDREF at position {i} has no following identifier")
        else:
            new_line.append(line[i])
            i += 1
    return new_line


def process_3(line: parser.Line, heap: heap.Heap) -> parser.Line:
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
    new_line = parser.Line("", pos=line.Pos)
    for token in line:
        if token.Type == tokenref.Types.IDENTIFIER and token.Value in heap.prompts:
            new_line.append(tokenref.Token(f"__p__{token.Value}", token.Pos))
        else:
            new_line.append(token)
    return new_line


def line_interpreter(line_of_tokens: parser.Line, heap: heap.Heap) -> str:
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


def _resolve_tokens(line: parser.Line, heap: heap.Heap) -> str:
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


def machine(jade_code_string: str, python_buffer: Buffer, heap: heap.Heap) -> None:
    """
    Main compilation pipeline that converts Jade source code to executable Python.

    This function orchestrates the entire compilation process:
    1. Preprocesses the source by encoding special characters
    2. Tokenizes the entire code into a block of lines
    3. Processes each line, translating Jade syntax to Python
    4. Writes the translated code to the output buffer

    The function handles both pure Python lines (passed through) and Jade-specific
    lines (processed through the interpreter).

    Args:
        jade_code_string: Raw Jade source code as a string
        python_buffer: Buffer to accumulate translated Python code
        heap: Heap for managing LLM prompts and responses

    Returns:
        None (results are written to python_buffer)
    """
    # Step 1: Tokenize all of the code in the file
    try:
        # preprocess code string
        preprocessed_space_code_str = jade_code_string
        for k, v in constants.SPACE_ENCODINGS.items():
            preprocessed_space_code_str = preprocessed_space_code_str.replace(k, v)
        token_block = parser.Block(preprocessed_space_code_str)
    except Exception as e:
        print(f"Error tokenizing Jade code: {e}")
        return
    # Step 2: Go line by line and based on tokens and types, translate and add these tokens to the buffer
    try:
        for line in token_block:
            jade_output = line_interpreter(line, heap)
            # Postprocess jade output to decode space encodings
            jade_output = parser.decode_encodings(jade_output, constants.SPACE_ENCODINGS)
            python_buffer.write(jade_output)
    except Exception as e:
        print(f"Error processing Jade code: {e}")
        return
    # If a prompt arises, handle via the heap first then write the output to the buffer
    return