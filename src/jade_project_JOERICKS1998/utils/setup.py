import keyring
from typing import Optional
from jade_project_JOERICKS1998.constants import constants


def llm_setup():
    """
    Setup function for LLM provider configuration.

    Prompts user for LLM provider details and stores them securely using keyring.
    """
    supported_models = list(constants.SUPPORTED_MODELS.keys())
    print(
        f"""
        The current list of supported LLM providers for jade are: {"/n - ".join(supported_models)}
        """
    )
    llm_provider = input(
        "Please input your preferred LLM provider from the list above: "
    )
    while llm_provider not in supported_models:
        print("Model provided is not supported...")
        llm_provider = input(
            "Please input your preferred LLM provider from the list above: "
        )
    username = input("Please enter your username for your given provider: ")
    api_key = input("Please enter the API key for your provider: ")

    # Store credentials securely
    keyring.set_password(
        "jade_llm",
        f"{username}@{llm_provider}",
        api_key,
    )
    print(f"✅ LLM configuration saved for {username} at {llm_provider}")


def general():
    """
    Display a comprehensive welcome message for the Jade programming language.
    """
    # isort: skip
    msg = """
    ╔══════════════════════════════════════════════════════════════════════════════╗
    ║                            🎉 WELCOME TO JADE! 🎉                           ║
    ║                  Your Gateway to Elegant Programming                         ║
    ╚══════════════════════════════════════════════════════════════════════════════╝

    🌟 About Jade:
    Jade is a modern, expressive programming language designed for clarity and
    productivity. Built with Python at its core, Jade combines the power of
    traditional programming with the elegance of modern language design.

    ✨ Key Features:
    • 🚀 Lightning-fast compilation and execution
    • 🐍 Seamless Python interoperability
    • 📚 Clean, readable syntax
    • 🔧 Extensible architecture
    • 🤖 Built-in LLM integration for enhanced development
    • 🎯 File-based execution with .jde extension

    📖 Getting Started:
    1. Create a .jde file with your Jade code
    2. Run: jade your_file.jde
    3. Watch your code come to life!

    💡 Example Usage:
    $ echo "print('Hello, Jade!')" > hello.jde
    $ jade hello.jde
    Hello, Jade!

    🔧 Available Commands:
    • jade setup llm    - Configure LLM provider
    • jade setup help   - Show help information
    • jade <file.jde>   - Compile and execute Jade code

    📚 Documentation & Support:
    • GitHub: https://github.com/joericks1998/jade
    • Issues: Report bugs and feature requests
    • Community: Join our growing developer community

    🎯 Your Journey Starts Here!
    Whether you're building simple scripts or complex applications, Jade provides
    the tools and elegance you need to bring your ideas to reality.

    Happy coding with Jade! 🎊
    """
    print(msg)


def get_llm_api_key(username: str, provider: str) -> Optional[str]:
    """
    Retrieve the LLM API key for a specific user and provider.

    Args:
        username (str): The username for the LLM provider
        provider (str): The LLM provider name

    Returns:
        Optional[str]: The API key if found, None otherwise
    """
    service_name = "jade_llm"
    key_name = f"{username}@{provider}"

    try:
        api_key = keyring.get_password(service_name, key_name)
        return api_key
    except Exception as e:
        print(f"Error retrieving API key: {e}")
        return None


def help():
    """
    Display help information for Jade setup commands.
    """
    # isort: skip
    help_msg = """
        Jade Setup Help
        ═══════════════

        Available Commands:
        • general() - Display welcome message and Jade overview
        • llm_setup() - Configure LLM provider for enhanced features
        • get_llm_api_key() - Retrieve stored API key for LLM integration
        • help() - Show this help message

        Usage Examples:
        >>> from jade_project_JOERICKS1998.utils.setup import general
        >>> general()  # Shows welcome message

        >>> from jade_project_JOERICKS1998.utils.setup import llm_setup
        >>> llm_setup()  # Configure LLM integration

        >>> from jade_project_JOERICKS1998.utils.setup import get_llm_api_key
        >>> api_key = get_llm_api_key("your_username", "your_provider")

        For more information, visit: https://github.com/joericks1998/jade
    """
    print(help_msg)
