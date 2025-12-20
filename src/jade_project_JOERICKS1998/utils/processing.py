from . import heap, tokenizer
from .tokentypes import TokenType


def process_jade_block(block: str, heap: heap.Heap) -> None:
    tk = tokenizer.Block(block)
    # when processing a block, there are 2 tokens we are really looking for
    # prompt, which can only occur in position 0 to be valid
    # and ?, or the deref token
    for line in tk.block:
        try:
            if line.tokens[0].type == TokenType.PROMPT:
                heap.add(line.tokens[1].value, line.tokens[3].value)
            else:
                print(line.tokens)
                for i in range(0, len(line.tokens) - 2):
                    if line.tokens[i].type == TokenType.PROMPTDREF:
                        heap.release(line.tokens[i + 1].value)
        except Exception as e:
            print(f"Line execution failed with exception {e}")
    # except Exception as e:
    #     print(f"Both line and jade line exectution failed with exception {e}")


def chunk_file(file) -> list[str]:
    try:
        chunks = []
        chunk = ""
        containers = {
            "try:": False,
            "except:": False,
            "if:": False,
            "elif:": False,
            "else:": False,
            "def": False,
            "class": False,
        }
        for line in file.readlines():
            # first check to see if the line is empty
            # if the end of a line ends with a semicolon, mark the section as contained
            # and add line to chunk
            # case 1, no containers identified
            if not any(containers.values()):
                for k in containers:
                    if k in line:
                        containers[k] = True
                if any(containers.values()):
                    chunk += line
                else:
                    chunks.append(line)
            else:
                if len(line) - len(line.lstrip()) > 0 or any(
                    [containers[k] for k in containers if ":" in k]
                ):
                    chunk += line
                else:
                    # first append the chunk and clear it
                    chunks.append(chunk)
                    chunk = ""
                    # reset the containers afterwards
                    for k in containers:
                        containers[k] = False
                    # then run case 1
                    for k in containers:
                        if k in line:
                            containers[k] = True
                    if any(containers.values()):
                        chunk += line
                    else:
                        chunks.append(line)
        chunks.append(chunk)
        return chunks
    except Exception as e:
        print(f"Chunking failed with exception {e}")
        return []
