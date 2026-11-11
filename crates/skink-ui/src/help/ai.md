# AI

Skink talks to an ACP agent it starts in the background (Claude Code, Gemini CLI, opencode, Goose, Antigravity, Codex), or to EveryAI's agents inside EveryAI.

| Key | Does |
|---|---|
| Ctrl-G | the prompt box, for the selection (or the code around the cursor) |
| Alt-Shift-A | the agent panel: a chat with tool calls and the plan (`:agent [MESSAGE]`) |
| Alt-Shift-E | send errors to the agent (`:agent errors`) |

## The prompt box

Enter sends, Alt-Enter adds a line, Shift-Tab changes mode, Tab cycles the model, Up/Down history, Esc closes (and cancels while it works).

- **rewrite** / **implement**: the answer appears as a diff in the file. y accept, n reject, u undecided, Up/Down or `]` `[` move, Enter applies (undecided count as accepted), a accepts all, Esc discards. One undo step.
- **ask**: the answer streams into a read-only tab.
- **search**: the hits open in a results tab.

Context chips: `@file`, `#rule` (AGENTS.md / AGENT.md walk-up and `.skink/rules/*.md`), `@selection`, `@diagnostics`, `@terminal`. Slash commands: `/model NAME`, `/ask`, `/search`, `/implement`, `/rewrite`.

Answers from rewrite and implement are code; ask for an explanation and it comes as comments. An answer without code opens in an "AI answer" tab.

## The agent

Files the agent writes are staged and open in the refactor preview when its turn ends (`:review` reopens them). Permission requests show a dialog: Up/Down, Enter or 1-9, Esc rejects. `:agent cancel` stops the turn, `:agent stop` ends the agent, `:agent send` sends the selection or the terminal's screen. `:model [NAME]` shows or sets the model for this run.

## Settings

```json
"ai.agent": "auto",
"ai.model": "default",
"ai.effort": "default",
"ai.model.rewrite": "opus 5.5",
"ai.effort.rewrite": "low"
```

- `ai.agent`: `auto` tries Claude Code first (claude-code-acp, or your `claude` through npx, reusing its login), then gemini, opencode, goose, agy. Or a name (claude, gemini, opencode, goose, antigravity, codex; case, spaces, `-` and `_` don't matter), or a command: `["my-agent", "--acp"]`. Missing adapters run through npx when they can; Skink asks before the first download of each one.
- `ai.model`: a model the agent offers; loose names work (`opus5.5`, `claude-opus-5-5`).
- `ai.effort`: low, medium, high, max (Claude Code: 4k / 10k / 20k / 32k thinking tokens).
- Per mode: `.rewrite`, `.ask`, `.search`, `.implement`.

Saving a change to these tests the agent with a one-line chat in a throwaway folder and shows "Claude Code ✓" or why not. `:agents` lists the agents and the last check; `:agents check` runs it now. `:settings json` puts a reference of agents, models and effort at the top of the file.
