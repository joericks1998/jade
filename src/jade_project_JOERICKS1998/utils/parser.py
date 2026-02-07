import re

from ..constants import constants
from . import tokenref

"""
Enhanced tokenizer with line-based token organization.

This module provides a tokenizer that organizes tokens by line,
allowing both flat token access and line-based token access.
"""


class Line:
    def __init__(self, line_str: str, pos: int) -> None:
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

    def __str__(self):
        return f"Line(tokens = [{', '.join(str(tk) for tk in self.Tokens)}], position = {self.Pos})"

    def __iter__(self):
        return iter(self.Tokens)

    def __getitem__(self, index):
        return self.Tokens[index]

    def __len__(self):
        return len(self.Tokens)

    @property
    def TokenValues(self):
        return [token.Value for token in self.Tokens]

    def is_jade(self):
        for token in self.Tokens:
            if tokenref.jade_switch.get(token.Type):
                return True


class Block:
    def __init__(self, block_str: str) -> None:
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

    def __str__(self):
        return f"Block(Lines = [{', '.join(str(line) for line in self.lines)}])"

    def __iter__(self):
        return iter(self.lines)
    
class LLMOutput:
    def __init__(self, llm_str: str) -> None:
        self.original_string = llm_str

    @property
    def Clean(self) -> str:
        """Clean LLM output by keeping only alphanumerics and standard keyboard symbols."""
        cleaned_string = self.original_string

        # Remove control characters (except newline, tab, carriage return)
        cleaned_string = re.sub(r'[\x00-\x08\x0B\x0C\x0E-\x1F\x7F]', '', cleaned_string)

        # Remove invisible Unicode characters
        cleaned_string = re.sub(r'[\u200B-\u200F\uFEFF\u00A0]', '', cleaned_string)

        # Keep only alphanumerics, whitespace, and common keyboard symbols
        cleaned_string = re.sub(r'[^a-zA-Z0-9\s\.\,\!\?\'\"\-\_\(\)\[\]\{\}\<\>\/\\\|\@\#\$\%\^\&\*\+\=\~\`\;\:]', '', cleaned_string)

        return cleaned_string.strip()
