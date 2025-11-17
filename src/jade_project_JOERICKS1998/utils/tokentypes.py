from enum import Enum, auto


class TokenType(Enum):
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
    EOF = auto()

    # Jade Tokens
    PROMPT = auto()
    PROMPTDREF = auto()

    # Fallback type (for no matches)
    FALLBACK = auto()


# Keyword mapping
KEYWORDS = {
    "if": TokenType.IF,
    "else": TokenType.ELSE,
    "elif": TokenType.ELIF,
    "for": TokenType.FOR,
    "while": TokenType.WHILE,
    "break": TokenType.BREAK,
    "continue": TokenType.CONTINUE,
    "pass": TokenType.PASS,
    "def": TokenType.DEF,
    "class": TokenType.CLASS,
    "return": TokenType.RETURN,
    "yield": TokenType.YIELD,
    "lambda": TokenType.LAMBDA,
    "import": TokenType.IMPORT,
    "from": TokenType.FROM,
    "as": TokenType.AS,
    "try": TokenType.TRY,
    "except": TokenType.EXCEPT,
    "finally": TokenType.FINALLY,
    "raise": TokenType.RAISE,
    "assert": TokenType.ASSERT,
    "with": TokenType.WITH,
    "global": TokenType.GLOBAL,
    "nonlocal": TokenType.NONLOCAL,
    "and": TokenType.AND,
    "or": TokenType.OR,
    "not": TokenType.NOT,
    "in": TokenType.IN,
    "is": TokenType.IS,
    "True": TokenType.TRUE,
    "False": TokenType.FALSE,
    "None": TokenType.NONE,
    "async": TokenType.ASYNC,
    "await": TokenType.AWAIT,
    "match": TokenType.MATCH,
    "case": TokenType.CASE,
    "prompt": TokenType.PROMPT,
    "?": TokenType.PROMPTDREF,
}


def set(w: str):
    token_type = KEYWORDS.get(w)
    if not token_type:
        return TokenType.FALLBACK
    return token_type
