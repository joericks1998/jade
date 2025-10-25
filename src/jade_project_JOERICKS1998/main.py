import sys

from .utils import compiler, setup


def main() -> None:
    """
    Main entry point for the Jade command-line interface.

    This function handles command-line arguments and provides multiple modes of operation:
    - Compile and execute Jade source files (.jde extension)
    - Setup and configuration commands for LLM integration
    - Help and welcome messages for new users

    It provides user-friendly error messages for common issues like missing files
    or incorrect usage.

    Usage:
        jade <filename.jde>    - Compile and execute a Jade file
        jade setup llm         - Configure LLM provider integration
        jade setup --help      - Show setup help information
        jade setup             - Display welcome message and overview

    Args:
        None (uses sys.argv for command-line arguments)

    Returns:
        None

    Raises:
        SystemExit: If incorrect number of arguments provided or invalid command used

    Note:
        The setup commands allow users to configure LLM providers for enhanced
        features and integration within the Jade programming environment.
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
