from ..constants import constants
from . import heap, parser, tokenref


# A buffer class that stores the intermediate python string
class Buffer:
    def __init__(self):
        self.out_py = ""

    def write(self, string: str):
        self.out_py += string

    def flush(self):
        exec(self.out_py)
        self.out_py = ""


# Other jade processing functions (i.e processing tokens)


# Process #1 declaring a prompt and adding to the heap
def process_1(line_of_tokens: parser.Line, heap: heap.Heap) -> str:
    variable_name = ""
    prompt = ""
    for token in line_of_tokens:
        if token.Type == tokenref.Types.IDENTIFIER:
            variable_name = token.Value
        elif token.Type == tokenref.Types.STRING:
            prompt = token.Value
    heap.add(variable_name, prompt)
    return f"__p__{variable_name} = {prompt}"


# Interpreter for jade lines
def interpreter(line_of_tokens: parser.Line, heap: heap.Heap) -> str:
    
    pass


# The main function that processes the code
def machine(jade_code_string: str, python_buffer: Buffer, heap: heap.Heap) -> None:
    # Step 1: Tokenize all of the code in the file
    try:
        # preprocess code string
        preprocessed_space_code_str = jade_code_string
        for k, v in constants.ENCODINGS.items():
            preprocessed_space_code_str = preprocessed_space_code_str.replace(k, v)
        token_block = parser.Block(preprocessed_space_code_str)
    except Exception as e:
        print(f"Error tokenizing Jade code: {e}")
        return
    # Step 2: Go line by line and based on tokens and types, translate and add these tokens to the buffer
    try:
        for line in token_block:
            if line.is_jade():
                print(line)
            else:
                py_line = "".join(line.Tokens)
                postprocessed_py_line = py_line
                for k, v in constants.ENCODINGS.items():
                    postprocessed_py_line = postprocessed_py_line.replace(v, k)
                python_buffer.write(postprocessed_py_line)
    except Exception as e:
        print(f"Error processing Jade code: {e}")
        return
    # If a prompt arises, handle via the heap first then write the output to the buffer
    return
