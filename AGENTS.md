# CodeQuilt development principles

This project is under active development. Keep the code and docs at the ideal, minimal design for what exists today (KISS).

- Only `docs/decisions.md` records history and alternatives. `docs/design.md` and the code describe the current state only.
- Remove dead code immediately. Backward compatibility is not a goal: change formats and protocols freely.
- Merge similar tests and drop overly fine-grained ones. Test behavior that matters, not every branch.
- Keep code comments minimal. When an explanation would get long, put it in `docs/` and point there.
- Generalize similar logic aggressively and prefer one simple mechanism over special cases (for example, the Queen is an ordinary session with a tag, not a separate kind of thing).
