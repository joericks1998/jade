import sys

from .utils import command_line, compiler


def main() -> None:
    # isort: off
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
    # isort: on
    # Get and clean user input args from the command line
    command_line.input_handler(*sys.argv[1:])
    return


if __name__ == "__main__":
    # This allows the file to be run directly as a script
    # Note: When installed as a package, the entry point is defined in pyproject.toml
    main()
