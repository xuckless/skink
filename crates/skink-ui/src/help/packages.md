# Packages and languages

Built in: Rust, TypeScript/TSX, JavaScript, Python, Go, JSON, TOML, Bash, Markdown and YAML. Everything else is a package: Zig, Dart, HTML, CSS, Lua, Make, INI, diff, XML/SVG, and kits for Kotlin, Java, C/C++, C#, Ruby, PHP, Swift, Elixir, Haskell, Scala, Nix, Svelte and SQL.

Opening a file with no grammar says which package to install: "No grammar for .kt: :install kotlin".

| In Skink | From a shell | Does |
|---|---|---|
| `:install kotlin` | `skink --install kotlin` | a verified package by name |
| `:install .` | `skink --install .` | what this project needs (asks first; `-y` yes) |
| `:uninstall NAME` | `skink --uninstall NAME` | remove everything it placed |
| `:packages` | `skink --list` | what is installed |
| | `skink --search TEXT` | find packages |
| | `skink --update-packages` | update them |

Other sources: `owner/repo[/pkg][@tag]` (any GitHub release, unverified) and `./my-package` (a folder with a skink-package.toml). A project lists its own in `.skink/packages.json`: `{"packages": ["kotlin"]}`. `:packages disable NAME` keeps a package but turns it off.

Grammars and themes apply at once; language servers and scripts at the next start. A package's Lua script asks once per version, like a project's `.skink/init.lua`.

> [!NOTE]
> Short names only install verified packages, and names that look like one (Kotlin, kot1in, font_pack) are refused.

## How it works

A grammar is WebAssembly, loaded the first time a file needs it. A package's language server and tools are downloaded for your platform, checked against the SHA-256 the package pins, and put on the PATH of language servers and terminal tabs only. Code in Markdown fences, HTML `<script>` and `<style>` is coloured too.

Files live in `<data>/packages`, `<data>/tools` and `<data>/installed.json`, where `<data>` is `$SKINK_DATA_DIR`, else `~/.local/share/skink` (`%LOCALAPPDATA%\skink\data` on Windows). Fonts go to your user font folder. `SKINK_INDEX_URL` points `--search` at another index. Builds with `--features all-langs` have every grammar built in.
