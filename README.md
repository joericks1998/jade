# Jade Programming Language

![Jade Logo](extras/jade-logo.png){ width=200 }

A modern programming language that brings first-class LLM integration into your code. Jade lets you declare prompts as variables, dereference them with `?` to invoke an LLM, and mix the results seamlessly with standard Python-compatible logic — all in `.jde` files.

## Features

- **First-class LLM prompts** — declare prompts as variables, invoke them with `?`
- **Static and dynamic prompts** — resolve at compile time or runtime
- **Streaming output** — `print(?p)` streams tokens progressively
- **Conversation memory** — stateful chat history across your entire program
- **Built-in session builtins** — inspect tokens, messages, model, and clear history
- **Python-compatible syntax** — full support for Python control flow, functions, imports, and more
- **DeepSeek integration** — powered by `deepseek-chat` and `deepseek-reasoner`

## Quick Start

### Installation

Jade is available on TestPyPI. Install it using pip:

```bash
pip install --index-url https://test.pypi.org/simple/ --no-deps jade_project_JOERICKS1998
```

### Setup

Before running Jade files, configure your DeepSeek API key:

```bash
jade setup
```

This will prompt you for your API key from https://platform.deepseek.com/ and store it securely in your system keychain.

### Your First Jade Program

Create a file `hello.jde`:

```python
prompt greeting = "Say hello in exactly five words"
print(?greeting)
```

Run it:

```bash
jade hello.jde
```

## Language Reference

### Prompts

Jade introduces the `prompt` keyword for declaring LLM prompts as named variables.

**Static prompt** — the string literal is sent to the LLM at translation time:

```python
prompt greeting = "Say hello in exactly five words"
result = ?greeting
print(result)
```

**Dynamic prompt** — the expression is evaluated at runtime, so the LLM is called each time:

```python
prompt p = input("Enter your question: ")
response = ?p
print(response)
```

### Dereference operator (`?`)

Use `?name` anywhere a value is expected to invoke the LLM and get its response:

```python
prompt summary = "Summarize the water cycle in two sentences"
print(?summary)                      # print the response
text = ?summary                      # assign to a variable
items = [?summary, "other item"]     # use in a list
```

### Streaming output

Wrapping a prompt dereference in `print()` streams the response token by token instead of buffering it:

```python
prompt p = input("Ask me anything: ")
print(?p)    # streams progressively to stdout
```

### f-string prompts

You can use Python f-strings to build prompts dynamically:

```python
topic = "black holes"
prompt explanation = f"Explain {topic} to a 10-year-old in three sentences"
print(?explanation)
```

You can also chain LLM calls by injecting a prior response into a new prompt:

```python
prompt question = input("What should I reason about? ")
reasoning = ?question

prompt answer_prompt = f"Given this reasoning:\n{reasoning}\n\nGive a concise final answer."
print(?answer_prompt)
```

### Conversation memory

Jade maintains a conversation history across the entire program execution. Each prompt dereference adds to the same conversation, enabling chatbot-style programs:

```python
while True:
    prompt p = input("You: ")
    if p.upper() == "Q":
        break
    print(?p)
```

### Built-in session variables

Jade provides special built-in names for inspecting the current LLM session:

| Built-in | Type | Description |
|---|---|---|
| `__tokens__` | `int` | Total tokens used this session |
| `__prompt_tokens__` | `int` | Tokens sent in prompts |
| `__completion_tokens__` | `int` | Tokens received in completions |
| `__messages__` | `list` | Full conversation message history |
| `__model__` | `str` | Active model name |
| `__clear__()` | function | Clears conversation history and resets token counters |

Example:

```python
prompt q = "What is 2 + 2?"
print(?q)

print(f"Model: {__model__}")
print(f"Tokens used: {__tokens__}")
print(f"Messages in history: {len(__messages__)}")

__clear__()   # reset for a fresh conversation
```

## CLI Reference

### Running a Jade file

```bash
jade <filename.jde> [flags]
```

| Flag | Short | Description |
|---|---|---|
| `--verbose` | `-v` | Show debug output (compilation steps, token info) |
| `--show-python` | `-s` | Print the generated Python source after execution |

Examples:

```bash
jade hello.jde
jade hello.jde -v
jade hello.jde --show-python
jade hello.jde -v -s
```

### Setup commands

```bash
jade setup             # Interactive DeepSeek API key configuration
jade setup --help      # Show setup instructions and prerequisites
```

### Information commands

```bash
jade info              # Learn about Jade language features
jade help              # Show all available commands and usage
```

## Project Structure

```
src/jade_project_JOERICKS1998/
├── main.py                  # CLI entry point
├── config.py                # Runtime flags (verbose, show_python)
├── constants/
│   └── constants.py         # Supported models, space encodings
├── utils/
│   ├── command_line.py      # CLI argument routing
│   ├── compiler.py          # Compilation entry point
│   ├── heap.py              # Prompt storage and LLM invocation
│   ├── parser.py            # Tokenizer (Chunk, Block, LLMOutput)
│   ├── processer.py         # Compilation pipeline
│   └── tokenref.py          # Token type definitions
└── llm/
    └── deepseek.py          # DeepSeek API client and conversation management
```

## Development

### Local Installation

```bash
git clone https://github.com/joericks1998/jade.git
cd jade
pip install -e .
```

### Building from Source

```bash
hatch build
```

## Contributing

Contributions are welcome. Please visit the [GitHub repository](https://github.com/joericks1998/jade) to:

- Report bugs
- Request features
- Submit pull requests
- Improve documentation

## License

MIT License — see the [LICENSE](LICENSE) file for details.

## Support

- **GitHub Issues**: [Report bugs & feature requests](https://github.com/joericks1998/jade/issues)

---

*Built by Joe Ricks*
