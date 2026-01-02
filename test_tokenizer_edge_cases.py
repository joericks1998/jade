#!/usr/bin/env python3
import sys

sys.path.insert(0, ".")

from src.jade_project_JOERICKS1998.utils import parser, tokenref


def test_edge_cases():
    test_cases = [
        (
            "x = 3.14",
            [
                ("x", tokens.TokenType.IDENTIFIER),
                ("=", tokens.TokenType.ASSIGN),
                ("3.14", tokens.TokenType.NUMBER),
            ],
        ),
        (
            "x = .5",
            [
                ("x", tokens.TokenType.IDENTIFIER),
                ("=", tokens.TokenType.ASSIGN),
                (
                    ".5",
                    tokens.TokenType.NUMBER,
                ),  # Currently fails - regex doesn't match .5
            ],
        ),
        (
            "x = 1e10",
            [
                ("x", tokens.TokenType.IDENTIFIER),
                ("=", tokens.TokenType.ASSIGN),
                (
                    "1e10",
                    tokens.TokenType.NUMBER,
                ),  # Currently fails - no scientific notation
            ],
        ),
        (
            "x = 0xFF",
            [
                ("x", tokens.TokenType.IDENTIFIER),
                ("=", tokens.TokenType.ASSIGN),
                ("0xFF", tokens.TokenType.NUMBER),  # Currently fails - no hex
            ],
        ),
        (
            "x = [1, 2, 3]",
            [
                ("x", tokens.TokenType.IDENTIFIER),
                ("=", tokens.TokenType.ASSIGN),
                ("[", tokens.TokenType.LBRACKET),
                ("1", tokens.TokenType.NUMBER),
                (",", tokens.TokenType.COMMA),
                ("2", tokens.TokenType.NUMBER),
                (",", tokens.TokenType.COMMA),
                ("3", tokens.TokenType.NUMBER),
                ("]", tokens.TokenType.RBRACKET),
            ],
        ),
        (
            "def foo(x: int) -> int:",
            [
                ("def", tokens.TokenType.DEF),
                ("foo", tokens.TokenType.IDENTIFIER),
                ("(", tokens.TokenType.LPAREN),
                ("x", tokens.TokenType.IDENTIFIER),
                (":", tokens.TokenType.COLON),
                ("int", tokens.TokenType.IDENTIFIER),
                (")", tokens.TokenType.RPAREN),
                ("->", tokens.TokenType.ARROW),
                ("int", tokens.TokenType.IDENTIFIER),
                (":", tokens.TokenType.COLON),
            ],
        ),
        (
            "x = ?my_prompt",
            [
                ("x", tokens.TokenType.IDENTIFIER),
                ("=", tokens.TokenType.ASSIGN),
                ("?", tokens.TokenType.PROMPTDREF),
                ("my_prompt", tokens.TokenType.IDENTIFIER),
            ],
        ),
    ]

    for source, expected in test_cases:
        print(f"\nTesting: {source}")
        block = parser.Block(source)
        if not block.block:
            print(f"  ERROR: No lines parsed")
            continue

        line = block.block[0]
        tokens = line.tokens

        print(f"  Parsed {len(tokens)} tokens:")
        for i, token in enumerate(tokens):
            print(f"    [{i}] {token.value}: {token.type}")

        # Check against expected
        if len(tokens) != len(expected):
            print(f"  WARNING: Expected {len(expected)} tokens, got {len(tokens)}")
        else:
            all_correct = True
            for i, (token, (expected_value, expected_type)) in enumerate(
                zip(tokens, expected)
            ):
                if token.value != expected_value or token.type != expected_type:
                    print(f"  MISMATCH at token {i}:")
                    print(f"    Expected: ({expected_value}, {expected_type})")
                    print(f"    Got: ({token.value}, {token.type})")
                    all_correct = False

            if all_correct:
                print(f"  ✓ All tokens match expected")


if __name__ == "__main__":
    test_edge_cases()
