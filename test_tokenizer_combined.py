#!/usr/bin/env python3
import sys
sys.path.insert(0, '.')

from src.jade_project_JOERICKS1998.utils import tokenizer
from src.jade_project_JOERICKS1998.utils import tokentypes

def test_token_classification():
    test_cases = [
        ("if x == 5:", [
            ("if", tokentypes.TokenType.IF),
            ("x", tokentypes.TokenType.IDENTIFIER),
            ("==", tokentypes.TokenType.EQUALS),
            ("5", tokentypes.TokenType.NUMBER),
            (":", tokentypes.TokenType.COLON),
        ]),
        ('print("hello")', [
            ("print", tokentypes.TokenType.IDENTIFIER),
            ("(", tokentypes.TokenType.LPAREN),
            ('"hello"', tokentypes.TokenType.STRING),
            (")", tokentypes.TokenType.RPAREN),
        ]),
        ("x += y * 2", [
            ("x", tokentypes.TokenType.IDENTIFIER),
            ("+=", tokentypes.TokenType.PLUS_ASSIGN),
            ("y", tokentypes.TokenType.IDENTIFIER),
            ("*", tokentypes.TokenType.MULTIPLY),
            ("2", tokentypes.TokenType.NUMBER),
        ]),
        ("prompt my_var = 'What is 2+2?'", [
            ("prompt", tokentypes.TokenType.PROMPT),
            ("my_var", tokentypes.TokenType.IDENTIFIER),
            ("=", tokentypes.TokenType.ASSIGN),
            ("'What is 2+2?'", tokentypes.TokenType.STRING),
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
        
        if len(tokens) != len(expected):
            print(f"  ERROR: Expected {len(expected)} tokens, got {len(tokens)}")
            print(f"  Tokens: {[(t.value, t.type) for t in tokens]}")
            continue
            
        all_correct = True
        for i, (token, (expected_value, expected_type)) in enumerate(zip(tokens, expected)):
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