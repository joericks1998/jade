import sys
import os

sys.path.insert(0, os.path.join(os.path.dirname(__file__), '..', 'src'))

from jade_project_JOERICKS1998.constants import constants
from jade_project_JOERICKS1998.utils import parser

code = '''
prompt greeting = "Say hello"
print(f"Result: {greeting}")
x = 5 + 3
'''

# Preprocess: encode spaces, tabs, newlines
for k, v in constants.SPACE_ENCODINGS.items():
    code = code.replace(k, v)

block = parser.Block(code)
