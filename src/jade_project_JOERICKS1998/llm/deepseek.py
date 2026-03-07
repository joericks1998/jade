"""
Enhanced DeepSeek LLM integration module with message handling.

This module provides functionality to interact with DeepSeek's API
with built-in conversation management and message history tracking.
"""

import getpass
import json
import os
from dataclasses import dataclass, field
from typing import Dict, Generator, List, Optional

import keyring
import requests


@dataclass
class Message:
    """Represents a single message in a conversation."""

    role: str  # "system", "user", "assistant"
    content: str
    timestamp: Optional[float] = None

    def to_dict(self) -> Dict[str, str]:
        """Convert message to API-compatible dictionary."""
        return {"role": self.role, "content": self.content}


@dataclass
class Conversation:
    """Manages a conversation with message history and context."""
    # test commit

    messages: List[Message] = field(default_factory=list)
    system_prompt: Optional[str] = None
    max_history: int = 50  # Maximum number of messages to keep

    def __post_init__(self):
        """Initialize conversation with system prompt if provided."""
        if self.system_prompt and not any(
            msg.role == "system" for msg in self.messages
        ):
            self.add_message("system", self.system_prompt)

    def add_message(self, role: str, content: str) -> None:
        """Add a message to the conversation."""
        message = Message(role=role, content=content)
        self.messages.append(message)

        # Trim history if exceeds maximum
        if len(self.messages) > self.max_history:
            # Keep system message and recent messages
            system_msg = next(
                (msg for msg in self.messages if msg.role == "system"), None
            )
            self.messages = (
                [system_msg] + self.messages[-self.max_history + 1 :]
                if system_msg
                else self.messages[-self.max_history :]
            )

    def add_user_message(self, content: str) -> None:
        """Add a user message to the conversation."""
        self.add_message("user", content)

    def add_assistant_message(self, content: str) -> None:
        """Add an assistant message to the conversation."""
        self.add_message("assistant", content)

    def get_api_messages(self) -> List[Dict[str, str]]:
        """Get messages in API-compatible format."""
        return [msg.to_dict() for msg in self.messages]

    def clear_history(self, keep_system: bool = True) -> None:
        """Clear conversation history, optionally keeping system message."""
        if keep_system:
            system_msg = next(
                (msg for msg in self.messages if msg.role == "system"), None
            )
            self.messages = [system_msg] if system_msg else []
        else:
            self.messages = []

    def get_last_user_message(self) -> Optional[Message]:
        """Get the last user message."""
        for msg in reversed(self.messages):
            if msg.role == "user":
                return msg
        return None

    def get_last_assistant_message(self) -> Optional[Message]:
        """Get the last assistant message."""
        for msg in reversed(self.messages):
            if msg.role == "assistant":
                return msg
        return None


