import os
import sys
import logging


def compile(source_file):
    with open(source_file, "r") as f:
        source_code = f.read()
        for line in source_code.splitlines():
            print(line)
    return True
