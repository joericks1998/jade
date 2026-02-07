"""
Token definitions and lexical analysis for the Jade language.

This module defines:
- Token types for all Jade and Python keywords, operators, and punctuation
- Regular expression patterns for matching tokens
- Token class for representing individual lexical elements
- Jade-specific token identifiers

The tokenizer supports both standard Python syntax and Jade-specific
extensions like the 'prompt' keyword and '?' dereference operator.
"""

import re
from enum import Enum, auto


class Types(Enum):
    """
    Enumeration of all token types in the Jade language.

    This enum includes:
    - Python keywords (if, for, class, def, etc.)
    - Operators (arithmetic, comparison, bitwise, assignment)
    - Literals (identifiers, numbers, strings)
    - Punctuation (parentheses, brackets, commas, etc.)
    - Special tokens (newline, indent, comment)
    - Jade-specific tokens (PROMPT, PROMPTDREF)
    """

    # Python-like keywords
    IF = auto()
    ELSE = auto()
    ELIF = auto()
    FOR = auto()
    WHILE = auto()
    BREAK = auto()
    CONTINUE = auto()
    PASS = auto()
    DEF = auto()
    CLASS = auto()
    RETURN = auto()
    YIELD = auto()
    LAMBDA = auto()
    IMPORT = auto()
    FROM = auto()
    AS = auto()
    TRY = auto()
    EXCEPT = auto()
    FINALLY = auto()
    RAISE = auto()
    ASSERT = auto()
    WITH = auto()
    GLOBAL = auto()
    NONLOCAL = auto()
    AND = auto()
    OR = auto()
    NOT = auto()
    IN = auto()
    IS = auto()
    TRUE = auto()
    FALSE = auto()
    NONE = auto()
    ASYNC = auto()
    AWAIT = auto()
    MATCH = auto()
    CASE = auto()

    # Operators
    ASSIGN = auto()  # =
    PLUS = auto()  # +
    MINUS = auto()  # -
    MULTIPLY = auto()  # *
    DIVIDE = auto()  # /
    MODULO = auto()  # %
    POWER = auto()  # **
    FLOOR_DIV = auto()  # //

    # Comparison
    EQUALS = auto()  # ==
    NOT_EQUALS = auto()  # !=
    LESS = auto()  # <
    GREATER = auto()  # >
    LESS_EQUAL = auto()  # <=
    GREATER_EQUAL = auto()  # >=

    # Logical
    AND_OP = auto()  # and
    OR_OP = auto()  # or
    NOT_OP = auto()  # not

    # Bitwise
    BIT_AND = auto()  # &
    BIT_OR = auto()  # |
    BIT_XOR = auto()  # ^
    BIT_NOT = auto()  # ~
    LEFT_SHIFT = auto()  # <<
    RIGHT_SHIFT = auto()  # >>

    # Assignment operators
    PLUS_ASSIGN = auto()  # +=
    MINUS_ASSIGN = auto()  # -=
    MULT_ASSIGN = auto()  # *=
    DIV_ASSIGN = auto()  # /=
    MOD_ASSIGN = auto()  # %=
    AND_ASSIGN = auto()  # &=
    OR_ASSIGN = auto()  # |=
    XOR_ASSIGN = auto()  # ^=
    LEFT_SHIFT_ASSIGN = auto()  # <<=
    RIGHT_SHIFT_ASSIGN = auto()  # >>=

    # Literals
    IDENTIFIER = auto()
    NUMBER = auto()
    STRING = auto()
    FSTRING = auto()  # Intermediate: captured whole, then expanded
    FPREFIX = auto()  # f-string prefix: f
    FSTRING_START = auto()  # f-string opening quote + text before first {
    FSTRING_MID = auto()  # f-string text between } and {
    FSTRING_END = auto()  # f-string text after last } + closing quote

    # Punctuation
    LPAREN = auto()  # (
    RPAREN = auto()  # )
    LBRACE = auto()  # {
    RBRACE = auto()  # }
    LBRACKET = auto()  # [
    RBRACKET = auto()  # ]
    COMMA = auto()  # ,
    SEMICOLON = auto()  # ;
    COLON = auto()  # :
    DOT = auto()  # .
    ELLIPSIS = auto()  # ...
    ARROW = auto()  # ->

    # Special
    NEWLINE = auto()
    INDENT = auto()
    DEDENT = auto()
    SPACE = auto()

    # Jade Tokens
    PROMPT = auto()
    PROMPTDREF = auto()

    # Null type (for no matches)
    NULL = auto()
    FALLBACK = auto()

    # Comment
    COMMENT = auto()

    # Empty token
    EMPTY = auto()


