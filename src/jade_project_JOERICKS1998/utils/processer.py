from ..constants import constants
from . import heap, parser, tokenref


# A buffer class that stores the intermediate python string
class Buffer:
    def __init__(self):
        self.out_py = ""

    def write(self, string: str):
        self.out_py += string

    def flush(self):
        exec(self.out_py, {"__builtins__": __builtins__})
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

def process_2(line_of_tokens: parser.Line, heap: heap) -> str:
    output_str = ""
    i = 0
    while i < len(line_of_tokens):
        if line_of_tokens[i].Type == tokenref.Types.PROMPTDREF:
            if i + 1 < len(line_of_tokens):
                response = heap.release(line_of_tokens[i+1].Value)
                output_str += f'\"\"\"{response.Clean}\"\"\"'
                i += 2
            else:
                raise IndexError(f"PROMPTDREF at position {i} has no following identifier")
        else:
            output_str += line_of_tokens[i].Value
            i+=1
    return output_str


# Interpreter for jade lines
def line_interpreter(line_of_tokens: parser.Line, heap: heap.Heap) -> str:
    try:
        if tokenref.Types.PROMPT in [token.Type for token in line_of_tokens]:
            return process_1(line_of_tokens, heap)
        elif tokenref.Types.PROMPTDREF in [token.Type for token in line_of_tokens]:
            return process_2(line_of_tokens, heap)
    except Exception as e:
        print(f"Error interpreting line {line_of_tokens.Pos}: {e}")
        raise
    return ""


# The main function that processes the code
def machine(jade_code_string: str, python_buffer: Buffer, heap: heap.Heap) -> None:
    # Step 1: Tokenize all of the code in the file
    try:
        # preprocess code string
        preprocessed_space_code_str = jade_code_string
        for k, v in constants.SPACE_ENCODINGS.items():
            preprocessed_space_code_str = preprocessed_space_code_str.replace(k, v)
        token_block = parser.Block(preprocessed_space_code_str)
    except Exception as e:
        print(f"Error tokenizing Jade code: {e}")
        return
    # Step 2: Go line by line and based on tokens and types, translate and add these tokens to the buffer
    try:
        for line in token_block:
            if line.is_jade():
                jade_output = line_interpreter(line, heap)
                # Postprocess jade output to decode space encodings
                for k, v in constants.SPACE_ENCODINGS.items():
                    jade_output = jade_output.replace(v, k)
                python_buffer.write(jade_output)
            else:
                py_line = "".join(line.TokenValues)
                postprocessed_py_line = py_line
                for k, v in constants.SPACE_ENCODINGS.items():
                    postprocessed_py_line = postprocessed_py_line.replace(v, k)
                python_buffer.write(postprocessed_py_line)
    except Exception as e:
        print(f"Error processing Jade code: {e}")
        return
    # If a prompt arises, handle via the heap first then write the output to the buffer
    return
