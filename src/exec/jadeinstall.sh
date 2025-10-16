#!/bin/bash
# Usage: jadeinstall.sh
# Check if pip is installed
if ! command -v pip &> /dev/null; then
    echo "Error: pip is not installed. Please install pip before running this script."
    exit 1
else
    pip install --index-url https://test.pypi.org/simple/ --no-deps jade-project-JOERICKS1998
fi

# Check if /usr/local/bin directory exists
if [ -d "/usr/local/bin" ]; then
    sudo curl -sSL https://raw.githubusercontent.com/joericks1998/jade/refs/heads/main/src/exec/jade.sh -o /usr/local/bin/jade
    sudo chmod +x /usr/local/bin/jade
    echo "jade installed successfully"
else
    echo "Error: /usr/local/bin directory does not exist. Cannot set execute permissions on jade."
    exit 1
fi

if grep -q '404: Not Found' /usr/local/bin/jade; then
  echo "Error: Download failed, got 404 Not Found."
  exit 1
fi
