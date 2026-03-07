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
    TRIPLEQ = auto()
    F = auto()

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
    BUILTIN = auto()

    # Null type (for no matches)
    NULL = auto()
    FALLBACK = auto()

    # Comment
    COMMENT = auto()

    # Empty token
    EMPTY = auto()


BUILTIN_MAP: dict[str, str] = {
    "__tokens__":            "__jade_heap.tokens",
    "__prompt_tokens__":     "__jade_heap.prompt_tokens",
    "__completion_tokens__": "__jade_heap.completion_tokens",
    "__messages__":          "__jade_heap.messages",
    "__model__":             "__jade_heap.model",
    "__clear__":             "__jade_heap.clear",
}

KEYWORD_TYPES: dict[str, "Types"] = {
    "prompt": Types.PROMPT,
    "if": Types.IF, "else": Types.ELSE, "elif": Types.ELIF,
    "for": Types.FOR, "while": Types.WHILE, "break": Types.BREAK,
    "continue": Types.CONTINUE, "pass": Types.PASS,
    "def": Types.DEF, "class": Types.CLASS,
    "return": Types.RETURN, "yield": Types.YIELD,
    "import": Types.IMPORT, "from": Types.FROM, "as": Types.AS,
    "try": Types.TRY, "except": Types.EXCEPT, "finally": Types.FINALLY,
    "raise": Types.RAISE, "assert": Types.ASSERT, "with": Types.WITH,
    "global": Types.GLOBAL, "nonlocal": Types.NONLOCAL,
    "and": Types.AND, "or": Types.OR, "not": Types.NOT,
    "in": Types.IN, "is": Types.IS,
    "True": Types.TRUE, "False": Types.FALSE, "None": Types.NONE,
    "async": Types.ASYNC, "await": Types.AWAIT,
    "match": Types.MATCH, "case": Types.CASE,
    "lambda": Types.LAMBDA,
}


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

    def __init__(self, lex: str, pos: int, type: "Types" = None) -> None:
        """
        Create a Token with an explicit type classification.

        Args:
            lex: The raw text (lexeme) to tokenize
            pos: Position of this token in its line (0-indexed)
            type: The token type; defaults to Types.FALLBACK if not provided
        """
        self.Pos = pos
        self.Value = lex
        self.Type = type if type is not None else Types.FALLBACK

    def __str__(self) -> str:
        """Return a string representation showing the token's value, type, and position."""
        return f"Token(Value = {self.Value}, Type = {self.Type}, Position = {self.Pos})"
