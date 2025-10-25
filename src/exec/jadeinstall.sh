#!/bin/bash
# Usage: jadeinstall.sh
# Enhanced jade installation script with flexible binary directory detection

# Function to find suitable bin directory
find_bin_directory() {
    local bin_dirs=(
        "/usr/local/bin"
        "/usr/bin"
        "$HOME/.local/bin"
        "$HOME/bin"
        "/opt/local/bin"
        "/opt/bin"
    )

    # Check PATH for writable bin directories
    IFS=':' read -ra path_dirs <<< "$PATH"
    for dir in "${path_dirs[@]}"; do
        if [[ -d "$dir" && -w "$dir" ]]; then
            bin_dirs+=("$dir")
        fi
    done

    # Remove duplicates and check each directory
    local unique_dirs=($(printf "%s\n" "${bin_dirs[@]}" | sort -u))

    for dir in "${unique_dirs[@]}"; do
        if [[ -d "$dir" && -w "$dir" ]]; then
            echo "$dir"
            return 0
        elif [[ -d "$dir" ]]; then
            echo "$dir"
            return 1  # Directory exists but not writable
        fi
    done

    return 2  # No suitable directory found
}

# Function to install with sudo if needed
install_with_privileges() {
    local target_dir="$1"
    local use_sudo=false

    if [[ ! -w "$target_dir" ]]; then
        if command -v sudo &> /dev/null; then
            use_sudo=true
        else
            echo "Error: Directory $target_dir is not writable and sudo is not available."
            return 1
        fi
    fi

    local download_cmd="curl -sSL https://raw.githubusercontent.com/joericks1998/jade/refs/heads/main/src/exec/jade.sh -o $target_dir/jade"
    local chmod_cmd="chmod +x $target_dir/jade"

    if [[ "$use_sudo" == true ]]; then
        sudo $download_cmd && sudo $chmod_cmd
    else
        $download_cmd && $chmod_cmd
    fi
}

# Main installation logic
main() {
    # Check if pip3 is installed
    if ! command -v pip3 &> /dev/null; then
        echo "Error: pip3 is not installed. Please install pip3 before running this script."
        exit 1
    fi

    # Install the package
    echo "Installing jade package from TestPyPI..."
    if ! pip3 install --index-url https://test.pypi.org/simple/ --no-deps jade-project-JOERICKS1998; then
        echo "Error: Failed to install jade package."
        exit 1
    fi

    # Find suitable bin directory
    echo "Searching for installation directory..."
    bin_dir=$(find_bin_directory)
    find_exit_code=$?

    case $find_exit_code in
        0)
            echo "Found writable directory: $bin_dir"
            ;;
        1)
            echo "Found directory (requires sudo): $bin_dir"
            ;;
        2)
            echo "Error: No suitable bin directory found in common locations or PATH."
            echo "Common locations checked: /usr/local/bin, /usr/bin, ~/.local/bin, ~/bin, /opt/local/bin, /opt/bin"
            echo "Please create a bin directory in your PATH or specify one manually."
            exit 1
            ;;
    esac

    # Download and install the jade executable
    echo "Installing jade executable to $bin_dir..."
    if ! install_with_privileges "$bin_dir"; then
        echo "Error: Failed to install jade executable."
        exit 1
    fi

    # Verify installation
    if [[ ! -f "$bin_dir/jade" ]]; then
        echo "Error: jade executable was not created."
        exit 1
    fi

    if grep -q '404: Not Found' "$bin_dir/jade"; then
        echo "Error: Download failed, got 404 Not Found."
        exit 1
    fi

    echo "jade installed successfully to $bin_dir/jade"
    echo "You can now run 'jade' from anywhere in your terminal."

    # Check if the directory is in PATH
    if [[ ":$PATH:" != *":$bin_dir:"* ]]; then
        echo "Warning: $bin_dir is not in your PATH. You may need to add it to your shell profile."
        echo "Add this line to your ~/.bashrc or ~/.zshrc:"
        echo "export PATH=\"\$PATH:$bin_dir\""
    fi
}

# Run main function
main "$@"
