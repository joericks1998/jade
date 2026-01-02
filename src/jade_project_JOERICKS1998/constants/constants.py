from pickle import NEWFALSE
from termios import TAB0

SUPPORTED_MODELS = {
    "deepseek-chat": "https://api.deepseek.com",
    "deepseek-reasoner": "https://api.deepseek.com",
}

ENCODINGS = {
    " ": "␠",
    "\t": "␉",
    "\n": "␤",
}
