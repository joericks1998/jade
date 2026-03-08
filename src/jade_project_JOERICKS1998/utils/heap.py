"""
Heap management for Jade LLM prompt storage and retrieval.

This module provides the Heap class which manages the lifecycle of LLM prompts
during Jade code execution. Prompts are stored when declared and released
(executed via LLM) when dereferenced in the code.
"""

import inspect

from ..llm.deepseek import DeepSeekClient
from . import parser


class PromptOverflowError(Exception):
    """Raised when a typed dereference exhausts all retry attempts without a valid coercion."""


_PRIMITIVE_HINTS: dict = {
    int:   "Respond with only a plain integer (e.g. 42), no other text.",
    float: "Respond with only a plain decimal number (e.g. 3.14), no other text.",
    bool:  "Respond with only True or False, no other text.",
    str:   "Respond with only a plain string value, no other text.",
}


def _build_schema_hint(output_type) -> str:
    if output_type in _PRIMITIVE_HINTS:
        return _PRIMITIVE_HINTS[output_type]
    try:
        sig = inspect.signature(output_type.__init__)
        params = [p for p in sig.parameters if p != "self"]
        param_desc = params[0] if params else "value"
    except (ValueError, TypeError):
        param_desc = "value"
    return (
        f"Your entire response will be passed as a string to {output_type.__name__}({param_desc}=...). "
        f"Respond with only the value, no explanation or extra text."
    )


def _build_correction(output_type, error: Exception) -> str:
    return (
        f"Your previous response could not be converted to {output_type.__name__}: {error}. "
        f"Please try again. {_build_schema_hint(output_type)}"
    )


def _coerce(raw: str, output_type):
    cleaned = raw.strip()
    if output_type is bool:
        if cleaned.lower() == "true":
            return True
        if cleaned.lower() == "false":
            return False
        raise ValueError(f"Cannot convert {cleaned!r} to bool — expected 'True' or 'False'")
    return output_type(cleaned)


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
        self.retry_log: list[dict] = []
        self.max_retries: int = 3

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

    def ask_typed(self, prompt_text: str, output_type) -> any:
        """
        Call the LLM and coerce the response into output_type via its constructor.

        Injects a schema hint into the first prompt, then retries with a correction
        message on each parse failure. Retry turns are stripped from conversation
        history after resolution. Any call that required at least one retry is
        appended to self.retry_log.

        Args:
            prompt_text: The prompt string to send to the LLM
            output_type: A callable (class or primitive) used to coerce the raw response

        Returns:
            output_type(raw_response.strip())

        Raises:
            PromptOverflowError: If all retry attempts are exhausted without a valid coercion
        """
        max_retries = max(self.max_retries, 1)
        augmented = prompt_text + "\n\n" + _build_schema_hint(output_type)
        failed_responses: list[str] = []
        last_error: Exception = None
        conv = self.client.active_conversation
        raw = ""

        for attempt in range(max_retries):
            try:
                if attempt == 0:
                    raw = self.client.send_message(augmented)
                else:
                    raw = self.client.send_message(_build_correction(output_type, last_error))

                result = _coerce(raw, output_type)

                if attempt > 0:
                    # Strip the retry turns from conversation history:
                    # keep the first user message and the final assistant message,
                    # remove the 2*attempt messages in between.
                    if conv is not None:
                        del conv.messages[-(attempt * 2 + 1):-1]
                    self.retry_log.append({
                        "prompt":   prompt_text,
                        "type":     output_type.__name__,
                        "attempts": attempt + 1,
                        "success":  True,
                        "failed":   failed_responses,
                    })

                return result

            except Exception as e:
                failed_responses.append(raw)
                last_error = e

        # All attempts exhausted — strip every message added during this call
        if conv is not None:
            del conv.messages[-(max_retries * 2):]
        self.retry_log.append({
            "prompt":   prompt_text,
            "type":     output_type.__name__,
            "attempts": max_retries,
            "success":  False,
            "failed":   failed_responses,
        })
        raise PromptOverflowError(
            f"Could not coerce LLM output to {output_type.__name__} after {max_retries} attempts"
        )

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
