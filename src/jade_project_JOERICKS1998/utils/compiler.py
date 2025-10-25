def compile(source_file: str) -> bool:
    with open(source_file, "r") as f:
        source_code = f.read()
        for line in source_code.splitlines():
            print(line)
    return True
