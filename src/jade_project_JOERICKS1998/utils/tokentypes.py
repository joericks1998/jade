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
    SPACE = auto()

    # Jade Tokens
    PROMPT = auto()
    PROMPTDREF = auto()

    # Fallback type (for no matches)
    NULL = auto()
    FALLBACK = auto()


# Combined token mapping (keywords, operators, punctuation)
TOKEN_MAP = {
    # Keywords
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
    # Operators
    "=": TokenType.ASSIGN,
    "+": TokenType.PLUS,
    "-": TokenType.MINUS,
    "*": TokenType.MULTIPLY,
    "/": TokenType.DIVIDE,
    "%": TokenType.MODULO,
    "**": TokenType.POWER,
    "//": TokenType.FLOOR_DIV,
    "==": TokenType.EQUALS,
    "!=": TokenType.NOT_EQUALS,
    "<": TokenType.LESS,
    ">": TokenType.GREATER,
    "<=": TokenType.LESS_EQUAL,
    ">=": TokenType.GREATER_EQUAL,
    "&": TokenType.BIT_AND,
    "|": TokenType.BIT_OR,
    "^": TokenType.BIT_XOR,
    "~": TokenType.BIT_NOT,
    "<<": TokenType.LEFT_SHIFT,
    ">>": TokenType.RIGHT_SHIFT,
    "+=": TokenType.PLUS_ASSIGN,
    "-=": TokenType.MINUS_ASSIGN,
    "*=": TokenType.MULT_ASSIGN,
    "/=": TokenType.DIV_ASSIGN,
    "%=": TokenType.MOD_ASSIGN,
    "&=": TokenType.AND_ASSIGN,
    "|=": TokenType.OR_ASSIGN,
    "^=": TokenType.XOR_ASSIGN,
    "<<=": TokenType.LEFT_SHIFT_ASSIGN,
    ">>=": TokenType.RIGHT_SHIFT_ASSIGN,
    # Punctuation
    "(": TokenType.LPAREN,
    ")": TokenType.RPAREN,
    "{": TokenType.LBRACE,
    "}": TokenType.RBRACE,
    "[": TokenType.LBRACKET,
    "]": TokenType.RBRACKET,
    ",": TokenType.COMMA,
    ";": TokenType.SEMICOLON,
    ":": TokenType.COLON,
    ".": TokenType.DOT,
    "...": TokenType.ELLIPSIS,
    "->": TokenType.ARROW,
}


def set(w: str):
    # Check combined token map first (keywords, operators, punctuation)
    # Need to handle multi-character tokens by checking longest matches first
    for token_str in sorted(TOKEN_MAP.keys(), key=len, reverse=True):
        if w == token_str:
            return TOKEN_MAP[token_str]

    # Check for literals
    # Strings (quoted literals)
    if (w.startswith('"') and w.endswith('"')) or (
        w.startswith("'") and w.endswith("'")
    ):
        return TokenType.STRING

    # Numbers (numeric patterns)
    # Simple check - more robust number parsing would be better
    if w.replace(".", "", 1).isdigit() and w.count(".") <= 1:
        return TokenType.NUMBER

    # Identifiers (must start with letter or underscore)
    if w and (w[0].isalpha() or w[0] == "_"):
        return TokenType.IDENTIFIER

    if w == " ":
        return TokenType.SPACE

    if w == "\n":
        return TokenType.NEWLINE

    if w == "\t":
        return TokenType.INDENT

    return TokenType.FALLBACK
