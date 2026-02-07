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