# Token pattern mapping for lexical analysis
# Maps regular expression patterns to their corresponding token types
# NOTE: Order matters! More specific patterns should come before general ones
# to ensure correct tokenization (e.g., '**' before '*', '<=' before '<')
map = {
    # Keywords (with word boundaries to prevent matching inside identifiers)
    r"\bif\b": Types.IF,
    r"\belse\b": Types.ELSE,
    r"\belif\b": Types.ELIF,
    r"\bfor\b": Types.FOR,
    r"\bwhile\b": Types.WHILE,
    r"\bbreak\b": Types.BREAK,
    r"\bcontinue\b": Types.CONTINUE,
    r"\bpass\b": Types.PASS,
    r"\bdef\b": Types.DEF,
    r"\bclass\b": Types.CLASS,
    r"\breturn\b": Types.RETURN,
    r"\byield\b": Types.YIELD,
    r"\blambda\b": Types.LAMBDA,
    r"\bimport\b": Types.IMPORT,
    r"\bfrom\b": Types.FROM,
    r"\bas\b": Types.AS,
    r"\btry\b": Types.TRY,
    r"\bexcept\b": Types.EXCEPT,
    r"\bfinally\b": Types.FINALLY,
    r"\braise\b": Types.RAISE,
    r"\bassert\b": Types.ASSERT,
    r"\bwith\b": Types.WITH,
    r"\bglobal\b": Types.GLOBAL,
    r"\bnonlocal\b": Types.NONLOCAL,
    r"\band\b": Types.AND,
    r"\bor\b": Types.OR,
    r"\bnot\b": Types.NOT,
    r"\bin\b": Types.IN,
    r"\bis\b": Types.IS,
    r"\bTrue\b": Types.TRUE,
    r"\bFalse\b": Types.FALSE,
    r"\bNone\b": Types.NONE,
    r"\basync\b": Types.ASYNC,
    r"\bawait\b": Types.AWAIT,
    r"\bmatch\b": Types.MATCH,
    r"\bcase\b": Types.CASE,
    r"\bprompt\b": Types.PROMPT,  # Jade-specific: prompt declaration
    r"\?": Types.PROMPTDREF,  # Jade-specific: prompt dereference operator
    # Operators (multi-character operators before single-character)
    r"\=": Types.ASSIGN,
    r"\+": Types.PLUS,
    r"\-": Types.MINUS,
    r"\*": Types.MULTIPLY,
    r"/": Types.DIVIDE,
    r"%": Types.MODULO,
    r"\*\*": Types.POWER,
    r"//": Types.FLOOR_DIV,
    r"==": Types.EQUALS,
    r"!=": Types.NOT_EQUALS,
    r"<": Types.LESS,
    r">": Types.GREATER,
    r"<=": Types.LESS_EQUAL,
    r">=": Types.GREATER_EQUAL,
    r"&": Types.BIT_AND,
    r"\|": Types.BIT_OR,
    r"\^": Types.BIT_XOR,
    r"~": Types.BIT_NOT,
    r"<<": Types.LEFT_SHIFT,
    r">>": Types.RIGHT_SHIFT,
    r"\+=": Types.PLUS_ASSIGN,
    r"\-=": Types.MINUS_ASSIGN,
    r"\*=": Types.MULT_ASSIGN,
    r"/=": Types.DIV_ASSIGN,
    r"%=": Types.MOD_ASSIGN,
    r"&=": Types.AND_ASSIGN,
    r"\|=": Types.OR_ASSIGN,
    r"\^=": Types.XOR_ASSIGN,
    r"<<=": Types.LEFT_SHIFT_ASSIGN,
    r">>=": Types.RIGHT_SHIFT_ASSIGN,
    # Punctuation
    r"\(": Types.LPAREN,
    r"\)": Types.RPAREN,
    r"\{": Types.LBRACE,
    r"\}": Types.RBRACE,
    r"\[": Types.LBRACKET,
    r"\]": Types.RBRACKET,
    r"\,": Types.COMMA,
    r";": Types.SEMICOLON,
    r"\:": Types.COLON,
    r"\.": Types.DOT,
    r"\.\.\.": Types.ELLIPSIS,
    r"->": Types.ARROW,
    # Literals
    r"\"[^\"]*\"": Types.STRING,  # Double quoted strings
    r"'[^']*'": Types.STRING,  # Single quoted strings
    r"\b0[xX][0-9a-fA-F]+\b ": Types.NUMBER,  # Hex: 0xFF, 0x1a
    r"\b0[oO][0-7]+\b": Types.NUMBER,  # Octal: 0o777
    r"\b0[bB][01]+\b": Types.NUMBER,  # Binary: 0b1010
    r"\b\d+[eE][+-]?\d+\b": Types.NUMBER,  # Scientific: 1e10, 2.5e-3
    r"(?:\.\d+|\d+(?:\.\d*)?)": Types.NUMBER,  # Numbers: 123, 45.67, .89, 3.14
    r"\b[a-zA-Z_]\w*\b": Types.IDENTIFIER,  # Identifiers: variable_name, function_name
    # Special characters
    r"\#.*": Types.COMMENT,  # Single line comment
    r"\u2420": Types.SPACE,  # Encoded space character
    r"\u2424": Types.NEWLINE,  # Encoded newline character
}

# Jade-specific token identification
# Used to quickly check if a token type is Jade-specific syntax
jade_switch = {Types.PROMPT: True, Types.PROMPTDREF: True}


class Token:
    """
    Represents a single lexical token in Jade source code.

    A Token combines the raw text (lexeme), its classified type,
    and its position in the source. Tokens are created by matching
    lexemes against the regex patterns in the 'map' dictionary.

    Attributes:
        Pos (int): Position of this token in its line (0-indexed)
        Value (str): The raw text of the token (lexeme)
        Type (Types | None): The classified token type, or None if no match
    """

    def __init__(self, lex: str, pos: int) -> None:
        """
        Create a Token by matching a lexeme against known patterns.

        The lexeme is tested against all regex patterns in the 'map'
        dictionary until a full match is found. The first matching
        pattern determines the token type.

        Args:
            lex: The raw text (lexeme) to tokenize
            pos: Position of this token in its line (0-indexed)
        """
        # Set the position
        self.Pos = pos

        # Set the value (lexeme)
        self.Value = lex

        # Classify the token type by pattern matching
        self.Type = None
        for pattern, type in map.items():
            if re.fullmatch(pattern, lex):
                self.Type = type
                break

    def __str__(self) -> str:
        """Return a string representation showing the token's value, type, and position."""
        return f"Token(Value = {self.Value}, Type = {self.Type}, Position = {self.Pos})"
