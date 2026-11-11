# Splits, docks and zoom

| Key | Does |
|---|---|
| Ctrl-\ (old terminals: Ctrl-4) | split side by side (`:vsplit [FILE]`) |
| Alt-\ | split top and bottom (`:split [FILE]`) |
| Ctrl-W / Alt-Shift-W | next / previous pane |
| Alt-Q | close the pane; its tab stays open (`:unsplit`, `:only`) |
| Alt-= / Alt-- | grow / shrink the pane (or drag a divider or title row) |
| Alt-T | a shell docked along the bottom (`:termdock`) |
| Alt-Shift-T | the agent panel on the right (`:agentdock`) |
| Alt-Z | zoom: only the text (`:zoom`) |

Every pane shows one tab. A file in two panes is one buffer with two cursors: edits show in both. Clicking a pane focuses it; switching tabs changes the focused pane. A terminal shows in one pane at a time; hiding a dock keeps its shell running in its tab.

Zoom hides the tab bar, tree and status line; the status line comes back while a prompt or message is up.

## Font size

Font size belongs to the terminal (Ctrl-= / Ctrl-- in most). `:fontsize +1|-1|0` (`:font`, actions FontBigger, FontSmaller, FontReset) asks EveryAI when Skink runs inside it, or Kitty when remote control is allowed (`allow_remote_control` in kitty.conf).