class DeepSeekClient:
    """
    Enhanced client for DeepSeek's LLM API with conversation management.

    This class provides:
    - Secure credential management via keyring
    - Conversation state management
    - Message history tracking
    - Utility methods for common LLM tasks
    """

    def __init__(self, service_name: str = "jade_deepseek"):
        """
        Initialize the DeepSeek client.

        Args:
            service_name (str): Service identifier for keyring storage
        """
        self.service_name = service_name
        self.base_url = "https://api.deepseek.com/v1"
        self.api_key = self._get_api_key()
        self.active_conversation: Optional[Conversation] = None
        self.active_model: str = "deepseek-chat"
        self.total_tokens: int = 0
        self.total_prompt_tokens: int = 0
        self.total_completion_tokens: int = 0
        self._pending_usage: Dict = {}

    def _get_api_key(self) -> str:
        """
        Retrieve the DeepSeek API key from keyring or environment.

        Returns:
            str: The DeepSeek API key

        Raises:
            ValueError: If no API key can be found or obtained
        """
        # Try to get API key from keyring first
        api_key = keyring.get_password(self.service_name, "api_key")
        if api_key:
            return api_key

        # Try environment variable as fallback
        api_key = os.getenv("DEEPSEEK_API_KEY")
        if api_key:
            self._store_api_key(api_key)
            print("DeepSeek API key already configured!")
            return api_key

        # If no key found, prompt user securely
        api_key = getpass.getpass(
            "DeepSeek API key not found.\nPlease enter your DeepSeek API key (input will be hidden): "
        ).strip()

        if not api_key:
            raise ValueError("No DeepSeek API key provided")

        self._store_api_key(api_key)
        return api_key

    def _store_api_key(self, api_key: str) -> None:
        """Store the API key securely in the system keyring."""
        keyring.set_password(self.service_name, "api_key", api_key)
        print("API key stored securely in system keyring.")

    def is_configured(self) -> bool:
        """
        Check if DeepSeek API key is configured without prompting user.

        Returns:
            bool: True if API key exists in keyring or environment variable
        """
        # Check keyring first
        if keyring.get_password(self.service_name, "api_key"):
            return True
        # Check environment variable
        if os.getenv("DEEPSEEK_API_KEY"):
            return True
        return False

    def _make_request(self, endpoint: str, data: Dict) -> Dict:
        """
        Make an authenticated request to the DeepSeek API.

        Returns:
            Dict: API response data

        Raises:
            requests.RequestException: If the API request fails
        """
        url = f"{self.base_url}/{endpoint}"
        headers = {
            "Authorization": f"Bearer {self.api_key}",
            "Content-Type": "application/json",
        }

        try:
            response = requests.post(url, headers=headers, json=data, timeout=30)
            response.raise_for_status()
            return response.json()
        except requests.RequestException as e:
            error_msg = f"API request failed: {e}"
            if hasattr(e, "response") and e.response is not None:
                try:
                    error_data = e.response.json()
                    error_msg = f"API Error: {error_data.get('error', {}).get('message', str(e))}"
                except:
                    error_msg = f"API Error: {e.response.text}"
            raise requests.RequestException(error_msg) from e

    def _make_streaming_request(self, endpoint: str, data: Dict) -> Generator[str, None, None]:
        """
        Make a streaming request to the DeepSeek API, yielding text chunks.

        Parses the SSE (Server-Sent Events) response line by line. If the
        final chunk contains usage statistics (requires stream_options.include_usage
        in the request), they are stashed in self._pending_usage for the caller.

        Yields:
            str: Text content from each delta chunk as it arrives
        """
        url = f"{self.base_url}/{endpoint}"
        headers = {
            "Authorization": f"Bearer {self.api_key}",
            "Content-Type": "application/json",
        }

        try:
            response = requests.post(url, headers=headers, json=data, stream=True, timeout=60)
            response.raise_for_status()

            for raw_line in response.iter_lines():
                if not raw_line:
                    continue
                line = raw_line.decode("utf-8")
                if not line.startswith("data: "):
                    continue
                payload = line[6:]
                if payload == "[DONE]":
                    break
                try:
                    chunk = json.loads(payload)
                    if "usage" in chunk and chunk["usage"]:
                        self._pending_usage = chunk["usage"]
                    content = chunk["choices"][0]["delta"].get("content", "")
                    if content:
                        yield content
                except (json.JSONDecodeError, KeyError, IndexError):
                    continue

        except requests.RequestException as e:
            error_msg = f"API request failed: {e}"
            if hasattr(e, "response") and e.response is not None:
                try:
                    error_data = e.response.json()
                    error_msg = f"API Error: {error_data.get('error', {}).get('message', str(e))}"
                except Exception:
                    error_msg = f"API Error: {e.response.text}"
            raise requests.RequestException(error_msg) from e

    def stream_message(
        self,
        message: str,
        conversation: Optional[Conversation] = None,
        model: str = "deepseek-chat",
        temperature: float = 0.7,
        max_tokens: Optional[int] = None,
    ) -> Generator[str, None, None]:
        """
        Send a message and stream the assistant's response token by token.

        Yields text chunks as they arrive. After the stream is exhausted the
        full accumulated response is added to conversation history and token
        usage counters are updated from the final SSE chunk.

        Args:
            message: User message to send
            conversation: Conversation to use (uses active conversation if None)
            model: Model to use for completion
            temperature: Sampling temperature
            max_tokens: Maximum tokens in response

        Yields:
            str: Text chunks of the assistant response
        """
        conv = conversation or self.active_conversation
        if conv is None:
            conv = self.start_conversation()

        conv.add_user_message(message)
        api_messages = conv.get_api_messages()

        data = {
            "model": model,
            "messages": api_messages,
            "temperature": temperature,
            "stream": True,
            "stream_options": {"include_usage": True},
        }
        if max_tokens is not None:
            data["max_tokens"] = max_tokens

        self._pending_usage = {}
        accumulated = ""

        for chunk in self._make_streaming_request("chat/completions", data):
            accumulated += chunk
            yield chunk

        # Stream exhausted — update conversation history and token counts
        conv.add_assistant_message(accumulated)
        self.active_model = model
        usage = self._pending_usage
        self.total_prompt_tokens += usage.get("prompt_tokens", 0)
        self.total_completion_tokens += usage.get("completion_tokens", 0)
        self.total_tokens += usage.get("total_tokens", 0)

    def start_conversation(self, system_prompt: Optional[str] = None) -> Conversation:
        """
        Start a new conversation.

        Args:
            system_prompt (str, optional): Initial system prompt

        Returns:
            Conversation: The new conversation object
        """
        self.active_conversation = Conversation(system_prompt=system_prompt)
        return self.active_conversation

    def chat_completion(
        self,
        messages: List[Dict[str, str]],
        model: str = "deepseek-chat",
        temperature: float = 0.7,
        max_tokens: Optional[int] = None,
        stream: bool = False,
    ) -> Dict:
        """
        Create a chat completion using DeepSeek's API.

        Args:
            messages: List of message objects with 'role' and 'content'
            model: DeepSeek model to use
            temperature: Sampling temperature (0.0 to 1.0)
            max_tokens: Maximum tokens to generate
            stream: Whether to stream the response

        Returns:
            Dict: Completion response containing the generated text
        """
        data = {
            "model": model,
            "messages": messages,
            "temperature": temperature,
            "stream": stream,
        }

        if max_tokens is not None:
            data["max_tokens"] = max_tokens

        return self._make_request("chat/completions", data)

    def send_message(
        self,
        message: str,
        conversation: Optional[Conversation] = None,
        model: str = "deepseek-chat",
        temperature: float = 0.7,
        max_tokens: Optional[int] = None,
    ) -> str:
        """
        Send a message and get the assistant's response.

        Args:
            message: The user message to send
            conversation: Conversation to use (uses active conversation if None)
            model: Model to use for completion
            temperature: Sampling temperature
            max_tokens: Maximum tokens in response

        Returns:
            str: The assistant's response text
        """
        conv = conversation or self.active_conversation

        if conv is None:
            conv = self.start_conversation()

        # Add user message to conversation
        conv.add_user_message(message)

        # Get API-compatible messages
        api_messages = conv.get_api_messages()

        # Make API call
        response = self.chat_completion(
            messages=api_messages,
            model=model,
            temperature=temperature,
            max_tokens=max_tokens,
        )

        # Extract assistant response
        assistant_response = response["choices"][0]["message"]["content"]

        # Accumulate token usage
        usage = response.get("usage", {})
        self.total_prompt_tokens += usage.get("prompt_tokens", 0)
        self.total_completion_tokens += usage.get("completion_tokens", 0)
        self.total_tokens += usage.get("total_tokens", 0)
        self.active_model = model

        # Add assistant response to conversation
        conv.add_assistant_message(assistant_response)

        return assistant_response

    def quick_chat(
        self,
        prompt: str,
        system_message: Optional[str] = None,
        model: str = "deepseek-chat",
        temperature: float = 0.7,
        max_tokens: Optional[int] = None,
    ) -> str:
        """
        Quick one-off chat without conversation history.

        Args:
            prompt: User prompt/message
            system_message: Optional system message
            model: Model to use
            temperature: Sampling temperature
            max_tokens: Maximum tokens in response

        Returns:
            str: Generated response text
        """
        messages = []
        if system_message:
            messages.append({"role": "system", "content": system_message})
        messages.append({"role": "user", "content": prompt})

        response = self.chat_completion(
            messages=messages,
            model=model,
            temperature=temperature,
            max_tokens=max_tokens,
        )

        return response["choices"][0]["message"]["content"]

    def continue_conversation(
        self,
        conversation: Optional[Conversation] = None,
        model: str = "deepseek-chat",
        temperature: float = 0.7,
        max_tokens: Optional[int] = None,
    ) -> str:
        """
        Continue the conversation based on the last message.

        This is useful for when you want the AI to continue its thought
        or provide more details without a new user message.

        Returns:
            str: The continued response
        """
        conv = conversation or self.active_conversation

        if conv is None:
            raise ValueError("No active conversation to continue")

        if not conv.messages:
            raise ValueError("Conversation is empty")

        # Use the last message as context
        api_messages = conv.get_api_messages()

        response = self.chat_completion(
            messages=api_messages,
            model=model,
            temperature=temperature,
            max_tokens=max_tokens,
        )

        assistant_response = response["choices"][0]["message"]["content"]
        conv.add_assistant_message(assistant_response)

        return assistant_response

    def get_available_models(self) -> List[str]:
        """Get list of available DeepSeek models."""
        try:
            response = self._make_request("models", {})
            return [model["id"] for model in response.get("data", [])]
        except requests.RequestException:
            # Fallback to known models if API call fails
            return ["deepseek-chat", "deepseek-coder"]

    def clear_credentials(self) -> None:
        """Remove stored API credentials from keyring."""
        try:
            keyring.delete_password(self.service_name, "api_key")
            print("API credentials cleared from keyring.")
        except keyring.errors.PasswordDeleteError:
            print("No credentials found to clear.")

    def test_connection(self) -> bool:
        """Test the connection to DeepSeek API."""
        try:
            self.get_available_models()
            return True
        except requests.RequestException:
            return False


