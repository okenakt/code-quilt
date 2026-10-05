# CodeQuilt development principles

This project is under active development. Keep the code and docs at the ideal, minimal design for what exists today (KISS).

- Only `docs/decisions.md` records history and alternatives. `docs/design.md` and the code describe the current state only.
- Remove dead code immediately. Backward compatibility is not a goal: change formats and protocols freely.
- Merge similar tests and drop overly fine-grained ones. Test behavior that matters, not every branch.
- Keep code comments minimal. When an explanation would get long, put it in `docs/` and point there.
- Generalize similar logic aggressively and prefer one simple mechanism over special cases (for example, the Queen is an ordinary session with a tag, not a separate kind of thing).

## Development notes

- When running the board with `npx tauri dev`, start the daemon outside it first (`cq daemon restart`). On Windows, cargo runs the board in a job object that kills everything started from it when the board stops or rebuilds, including the daemon and every session it launched.
