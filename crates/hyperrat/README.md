# hyperrat

This crate provides a `Link` widget for the ratatui library that renders clickable hyperlinks in supported terminals using the OSC 8 escape sequence. It handles label truncation, hover styles, and graceful degradation when hyperlinks aren't supported or enabled.

The `Link` widget can be customized with different styles, hover effects, and fallback suffixes to provide additional context when the label is truncated. It also ensures that any cells used for hyperlink rendering are properly marked to prevent interference with text selection and cursor movement in the terminal.
