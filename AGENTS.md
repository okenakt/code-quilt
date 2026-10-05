# CodeQuilt development principles

This project is under active development. Keep the code and docs at the ideal, minimal design for what exists today (KISS).

- Remove dead code immediately. Backward compatibility is not a goal: change formats and protocols freely.
- Merge similar tests and drop overly fine-grained ones. Test behavior that matters, not every branch.
- Keep code comments minimal. When an explanation would get long, put it in `docs/` and point there.
- Generalize similar logic aggressively and prefer one simple mechanism over special cases (for example, the Queen is an ordinary named session, not a separate kind of thing).

## Documentation

The docs in `docs/` are written in Japanese and must always match the code.

- `design.md`: architecture and the reasoning behind it, for the current state only.
- `features.md`: what the user can do today: features, every command (CLI, board, MCP, plugins), settings files and their keys, and where data is stored.
- `decisions.md`: the only place for history. Append each decision with its reason and the alternatives that were rejected; never rewrite past entries except to fix names.
- Update the affected docs in the same turn as any change to behavior, commands, settings, structure or names, and commit them with that change. Before finishing a turn, check that the docs still describe the code.
- Use the names the code uses: `cq daemon` or "the daemon" (never the nickname "cqd"), `cq host`, `cq-board`, `cq-queen`.

## Development notes

- When running the board with `npx tauri dev`, start the daemon outside it first (`cq daemon restart`). On Windows, cargo runs the board in a job object that kills everything started from it when the board stops or rebuilds, including the daemon and every session it launched.
