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


def _tokenize(encoded: str) -> list[tokenref.Token]:
    """
    Stateful character-level tokenizer for encoded Jade source lines.

    Walks the encoded string character by character, producing one Token
    per lexical unit. Every character is consumed into exactly one token,
    so joining all token Values reconstructs the original string.

    Args:
        encoded: Source string with whitespace encoded (␠, ␉, ␤)

    Returns:
        List of Token objects covering the entire input
    """
    tokens = []
    i = 0
    n = len(encoded)

    while i < n:
        ch = encoded[i]

        # Encoded whitespace
        if ch == '\u2420':  # ␠ space
            tokens.append(tokenref.Token(ch, i, tokenref.Types.SPACE))
            i += 1
        elif ch == '\u2409':  # ␉ tab
            tokens.append(tokenref.Token(ch, i, tokenref.Types.INDENT))
            i += 1
        elif ch == '\u2424':  # ␤ newline
            tokens.append(tokenref.Token(ch, i, tokenref.Types.NEWLINE))
            i += 1

        # f-string triple-double-quoted: f"""..."""
        elif ch == 'f' and encoded[i+1:i+4] == '"""':
            j = i + 4
            while j < n and encoded[j:j+3] != '"""':
                j += 1
            j += 3
            tokens.append(tokenref.Token(encoded[i:j], i, tokenref.Types.F))
            i = j

        # f-string triple-single-quoted: f'''...'''
        elif ch == 'f' and encoded[i+1:i+4] == "'''":
            j = i + 4
            while j < n and encoded[j:j+3] != "'''":
                j += 1
            j += 3
            tokens.append(tokenref.Token(encoded[i:j], i, tokenref.Types.F))
            i = j

        # Triple double-quoted string: """..."""
        elif encoded[i:i+3] == '"""':
            j = i + 3
            while j < n and encoded[j:j+3] != '"""':
                j += 1
            j += 3
            tokens.append(tokenref.Token(encoded[i:j], i, tokenref.Types.TRIPLEQ))
            i = j

        # Triple single-quoted string: '''...'''
        elif encoded[i:i+3] == "'''":
            j = i + 3
            while j < n and encoded[j:j+3] != "'''":
                j += 1
            j += 3
            tokens.append(tokenref.Token(encoded[i:j], i, tokenref.Types.TRIPLEQ))
            i = j

        # f-string double-quoted: f"..."
        elif ch == 'f' and i + 1 < n and encoded[i+1] == '"':
            j = i + 2
            while j < n:
                if encoded[j] == '\\':
                    j += 2
                elif encoded[j] == '"':
                    j += 1
                    break
                else:
                    j += 1
            tokens.append(tokenref.Token(encoded[i:j], i, tokenref.Types.F))
            i = j

        # f-string single-quoted: f'...'
        elif ch == 'f' and i + 1 < n and encoded[i+1] == "'":
            j = i + 2
            while j < n:
                if encoded[j] == '\\':
                    j += 2
                elif encoded[j] == "'":
                    j += 1
                    break
                else:
                    j += 1
            tokens.append(tokenref.Token(encoded[i:j], i, tokenref.Types.F))
            i = j

        # Regular double-quoted string: "..."
        elif ch == '"':
            j = i + 1
            while j < n:
                if encoded[j] == '\\':
                    j += 2
                elif encoded[j] == '"':
                    j += 1
                    break
                else:
                    j += 1
            tokens.append(tokenref.Token(encoded[i:j], i, tokenref.Types.STRING))
            i = j

        # Regular single-quoted string: '...'
        elif ch == "'":
            j = i + 1
            while j < n:
                if encoded[j] == '\\':
                    j += 2
                elif encoded[j] == "'":
                    j += 1
                    break
                else:
                    j += 1
            tokens.append(tokenref.Token(encoded[i:j], i, tokenref.Types.STRING))
            i = j

        # Comment: # until ␤
        elif ch == '#':
            j = i + 1
            while j < n and encoded[j] != '\u2424':
                j += 1
            tokens.append(tokenref.Token(encoded[i:j], i, tokenref.Types.COMMENT))
            i = j

        # Prompt dereference: ?
        elif ch == '?':
            tokens.append(tokenref.Token(ch, i, tokenref.Types.PROMPTDREF))
            i += 1

        # Identifiers and keywords
        elif ch.isalpha() or ch == '_':
            j = i + 1
            while j < n and (encoded[j].isalnum() or encoded[j] == '_'):
                j += 1
            word = encoded[i:j]
            tok_type = tokenref.KEYWORD_TYPES.get(word, tokenref.Types.IDENTIFIER)
            tokens.append(tokenref.Token(word, i, tok_type))
            i = j

        # Fallback: single character
        else:
            tokens.append(tokenref.Token(ch, i, tokenref.Types.FALLBACK))
            i += 1

    return tokens



class Chunk:
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
        self.Tokens = _tokenize(line_str)


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

    @property
    def AllTypes(self) -> list[tokenref.Types]:
        return [token.Type for token in self.Tokens]



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
        Initialize a Block by splitting encoded source into Chunks.

        Uses a stateful character-level walk to split on encoded newlines (␤)
        while treating triple-quoted string contents as opaque (no splits inside).

        Args:
            block_str: The complete encoded source code string to parse
        """
        self.chunks = []
        state = "NORMAL"
        buffer = ""
        chunk_index = 0
        i = 0
        n = len(block_str)

        while i < n:
            if state == "NORMAL" and block_str[i:i+3] == '"""':
                state = "TRIPLE_D"
                buffer += '"""'
                i += 3
            elif state == "NORMAL" and block_str[i:i+3] == "'''":
                state = "TRIPLE_S"
                buffer += "'''"
                i += 3
            elif state == "TRIPLE_D" and block_str[i:i+3] == '"""':
                state = "NORMAL"
                buffer += '"""'
                i += 3
            elif state == "TRIPLE_S" and block_str[i:i+3] == "'''":
                state = "NORMAL"
                buffer += "'''"
                i += 3
            elif state == "NORMAL" and block_str[i] == '\u2424':  # ␤
                buffer += '\u2424'
                self.chunks.append(Chunk(buffer, chunk_index))
                chunk_index += 1
                buffer = ""
                i += 1
            else:
                buffer += block_str[i]
                i += 1

        if buffer:
            self.chunks.append(Chunk(buffer, chunk_index))

    def __str__(self) -> str:
        """Return a string representation of the Block with all its chunks."""
        return f"Block(Chunks = [{', '.join(str(chunk) for chunk in self.chunks)}])"

    def __iter__(self):
        """Enable iteration over the chunks in this block."""
        return iter(self.chunks)
    
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
