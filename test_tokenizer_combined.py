#!/usr/bin/env python3
import sys

sys.path.insert(0, ".")

from src.jade_project_JOERICKS1998.utils import parser, tokenref


def test_token_classification():
    test_cases = [
        (
            "if x == 5:",
            [
                ("if", tokens.TokenType.IF),
                ("x", tokens.TokenType.IDENTIFIER),
                ("==", tokens.TokenType.EQUALS),
                ("5", tokens.TokenType.NUMBER),
                (":", tokens.TokenType.COLON),
            ],
        ),
        (
            'print("hello")',
            [
                ("print", tokens.TokenType.IDENTIFIER),
                ("(", tokens.TokenType.LPAREN),
                ('"hello"', tokens.TokenType.STRING),
                (")", tokens.TokenType.RPAREN),
            ],
        ),
        (
            "x += y * 2",
            [
                ("x", tokens.TokenType.IDENTIFIER),
                ("+=", tokens.TokenType.PLUS_ASSIGN),
                ("y", tokens.TokenType.IDENTIFIER),
                ("*", tokens.TokenType.MULTIPLY),
                ("2", tokens.TokenType.NUMBER),
            ],
        ),
        (
            "prompt my_var = 'What is 2+2?'",
            [
                ("prompt", tokens.TokenType.PROMPT),
                ("my_var", tokens.TokenType.IDENTIFIER),
                ("=", tokens.TokenType.ASSIGN),
                ("'What is 2+2?'", tokens.TokenType.STRING),
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

        if len(tokens) != len(expected):
            print(f"  ERROR: Expected {len(expected)} tokens, got {len(tokens)}")
            print(f"  Tokens: {[(t.value, t.type) for t in tokens]}")
            continue

        all_correct = True
        for i, (token, (expected_value, expected_type)) in enumerate(
            zip(tokens, expected)
        ):
            if token.value != expected_value or token.type != expected_type:
                print(f"  ERROR at token {i}:")
                print(f"    Expected: ({expected_value}, {expected_type})")
                print(f"    Got: ({token.value}, {token.type})")
                all_correct = False

        if all_correct:
            print(f"  ✓ All tokens correct")
            for token in tokens:
                print(f"    {token.value}: {token.type}")


if __name__ == "__main__":
    test_token_classification()
