def chunk_file(file) -> list:
    print(type(file))
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
