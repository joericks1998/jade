"""
Jade code processing and compilation engine.

This module contains the core logic for processing Jade source code, including:
- Buffer management for intermediate Python code
- Jade-specific token processing (prompts and dereferences)
- Line-by-line interpretation and translation
- Main compilation pipeline that converts Jade to executable Python
"""

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


def process_1(line_of_tokens: parser.Line, heap: heap.Heap) -> str:
    """
    Process a prompt declaration and store it in the heap.

    This handles Jade's 'prompt' keyword syntax, which declares a variable
    that will hold an LLM-generated response. The prompt is stored in the heap
    for later retrieval and LLM invocation.

    Example Jade syntax: `prompt my_var "Generate a greeting"`

    Args:
        line_of_tokens: Tokenized line containing a prompt declaration
        heap: Heap instance for storing prompt information

    Returns:
        Python code string that creates a placeholder variable
    """
    variable_name = ""
    prompt = ""
    for token in line_of_tokens:
        if token.Type == tokenref.Types.IDENTIFIER:
            variable_name = token.Value
        elif token.Type == tokenref.Types.STRING:
            prompt = token.Value
    heap.add(variable_name, prompt)
    return f"__p__{variable_name} = {prompt}"

def process_2(line_of_tokens: parser.Line, heap: heap.Heap) -> str:
    """
    Process a prompt dereference and inject LLM-generated content.

    This handles Jade's '?' operator, which retrieves a prompt from the heap,
    sends it to the LLM, and injects the cleaned response into the code.

    Example Jade syntax: `result = ?my_var`

    Args:
        line_of_tokens: Tokenized line containing a prompt dereference
        heap: Heap instance containing stored prompts

    Returns:
        Python code string with LLM response injected as a triple-quoted string

    Raises:
        IndexError: If a PROMPTDREF token has no following identifier
    """
    output_str = ""
    i = 0
    while i < len(line_of_tokens):
        if line_of_tokens[i].Type == tokenref.Types.PROMPTDREF:
            if i + 1 < len(line_of_tokens):
                # Retrieve LLM response from heap and inject it
                response = heap.release(line_of_tokens[i+1].Value)
                output_str += f'\"\"\"{response.Clean}\"\"\"'
                i += 2
            else:
                raise IndexError(f"PROMPTDREF at position {i} has no following identifier")
        else:
            output_str += line_of_tokens[i].Value
            i+=1
    return output_str


def line_interpreter(line_of_tokens: parser.Line, heap: heap.Heap) -> str:
    """
    Interpret a line containing Jade-specific syntax.

    Routes the line to the appropriate processor based on token types:
    - Lines with PROMPT tokens -> process_1 (declare and store prompt)
    - Lines with PROMPTDREF tokens -> process_2 (dereference and inject LLM response)

    Args:
        line_of_tokens: Tokenized line to interpret
        heap: Heap instance for prompt storage and retrieval

    Returns:
        Python code string representing the translated line

    Raises:
        Exception: Re-raises any exceptions from processing functions with context
    """
    try:
        if tokenref.Types.PROMPT in [token.Type for token in line_of_tokens]:
            return process_1(line_of_tokens, heap)
        elif tokenref.Types.PROMPTDREF in [token.Type for token in line_of_tokens]:
            return process_2(line_of_tokens, heap)
    except Exception as e:
        print(f"Error interpreting line {line_of_tokens.Pos}: {e}")
        raise
    return ""


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
            if line.is_jade():
                jade_output = line_interpreter(line, heap)
                # Postprocess jade output to decode space encodings
                for k, v in constants.SPACE_ENCODINGS.items():
                    jade_output = jade_output.replace(v, k)
                python_buffer.write(jade_output)
            else:
                py_line = "".join(line.TokenValues)
                postprocessed_py_line = py_line
                for k, v in constants.SPACE_ENCODINGS.items():
                    postprocessed_py_line = postprocessed_py_line.replace(v, k)
                python_buffer.write(postprocessed_py_line)
    except Exception as e:
        print(f"Error processing Jade code: {e}")
        return
    # If a prompt arises, handle via the heap first then write the output to the buffer
    return
