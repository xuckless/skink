# Updates

| Command | Does |
|---|---|
| `skink --upgrade` (or `skink upgrade`) | install the latest release in place |
| `skink --upgrade --check` | only say whether a newer one exists |
| `skink --upgrade --version 0.1.0` | install that release (can downgrade) |
| `:upgrade` | the same from inside, in the background |

With `"updates": "notify"` (the default) Skink checks GitHub at most once a day and says so in the status line. `"auto"` also installs, used from the next start; `"off"` never checks. `SKINK_NO_UPDATE_CHECK=1` or CI turn the check off.

When GitHub's download links fail, the files come through the GitHub API instead (GH_TOKEN or GITHUB_TOKEN raises its rate limit), with the same checksum check. Installs owned by cargo, Homebrew, scoop or winget print that tool's command instead.

Release builds check a minisign signature on SHA256SUMS when the project signs its releases; `SKINK_ALLOW_UNSIGNED=1` accepts older, unsigned ones.
