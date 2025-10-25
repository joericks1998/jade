import sys

from .utils import compiler, setup


def main() -> None:
    """
    Main entry point for the Jade command-line interface.

    This function handles command-line arguments and executes the Jade compiler
    on the specified source file. It provides user-friendly error messages
    for common issues like missing files or incorrect usage.

    Usage:
        jade <filename.jde>

    Args:
        None (uses sys.argv for command-line arguments)

    Returns:
        None

    Raises:
        SystemExit: If incorrect number of arguments provided
    """
    # Validate command-line arguments
    if len(sys.argv) != 2:
        print("Usage: jade <filename.jde>")
        sys.exit(1)

    # Extract filename from command-line arguments
    if sys.argv[1] == "setup":
        try:
            if sys.argv[2] == "llm":
                setup.llm_setup()
            elif sys.argv[2] == "--help":
                setup.help()
        except Exception:
            setup.general()
    else:
        filename = sys.argv[1]

    try:
        # Attempt to compile the specified file
        compiler.compile(filename)
    except FileNotFoundError:
        # Handle case where the specified file doesn't exist
        print(f"File not found: {filename}")
    except Exception as e:
        # Handle any other unexpected errors during compilation
        print(f"Error reading file: {e}")


if __name__ == "__main__":
    # This allows the file to be run directly as a script
    # Note: When installed as a package, the entry point is defined in pyproject.toml
    main()
