import os
import sys
import logging

class Compiler:
    def __init__(self):
        self.logger = logging.getLogger(__name__)
        self.logger.setLevel(logging.DEBUG)
        self.logger.addHandler(logging.StreamHandler())

    def __call__(self, source_file, output_file):
        self.logger.info(f"Compiling {source_file} to {output_file}")
        # Add your compilation logic here
        self.logger.debug("Compilation started")
        with open(source_file, "r") as f:
            source_code = f.read()
            for line in source_code.splitlines():
                print(line)
        self.logger.debug("Compilation completed")
        self.logger.info("Writting to object file")
        return True
