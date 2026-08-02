# Capsule App

This is a fully-featured web client for Capsule. It is built using React, Rsbuild, Tailwind CSS, Tanstack, and more.

## Development

### Prerequisites

- Install Bun
- The replacement API is not available yet; UI-only development uses local fixtures

### Starting

1. Run

    ```bash
    # Install dependencies
    bun install
    # Run development server
    bun dev
    # Build production build
    bun run build
    # Preview production build locally
    bun run preview
    ```

2. Open <http://localhost:5173/> with your browser to see the result.

### API

The web client will consume the checked-in Kynos REST/OpenAPI contract through the planned Spargen-generated SDK. The replacement server and SDK are not available yet.
