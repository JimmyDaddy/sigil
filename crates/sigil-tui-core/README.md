# sigil-tui-core

`sigil-tui-core` contains the application-neutral, bounded contracts used by a terminal UI:
stable node identity, surfaces, normalized input, damage, presentation receipts, themes, and
virtualized sequence metadata.

The crate does not create a runtime, perform I/O, or depend on Ratatui, Crossterm, Tokio, or a
product domain. Hosts may use these primitives to build a retained or immediate UI and decide
how asynchronous work is scheduled.
