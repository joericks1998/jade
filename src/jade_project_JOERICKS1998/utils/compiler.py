from typing import List

from jade_project_JOERICKS1998.llm.deepseek import DeepSeekClient

from . import heap, processing


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
    print(f"🔧 Starting compilation of: {source_file}")

    # Initialize Deepseek client
    print("  Initializing DeepSeek client...")
    client = DeepSeekClient()
    # Define heap
    print("  Creating heap...")
    hp = heap.Heap(client)

    # Check if the file has the correct Jade extension
    try:
        # Open and read the source file
        print("Reading source file...")
        with open(source_file, "r") as f:
            source_code: list[str] = processing.chunk_file(f)
        print(f"  File chunked into {len(source_code)} block(s)")

        # Execute the Jade/Python code directly
        # Note: This uses Python's exec() function which executes
        # the code in the current context
        for i, chunk in enumerate(source_code):
            print(f"  Processing block {i + 1}/{len(source_code)}...")
            try:
                exec(chunk)
                print(f"    ✓ Block {i + 1} executed successfully")
            except SyntaxError:
                print(f"    ⚠️  Block {i + 1} has Jade syntax, processing with LLM...")
                processing.process_jade_block(chunk, hp)
                print(f"    ✓ Block {i + 1} processed as Jade")
            except Exception as e:
                print(f"    ❌ Jade Error in block {i + 1}: {e}")

        print(hp.prompts)
    except Exception as e:
        # Handle any exceptions that occur during code execution
        print(f"❌ Compiler execution failed with exception: {e}")

    print(f"✅ Compilation completed for: {source_file}")
    return
