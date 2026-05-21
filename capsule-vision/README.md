# capsule-vision

This package implements experiemental computer vision features for Capsule (not all will be productionized immediately). There is both training and export scripts, as well as notebooks for exploring new model capabilities relevant to the project.

## Setup

Prerequisite:
- This project uses [uv](https://docs.astral.sh/uv/) for Python dependency management.
- Since many ML packages are platform-specific, we assume development is done on Linux.

```bash
# Sync dependencies (CUDA/CPU)
uv sync
# or with ROCm extras
uv sync --extra rocm

# Activate venv
source .venv/bin/activate

# ROCm in WSL only
location="./.venv/lib/python3.12/site-packages" # Verify this path
cd ${location}/torch/lib/
rm libhsa-runtime64.so*

# If using ROCm, need to re-install ROCm-compatible PyTorch manually
# Follow: <https://rocm.docs.amd.com/projects/radeon/en/latest/docs/install/native_linux/install-pytorch.html>
```

Other common commands include:

- Format: `uv run ruff format`
- Lint: `uv run ruff check`
- Type check: `uv run ty check`
- Run tests: `uv run pytest`
- Run notebook: `uv run jupyter notebook`
