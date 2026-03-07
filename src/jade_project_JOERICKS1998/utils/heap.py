"""
Heap management for Jade LLM prompt storage and retrieval.

This module provides the Heap class which manages the lifecycle of LLM prompts
during Jade code execution. Prompts are stored when declared and released
(executed via LLM) when dereferenced in the code.
"""

from ..llm.deepseek import DeepSeekClient
from . import parser


class Heap:
    """
    Storage and execution manager for LLM prompts in Jade code.

    The Heap stores prompt declarations and handles their execution through
    an LLM client when they are dereferenced. Each prompt is identified by
    a variable name and executed exactly once.

    Attributes:
        prompts (dict[str, str]): Mapping of variable names to prompt strings
        client (DeepSeekClient): LLM client for executing prompts
    """

    def __init__(self, client: DeepSeekClient) -> None:
        """
        Initialize a Heap with an LLM client.

        Args:
            client: Configured DeepSeekClient for LLM interactions
        """
        self.prompts: dict[str, str] = {}
        self.client: DeepSeekClient = client

    @property
    def tokens(self) -> int:
        """Total tokens consumed this session."""
        return self.client.total_tokens

    @property
    def prompt_tokens(self) -> int:
        """Tokens sent in prompts this session."""
        return self.client.total_prompt_tokens

    @property
    def completion_tokens(self) -> int:
        """Tokens received in completions this session."""
        return self.client.total_completion_tokens

    @property
    def messages(self) -> list:
        """Current conversation message list."""
        conv = self.client.active_conversation
        return conv.messages if conv is not None else []

    @property
    def model(self) -> str:
        """Active LLM model name."""
        return self.client.active_model

    def clear(self) -> None:
        """Clear conversation history, keeping the system message if present."""
        conv = self.client.active_conversation
        if conv is not None:
            conv.clear_history()

    def add(self, var_name: str, prompt: str) -> None:
        """
        Store a prompt in the heap for later execution.

        Args:
            var_name: Variable name identifier for this prompt
            prompt: The prompt text to send to the LLM
        """
        self.prompts[var_name] = prompt

    def ask(self, prompt_text: str) -> str:
        """
        Call the LLM with a runtime prompt string and return the cleaned response.

        Used by dynamic prompts (``prompt p = expr``) where the prompt text is
        only known at execution time, not at translation time.

        Args:
            prompt_text: The prompt string to send to the LLM

        Returns:
            Cleaned LLM response as a plain string
        """
        try:
            response = self.client.send_message(prompt_text)
            return parser.LLMOutput(response).Text
        except Exception as e:
            print(f"Heap Error: {e}")
            raise

    def release(self, var_name: str) -> parser.LLMOutput:
        """
        Execute a stored prompt via LLM and remove it from the heap.

        This method retrieves the prompt, sends it to the LLM client,
        and returns the response wrapped in an LLMOutput object. The
        prompt is deleted from the heap after execution (single-use).

        Args:
            var_name: Variable name of the prompt to execute

        Returns:
            LLMOutput object containing the cleaned LLM response

        Raises:
            KeyError: If the variable name doesn't exist in the heap
            Exception: Re-raises any LLM client errors with context
        """
        try:
            response = self.client.send_message(self.prompts[var_name])
            del self.prompts[var_name]
            return parser.LLMOutput(response)
        except KeyError:
            msg = f"var_name {var_name} does not exist in the heap"
            raise KeyError(msg)
        except Exception as e:
            print(f"Heap Error:{e}")
            raise
