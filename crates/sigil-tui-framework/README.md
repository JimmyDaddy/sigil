# sigil-tui

`sigil-tui` is the application-neutral public facade for the Sigil terminal UI
framework. It provides bounded surface construction, normalized input, damage
updates, stable node identity, and Ratatui presentation through a single
dependency.

The framework does not own an application worker, persistence, filesystem,
process, network, or domain state. Hosts implement [`App`] and retain ownership
of command execution and asynchronous work.

The `todo` and `chat` examples are intentionally independent consumers of this
public contract; they do not depend on Sigil application crates.
