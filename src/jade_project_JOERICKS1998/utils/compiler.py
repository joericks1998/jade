def compile(source_file: str) -> None:
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
    # Check if the file has the correct Jade extension
    if ".jde" in source_file:
        try:
            # Open and read the source file
            with open(source_file, "r") as f:
                source_code = f.read()

            # Execute the Jade/Python code directly
            # Note: This uses Python's exec() function which executes
            # the code in the current context
            exec(source_code)

        except Exception as e:
            # Handle any exceptions that occur during code execution
            print(f"Code execution failed with exception: {e}")

        return
    else:
        # Reject files that don't have the Jade extension
        print(f"Source file {source_file} is not a jade file, cannot be compiled...")
