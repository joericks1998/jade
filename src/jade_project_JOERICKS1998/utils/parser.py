import re

from ..constants.constants import MASTER_TOKEN_PATTERN
from . import tokenref

"""
Enhanced tokenizer with line-based token organization.

This module provides a tokenizer that organizes tokens by line,
allowing both flat token access and line-based token access.
"""


class Line:
    def __init__(self, line_str: str, pos: int) -> None:
        self.line_str = line_str
        self.pos = pos
        self.__re_pattern = MASTER_TOKEN_PATTERN
        self.tokens = []
        i = 0
        for lex in re.findall(self.__re_pattern, line_str, re.VERBOSE):
            self.tokens.append(tokenref.Token(lex, i))
            i += 1

    def __str__(self):
        return f"[{', '.join(str(tk) for tk in self.tokens)}]"


class Block:
    def __init__(self, block_str: str) -> None:
        self.block_str = block_str
        self.block = []
        i = 0
        for line_str in self.block_str.split("\n"):
            self.block.append(Line(line_str, i))
            i += 1
