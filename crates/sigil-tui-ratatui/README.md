# sigil-tui-ratatui

`sigil-tui-ratatui` is the Ratatui presentation adapter for the application-neutral
`sigil-tui-core` contracts. It provides a bounded scratch render context and terminal-epoch
presentation state while keeping Ratatui types out of the core crate.

Application state, persistence, process ownership, and asynchronous scheduling remain the
responsibility of the embedding host.
