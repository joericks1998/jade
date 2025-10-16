#!/bin/bash
# Usage: jadeinstall.sh
#
#
# Check if pip is installed
if ! command -v pip &> /dev/null; then
    echo "Error: pip is not installed. Please install pip before running this script."
    exit 1
else
    pip install --index-url https://test.pypi.org/simple/ --no-deps jade-project-JOERICKS1998
fi

# Check if /usr/local/bin directory exists
if [ -d "/usr/local/bin" ]; then
    curl -s https://raw.githubusercontent.com/JoeRicks1998/jade/main/jade.sh -o /usr/local/bin/jade
    chmod +x /usr/local/bin/jade
else
    echo "Error: /usr/local/bin directory does not exist. Cannot set execute permisßsions on jade."
    exit 1s
fi
