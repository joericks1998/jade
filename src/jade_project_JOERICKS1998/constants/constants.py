"""
Global constants for the Jade language compiler.

This module defines configuration constants used throughout the Jade
compilation process, including:
- Supported LLM models and their API endpoints
- Special character encodings for tokenization
"""

# Supported LLM models for Jade prompt execution
# Maps model identifiers to their API base URLs
SUPPORTED_MODELS: dict[str, str] = {
    "deepseek-chat": "https://api.deepseek.com",
    "deepseek-reasoner": "https://api.deepseek.com",
}

# Special character encodings for tokenization
# Whitespace characters are temporarily encoded as Unicode symbols
# during tokenization to prevent regex splitting issues, then decoded
# back to their original form after processing
SPACE_ENCODINGS: dict[str, str] = {
    " ": "␠",  # Space -> Unicode symbol for space (U+2420)
    "\t": "␉",  # Tab -> Unicode symbol for tab (U+2409)
    "\n": "␤",  # Newline -> Unicode symbol for newline (U+2424)
}
