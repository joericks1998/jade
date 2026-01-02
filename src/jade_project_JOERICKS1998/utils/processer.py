from ..constants import constants
from . import heap, parser


class Buffer:
    def __init__(self):
        self.out_py = ""

    def write(self, string: str):
        self.out_py += string

    def flush(self):
        exec(self.out_py)
        self.out_py = ""


def machine(jade_code_string: str, python_buffer: Buffer, heap: heap.Heap) -> None:
    # Step 1: Tokenize all of the code in the file
    try:
        # preprocess code string
        preprocessed_space_code_str = jade_code_string
        for k, v in constants.ENCODINGS.items():
            preprocessed_space_code_str = preprocessed_space_code_str.replace(k, v)
        token_block = parser.Block(preprocessed_space_code_str)
        print(token_block)
    except Exception as e:
        print(f"Error tokenizing Jade code: {e}")
        return
    # Step 2: Go line by line and based on tokens and types, translate and add these tokens to the buffer
    try:
        for line in token_block:
            py_line = "".join(line.Tokens)
            print(py_line)
    except Exception as e:
        print(f"Error processing Jade code: {e}")
        return
    # If a prompt arises, handle via the heap first then write the output to the buffer
    return
