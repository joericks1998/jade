from jade_project_JOERICKS1998.llm.deepseek import DeepSeekClient

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
    print(f"Starting compilation of: {source_file}")

    # Initialize Deepseek client
    print("  Initializing DeepSeek client...")
    client = DeepSeekClient()
    # Define heap
    print("  Creating heap...")
    prompt_heap = heap.Heap(client)

    # Initialize Jade Buffer
    print("  Initializing Jade Buffer...")
    buffer = processer.Buffer()

    # Check if the file has the correct Jade extension
    try:
        # Open and read the source file
        with open(source_file, "rb") as file:
            code_bytes = file.read()
            code_string = code_bytes.decode("utf-8")
        processer.machine(code_string, buffer, prompt_heap)
        # Execute the Jade/Python code directly
        # Note: This uses Python's exec() function which executes
        # the code in the current context
        print(buffer.out_py)

    except Exception as e:
        # Handle any exceptions that occur during code execution
        print(f"Compiler execution failed with exception: {e}")

    print(f"✅ Compilation completed for: {source_file}")
    return
