# Oh My Shell

An AI-powered, natural-language Linux shell. Type a request in plain English,
review a step-by-step plan, discuss/adjust it, confirm, and watch it execute
with live streaming progress. Raw shell syntax runs straight through, with
the AI staying silent unless a command looks destructive.

Oh My Shell is a standalone, login-shell-capable executable — not a terminal
emulator and not a plugin for another shell.

Built for CSE324 (Operating Systems Lab, Complex Engineering Problem).

## Status

Early development. See `Oh-My-Shell-Blueprint-FINAL.md` (project source of
truth) for full architecture, model-selection benchmarks, and specs.

## Requirements

- Ubuntu 22.04+ / Debian-family Linux
- Python 3.11+
- A Google AI Studio API key (free at https://aistudio.google.com/apikey — the first-run wizard asks for it)

## Development setup

```bash
git clone https://github.com/imshaid/oh-my-shell.git
cd oh-my-shell
python3 -m venv .venv
source .venv/bin/activate
pip install -e ".[dev]"
```

Run tests:

```bash
pytest
```

## License

MIT
