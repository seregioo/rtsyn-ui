# rtsyn-ui

Rust client frontends for the RTSyn HTTP API.

This module intentionally keeps workspace handling client-side. Workspaces are
files consumed by the CLI/GUI and translated into HTTP API commands; runtime and
API processes do not need workspace concepts.

## Binaries

- `rtsyn-cli`: command-line API client.
- `rtsyn-gui`: lightweight GUI entry point and headless state renderer.

## Plugin UI Metadata

Optional node GUI metadata can live next to a module as `rtsyn-node-ui.toml`.
The CLI does not need this file. The GUI reads it to build controls without
coupling UI behavior to module logic.

```toml
name = "Adder"
description = "Adds two values"

[[controls]]
name = "gain"
label = "Gain"
kind = "number"
target = "param"
param_id = 0
value_type = "f64"
default = "1.0"
```
