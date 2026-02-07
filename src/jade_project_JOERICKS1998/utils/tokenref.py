import re
from enum import Enum, auto


class Types(Enum):
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

    # Null type (for no matches)
    NULL = auto()
    FALLBACK = auto()

    # Comment
    COMMENT = auto()

    # Empty token
    EMPTY = auto()


# Combined token mapping (keywords, operators, punctuation)
# When adding to this list, ORDER MATTERS, see regex verbose documentation for more info...
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
    r"\bprompt\b": Types.PROMPT,
    r"\?": Types.PROMPTDREF,
    # Operators
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
    r"\"[^\"]*\"": Types.STRING,  # Double quoted strings
    r"'[^']*'": Types.STRING,  # Single quoted strings
    r"\b0[xX][0-9a-fA-F]+\b ": Types.NUMBER,  # Hex: 0xFF, 0x1a
    r"\b0[oO][0-7]+\b": Types.NUMBER,  # Octal: 0o777
    r"\b0[bB][01]+\b": Types.NUMBER,  # Binary: 0b1010
    r"\b\d+[eE][+-]?\d+\b": Types.NUMBER,  # Scientific: 1e10, 2.5e-3
    r"(?:\.\d+|\d+(?:\.\d*)?)": Types.NUMBER,  # Numbers: 123, 45.67, .89, 3.14
    r"\b[a-zA-Z_]\w*\b": Types.IDENTIFIER,  # Identifiers: variable_name, function_name
    r"\#.*": Types.COMMENT,  # single line comment
    r"\u2420": Types.SPACE,
    r"\u2424": Types.NEWLINE,
}

jade_switch = {Types.PROMPT: True, Types.PROMPTDREF: True}


class Token:
    def __init__(self, lex: str, pos: int) -> None:
        # first set the position
        self.Pos = pos

        # then set the value
        self.Value = lex

        # Set the type to none (default)
        self.Type = None
        # set the type
        for pattern, type in map.items():
            if re.fullmatch(pattern, lex):
                self.Type = type
                break

    def __str__(self):
        return f"Token(Value = {self.Value}, Type = {self.Type}, Position = {self.Pos})"
