import re
from typing import List, Union, Optional
from dataclasses import dataclass
from . import tokentypes

"""
Enhanced tokenizer with line-based token organization.

This module provides a tokenizer that organizes tokens by line,
allowing both flat token access and line-based token access.
"""


@dataclass
class Token:
    type: tokentypes.TokenType
    value: str
    pos: int

    def __str__(self):
        return f"Token(value = {self.value}, type = {self.type}, position = {self.pos})"


class Line:
    def __init__(self, line_str: str, pos: int) -> None:
        self.line_str = line_str
        self.pos = pos
        self.__re_pattern = r"""
                \"[^\"]*\"        | # Double quoted strings: "hello world"
                '[^']*'          | # Single quoted strings: 'hello world'
                \b\d+\.?\d*\b    | # Numbers: 123, 45.67, .89
                \b[a-zA-Z_]\w*\b | # Identifiers: variable_name, function_name
                [=+\-*/%&|^<>!]+ | # Operators: =, +, -, *, /, %, &, |, ^, <, >, !
                [(){}\[\],;]       # Punctuation: (, ), {, }, [, ], ,, ;
            """
        self.tokens = []
        i = 0
        for w in re.findall(self.__re_pattern, line_str, re.VERBOSE):
            if w.strip():
                tk = Token(tokentypes.set(w.strip()), w.strip(), i)
                self.tokens.append(tk)
            i += 1


class Block:
    def __init__(self, block_str: str) -> None:
        self.block_str = block_str
        self.block = []
        i = 0
        for line_str in self.block_str.split("/n"):
            Line(line_str, i)
            i += 1
