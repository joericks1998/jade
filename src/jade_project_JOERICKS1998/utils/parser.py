import re

from ..constants import constants
from . import tokenref

"""
Enhanced tokenizer with line-based token organization.

This module provides a tokenizer that organizes tokens by line,
allowing both flat token access and line-based token access.
"""


def decode_encodings(s: str, encodings: dict[str, str]) -> str:
    """Decode encoded symbols back to their original characters using the given encoding map."""
    for original, encoded in encodings.items():
        s = s.replace(encoded, original)
    return s


class Line:
    """
    Represents a single line of Jade source code with its tokenized content.

    This class tokenizes a line of source code into individual tokens and provides
    methods to access and iterate over these tokens. Each line tracks its position
    in the source file and can identify whether it contains Jade-specific syntax.

    Attributes:
        line_str (str): The original line string
        Pos (int): Line position in the source file (0-indexed)
        Tokens (List[tokenref.Token]): List of tokens parsed from the line
    """

    def __init__(self, line_str: str, pos: int) -> None:
        """
        Initialize a Line object by tokenizing the input string.

        Args:
            line_str: The source code line to tokenize
            pos: Position of this line in the source file (0-indexed)
        """
        self.line_str = line_str
        self.Pos = pos
        self.__re_pattern = "|".join(tokenref.map.keys())
        self.Tokens = []
        i = 0
        for lex in re.findall(self.__re_pattern, line_str, re.VERBOSE):
            try:
                self.Tokens.append(tokenref.Token(lex, i))
                i += 1
            except Exception as e:
                print(f"Error tokenizing position {i}: {e}")

    def __str__(self) -> str:
        """Return a string representation of the Line with its tokens and position."""
        return f"Line(tokens = [{', '.join(str(tk) for tk in self.Tokens)}], position = {self.Pos})"

    def __iter__(self):
        """Enable iteration over the tokens in this line."""
        return iter(self.Tokens)

    def __getitem__(self, index: int) -> tokenref.Token:
        """
        Get a token by its index in the line.

        Args:
            index: The index of the token to retrieve

        Returns:
            The token at the specified index
        """
        return self.Tokens[index]

    def __len__(self) -> int:
        """Return the number of tokens in this line."""
        return len(self.Tokens)

    def __setitem__(self, index: int, token: tokenref.Token) -> None:
        """Set a token at the given index."""
        self.Tokens[index] = token

    def __delitem__(self, index: int) -> None:
        del self.Tokens[index]

    def append(self, token: tokenref.Token) -> None:
        """Append a token to this line."""
        self.Tokens.append(token)

    @property
    def AllValues(self) -> list[str]:
        """
        Get a list of token values (strings) without type information.

        Returns:
            List of string values for all tokens in the line
        """
        return [token.Value for token in self.Tokens]

    def is_jade(self) -> bool:
        """
        Check if this line contains Jade-specific syntax.

        Jade-specific syntax includes the 'prompt' keyword and '?' prompt dereference operator.

        Returns:
            True if the line contains Jade tokens, False otherwise
        """
        for token in self.Tokens:
            if tokenref.jade_switch.get(token.Type):
                return True
        return False


class Block:
    """
    Represents a block of Jade source code containing multiple lines.

    A Block parses the entire source code string into individual Line objects,
    splitting on newline characters. It provides iteration and string representation
    for debugging and analysis.

    Attributes:
        block_str (str): The original source code block string
        lines (List[Line]): List of parsed Line objects
    """

    def __init__(self, block_str: str) -> None:
        """
        Initialize a Block by parsing source code into lines.

        The block is split using encoded newline characters from SPACE_ENCODINGS
        to handle special characters in the tokenization process.

        Args:
            block_str: The complete source code string to parse
        """
        self.block_str = block_str
        self.lines = []
        i = 0
        split_char = constants.SPACE_ENCODINGS["\n"]
        for line_str in re.split(re.compile(f"({split_char})"), self.block_str):
            try:
                self.lines.append(Line(line_str, i))
                i += 1
            except Exception as e:
                print(f"Error processing line {i}: {e}")

    def __str__(self) -> str:
        """Return a string representation of the Block with all its lines."""
        return f"Block(Lines = [{', '.join(str(line) for line in self.lines)}])"

    def __iter__(self):
        """Enable iteration over the lines in this block."""
        return iter(self.lines)
    
class LLMOutput:
    """
    Wrapper for LLM-generated output with cleaning and sanitization utilities.

    This class processes raw LLM responses to remove unwanted characters,
    control sequences, and invisible Unicode that could interfere with
    code execution or display.

    Attributes:
        original_string (str): The unmodified LLM response text
    """

    def __init__(self, llm_str: str) -> None:
        """
        Initialize an LLMOutput wrapper.

        Args:
            llm_str: The raw output string from the LLM
        """
        self.original_string = llm_str

    @property
    def Clean(self) -> str:
        """
        Clean LLM output by removing control characters and non-standard symbols.

        This property performs three sanitization steps:
        1. Removes control characters (except newline, tab, carriage return)
        2. Removes invisible Unicode characters (zero-width spaces, etc.)
        3. Keeps only alphanumerics, whitespace, and standard keyboard symbols

        Returns:
            Sanitized string safe for code execution and display
        """
        cleaned_string = self.original_string

        # Escape backslashes to prevent invalid escape sequences in generated Python
        cleaned_string = cleaned_string.replace('\\', '\\\\')

        # Remove control characters (except newline, tab, carriage return)
        cleaned_string = re.sub(r'[\x00-\x08\x0B\x0C\x0E-\x1F\x7F]', '', cleaned_string)

        # Remove invisible Unicode characters
        cleaned_string = re.sub(r'[\u200B-\u200F\uFEFF\u00A0]', '', cleaned_string)

        # Keep only alphanumerics, whitespace, and common keyboard symbols
        cleaned_string = re.sub(r'[^a-zA-Z0-9\s\.\,\!\?\'\"\-\_\(\)\[\]\{\}\<\>\/\\\|\@\#\$\%\^\&\*\+\=\~\`\;\:]', '', cleaned_string)

        return cleaned_string.strip()
