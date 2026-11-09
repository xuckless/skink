# Releasing Skink

Push a tag (`git tag -a v0.2.0 -m "Skink 0.2.0" && git push origin v0.2.0`). The
release workflow builds the five targets, publishes the archives with
per-archive `.sha256` files, `SHA256SUMS`, `install.sh` and `install.ps1`, and
then installs and self-updates on Linux (x86_64, arm64), macOS and Windows.

## Signed releases (one-time setup)

Without this, releases are checksum-only: `skink --upgrade` and the installers
detect corrupted downloads but cannot tell a release from an impostor. With it,
`SHA256SUMS` is signed with [minisign](https://jedisct1.github.io/minisign/)
and every release build refuses an update whose signature does not verify.

1. Install minisign locally (`brew install minisign`, `apt install minisign`,
   `scoop install minisign`) and create a key pair **without a password**
   (the GitHub secret store protects it; the workflow cannot type a password):

   ```sh
   minisign -G -W -p skink.pub -s skink.key
   ```

2. Add the **secret key** as a repository secret named `MINISIGN_SECRET_KEY`
   (the whole file, both lines):

   ```sh
   gh secret set MINISIGN_SECRET_KEY < skink.key
   ```

3. Add the **public key** (the second line of `skink.pub`, starting with `RW`)
   as a repository variable named `MINISIGN_PUBLIC_KEY`:

   ```sh
   gh variable set MINISIGN_PUBLIC_KEY --body "$(tail -n 1 skink.pub)"
   ```

4. Keep `skink.key` somewhere safe offline (a password manager) and delete the
   local copy. Losing it means rotating the key (below); leaking it means
   anyone can sign releases that Skink accepts.

From the next tag on:

- the publish job signs `SHA256SUMS` into `SHA256SUMS.minisig` and checks the
  signature against the public key before publishing;
- binaries embed the public key (`SKINK_MINISIGN_PUBKEY` at build time) and
  `skink --upgrade` requires a valid signature, then checks the archive against
  its line in `SHA256SUMS`;
- the published `install.sh` carries the public key and verifies the signature
  when `minisign` is installed on the user's machine.

Releases from before signing (v0.1.0) have no signature. A signing build only
installs them with `SKINK_ALLOW_UNSIGNED=1` (the release workflow's downgrade
check sets it).

## Rotating the key

Each binary embeds exactly one public key. After switching to a new pair,
binaries built with the old key reject new releases, so either:

- tell users to reinstall once with the one-liner, or
- cut one transition release signed with the **old** secret key but built with
  the **new** public key (set `MINISIGN_PUBLIC_KEY` to the new key, keep the old
  `MINISIGN_SECRET_KEY`, and drop the `minisign -V` self-check line in
  `release.yml` for that tag), then switch the secret.
