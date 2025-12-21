#!/usr/bin/env python3
import sys
sys.path.insert(0, '.')

from src.jade_project_JOERICKS1998.utils import tokenizer
from src.jade_project_JOERICKS1998.utils import tokentypes

def test_edge_cases():
    test_cases = [
        ("x = 3.14", [
            ("x", tokentypes.TokenType.IDENTIFIER),
            ("=", tokentypes.TokenType.ASSIGN),
            ("3.14", tokentypes.TokenType.NUMBER),
        ]),
        ("x = .5", [
            ("x", tokentypes.TokenType.IDENTIFIER),
            ("=", tokentypes.TokenType.ASSIGN),
            (".5", tokentypes.TokenType.NUMBER),  # Currently fails - regex doesn't match .5
        ]),
        ("x = 1e10", [
            ("x", tokentypes.TokenType.IDENTIFIER),
            ("=", tokentypes.TokenType.ASSIGN),
            ("1e10", tokentypes.TokenType.NUMBER),  # Currently fails - no scientific notation
        ]),
        ("x = 0xFF", [
            ("x", tokentypes.TokenType.IDENTIFIER),
            ("=", tokentypes.TokenType.ASSIGN),
            ("0xFF", tokentypes.TokenType.NUMBER),  # Currently fails - no hex
        ]),
        ("x = [1, 2, 3]", [
            ("x", tokentypes.TokenType.IDENTIFIER),
            ("=", tokentypes.TokenType.ASSIGN),
            ("[", tokentypes.TokenType.LBRACKET),
            ("1", tokentypes.TokenType.NUMBER),
            (",", tokentypes.TokenType.COMMA),
            ("2", tokentypes.TokenType.NUMBER),
            (",", tokentypes.TokenType.COMMA),
            ("3", tokentypes.TokenType.NUMBER),
            ("]", tokentypes.TokenType.RBRACKET),
        ]),
        ("def foo(x: int) -> int:", [
            ("def", tokentypes.TokenType.DEF),
            ("foo", tokentypes.TokenType.IDENTIFIER),
            ("(", tokentypes.TokenType.LPAREN),
            ("x", tokentypes.TokenType.IDENTIFIER),
            (":", tokentypes.TokenType.COLON),
            ("int", tokentypes.TokenType.IDENTIFIER),
            (")", tokentypes.TokenType.RPAREN),
            ("->", tokentypes.TokenType.ARROW),
            ("int", tokentypes.TokenType.IDENTIFIER),
            (":", tokentypes.TokenType.COLON),
        ]),
        ("x = ?my_prompt", [
            ("x", tokentypes.TokenType.IDENTIFIER),
            ("=", tokentypes.TokenType.ASSIGN),
            ("?", tokentypes.TokenType.PROMPTDREF),
            ("my_prompt", tokentypes.TokenType.IDENTIFIER),
        ]),
    ]
    
    for source, expected in test_cases:
        print(f"\nTesting: {source}")
        block = tokenizer.Block(source)
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
            for i, (token, (expected_value, expected_type)) in enumerate(zip(tokens, expected)):
                if token.value != expected_value or token.type != expected_type:
                    print(f"  MISMATCH at token {i}:")
                    print(f"    Expected: ({expected_value}, {expected_type})")
                    print(f"    Got: ({token.value}, {token.type})")
                    all_correct = False
            
            if all_correct:
                print(f"  ✓ All tokens match expected")

if __name__ == "__main__":
    test_edge_cases()