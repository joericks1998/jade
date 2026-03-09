"""
Heap management for Jade LLM prompt storage and retrieval.

This module provides the Heap class which manages the lifecycle of LLM prompts
during Jade code execution. Prompts are stored when declared and released
(executed via LLM) when dereferenced in the code.
"""

from ..llm.deepseek import DeepSeekClient
from . import parser


class RetryLimitExceeded(Exception):
    """Raised when a typed dereference exhausts all retry attempts without a valid coercion."""




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
        self.max_retries: int = 15
        self.retry_log: list[dict] = []

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
        """Clear conversation history and reset token counters."""
        conv = self.client.active_conversation
        if conv is not None:
            conv.clear_history()
        self.client.total_tokens = 0
        self.client.total_prompt_tokens = 0
        self.client.total_completion_tokens = 0

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
            accumulated = ""
            for chunk in self.client.stream_message(prompt_text):
                accumulated += chunk
            return parser.LLMOutput(accumulated).Text
        except Exception as e:
            print(f"\n  [Jade] Request failed: {e}")
            return "[Request failed — please try again]"

    def stream(self, prompt_text: str) -> None:
        """
        Stream a dynamic prompt response directly to stdout, token by token.

        Called when the Jade program uses ``print(?p)`` — the compiler replaces
        the entire print call with this method so tokens are printed progressively
        rather than buffered and displayed all at once.

        Args:
            prompt_text: The prompt string to send to the LLM
        """
        try:
            for chunk in self.client.stream_message(prompt_text):
                print(chunk, end="", flush=True)
            print()
        except Exception as e:
            print(f"\n  [Jade] Request failed: {e}")

    def _coerce(self, raw: str, output_type, attempts: int = 0, _failed: list = None):
        if _failed is None:
            _failed = []
        cleaned = raw.strip()
        error = None
        if output_type is bool:
            if cleaned.lower() == "true":
                return True
            if cleaned.lower() == "false":
                return False
            error = ValueError(f"Cannot convert {cleaned!r} to bool — expected 'True' or 'False'")
        else:
            try:
                return output_type(cleaned)
            except Exception as e:
                error = e
        _failed.append(raw)
        if attempts >= self.max_retries:
            raise RetryLimitExceeded(
                f"Could not coerce LLM response to {output_type.__name__} after {self.max_retries} attempts"
            ) from error
        return self._coerce(
            self.ask(f"{type(error).__name__}: failed to convert response to {output_type.__name__}. Respond with only the raw {output_type.__name__} value."),
            output_type,
            attempts + 1,
            _failed,
        )

    def ask_typed(self, prompt_text: str, output_type) -> any:
        failed = []
        try:
            raw = self.client.send_message(prompt_text)
            result = self._coerce(raw, output_type, _failed=failed)
            if failed:
                self.retry_log.append({
                    "prompt": prompt_text,
                    "type": output_type.__name__,
                    "attempts": len(failed) + 1,
                    "success": True,
                    "failed": failed,
                })
            return result
        except Exception:
            self.retry_log.append({
                "prompt": prompt_text,
                "type": output_type.__name__,
                "attempts": len(failed) + 1,
                "success": False,
                "failed": failed,
            })
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
