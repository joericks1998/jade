# Jade Programming Language

![Jade Logo](extras/jade-logo.png){ width=200 }

A modern, expressive programming language designed for clarity and productivity. Built with Python at its core, Jade combines the power of traditional programming with the elegance of modern language design.

## ✨ Features

- **🚀 Lightning-fast compilation and execution**
- **🐍 Seamless Python interoperability** - Leverage Python's extensive ecosystem
- **📚 Clean, readable syntax** - Focus on developer experience
- **🔧 Extensible architecture** - Easy to customize and extend
- **🤖 Built-in LLM integration** - Enhanced development with AI assistance
- **🎯 File-based execution** - Simple `.jde` file format

## 🚀 Quick Start

### Installation

Jade is available on TestPyPI. Install it using pip:

```bash
pip install --index-url https://test.pypi.org/simple/ --no-deps jade-project-JOERICKS1998
```

### Your First Jade Program

1. **Create a Jade file**:
   ```bash
   echo "print('Hello, Jade!')" > hello.jde
   ```

2. **Run your program**:
   ```bash
   jade hello.jde
   ```

3. **See the output**:
   ```
   Hello, Jade!
   ```

## 📖 Usage

### Basic Compilation
```bash
jade your_file.jde
```

### Setup Commands

Configure LLM integration for enhanced features:

```bash
# Configure LLM provider
jade setup llm

# Show setup help
jade setup --help

# Display welcome message
jade setup
```

## 🔧 Setup & Configuration

### LLM Integration

Jade supports LLM integration for enhanced development features. To set up:

1. Run `jade setup llm`
2. Enter your preferred LLM provider (OpenAI, Anthropic, etc.)
3. Provide your username and API key

Your credentials are stored securely using your system's keychain.

### Retrieving API Keys

In your Jade code, you can retrieve stored API keys:

```python
from jade_project_JOERICKS1998.utils.setup import get_llm_api_key

api_key = get_llm_api_key("your_username", "your_provider")
```

## 🏗️ Project Structure

```
jade_project_JOERICKS1998/
├── main.py              # CLI entry point
├── utils/
│   ├── compiler.py      # Core compilation engine
│   └── setup.py         # Configuration utilities
└── llm/                 # LLM integration modules
```

## 🎯 File Format

Jade uses `.jde` files, which are executed as Python code:

```python
# hello.jde
print("Welcome to Jade!")
for i in range(3):
    print(f"Counting: {i}")
```

## 🔍 Development

### Local Development Installation

1. **Clone the repository**:
   ```bash
   git clone https://github.com/joericks1998/jade.git
   cd jade
   ```

2. **Install in development mode**:
   ```bash
   pip install -e .
   ```

3. **Run tests**:
   ```bash
   hatch test
   ```

### Building from Source

```bash
hatch build
```

## 🤝 Contributing

We welcome contributions! Please see our [GitHub repository](https://github.com/joericks1998/jade) for:

- 🐛 Bug reports
- 💡 Feature requests
- 🔧 Pull requests
- 📚 Documentation improvements

## 📄 License

This project is licensed under the MIT License - see the [LICENSE](LICENSE) file for details.

## 📞 Support

- **GitHub Issues**: [Report bugs & feature requests](https://github.com/joericks1998/jade/issues)
- **Documentation**: Check the [wiki](https://github.com/joericks1998/jade/wiki)
- **Community**: Join our growing developer community

---

**Happy coding with Jade!** 🎊

*Built by Joe Ricks*
