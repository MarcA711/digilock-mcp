# Project overview

This project is a Rust MCP (Model Context Protocol) wrapper for the TOPTICA DigiLock 110 Remote Control Interface (RCI). It bridges MCP clients over stdio to the TCP RCI endpoints of the Windows DigiLock Module Server (DMS) and DigiLock User Interface (DUI).

# Python tools

Use the Python 3 environment in `/opt/venv/`, preferably by invoking `/opt/venv/bin/python3` directly rather than system Python. It provides tools for inspecting and extracting PDF documentation, NumPy/SciPy for numerical calculations and mcp including the cli to connect to an MCP server.

# Documentation and safety

`docs/` contains documentation for the RCI interface, the DigiLock 110, and its software. Consult these documents when checking protocol details and parameter behavior.

RCI commands can change real hardware outputs. Do not assume software-accepted parameter ranges are safe for an attached laser; preserve write guards and avoid hardware operations without explicit authorization.
