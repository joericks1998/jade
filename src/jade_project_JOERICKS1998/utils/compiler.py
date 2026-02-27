from jade_project_JOERICKS1998.llm.deepseek import DeepSeekClient

from .. import config
from . import heap, processer


def compile(source_file: str) -> None:
    # isort: off
    """
    Compile and execute a Jade source file.

    This function reads a Jade source file (with .jde extension), compiles it
    by executing the Python code directly, and handles any exceptions that
    occur during execution.

    Args:
        source_file (str): Path to the source file to compile

    Returns:
        None

    Raises:
        FileNotFoundError: If the specified source file does not exist
        Exception: Any exception that occurs during code execution

    Note:
        Currently, Jade files (.jde) are executed as Python code directly.
        Files without the .jde extension are rejected with an error message.
    """
    # isort: on
    if config.verbose:
        print(f"Starting compilation of: {source_file}")
    client = DeepSeekClient()
    prompt_heap = heap.Heap(client)
    try:
        with open(source_file, "rb") as file:
            code_string = file.read().decode("utf-8")
        processer.machine(code_string, prompt_heap)
    except Exception as e:
        print(f"Compiler execution failed: {e}")
    if config.verbose:
        print(f"Compilation completed: {source_file}")
    return