# Convenience functions
def create_chat_completion(
    prompt: str, system_message: Optional[str] = None, **kwargs
) -> str:
    """
    Convenience function for creating a simple chat completion.

    Args:
        prompt: User prompt/message
        system_message: Optional system message
        **kwargs: Additional arguments for chat_completion

    Returns:
        str: Generated response text
    """
    client = DeepSeekClient()
    return client.quick_chat(prompt, system_message, **kwargs)


def create_conversation(
    system_prompt: Optional[str] = None,
) -> tuple[DeepSeekClient, Conversation]:
    """
    Create a new client and conversation.

    Returns:
        tuple: (client, conversation) for easy chaining
    """
    client = DeepSeekClient()
    conversation = client.start_conversation(system_prompt)
    return client, conversation


def setup_deepseek() -> None:
    """Interactive setup function for DeepSeek integration."""
    print("=== DeepSeek LLM Setup ===")
    print("This will help you set up DeepSeek API integration.")
    print("You'll need a DeepSeek API key from https://platform.deepseek.com/")
    print("Note: Your API key input will be hidden for security.")
    print()

    client = DeepSeekClient()

    if client.test_connection():
        print("✅ DeepSeek integration is working correctly!")
        models = client.get_available_models()
        print(f"Available models: {', '.join(models)}")
    else:
        print("❌ Unable to connect to DeepSeek API.")
        print("Please check your API key and try again.")
