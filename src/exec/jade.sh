#!/bin/bash
# Usage: jade.sh <file-to-compile>

# Change to the directory where compile.py lives, or use full path
cd "$(dirname "$0")"

# Run Python script, passing all arguments through
python3 -m jade_project_JOERICKS1998.main "$@"
