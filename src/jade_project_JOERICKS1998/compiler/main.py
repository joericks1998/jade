import sys

from . import compiler


def main() -> None:
    if len(sys.argv) != 2:
        print("Usage: python readfile.py <filename>")
        sys.exit(1)

    filename = sys.argv[1]

    try:
        tst = compiler.compile(filename)
        print(tst)
    except FileNotFoundError:
        print(f"File not found: {filename}")
    except Exception as e:
        print(f"Error reading file: {e}")


if __name__ == "__main__":
    main()
