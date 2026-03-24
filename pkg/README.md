# AUR Packages

This directory contains PKGBUILDs for the [AUR](https://aur.archlinux.org/):

- `PKGBUILD` — **scrollshot-git**: builds from the latest git commit
- `bin/PKGBUILD` — **scrollshot-bin**: installs a prebuilt binary from a GitHub release

## Publishing to AUR

Generate `.SRCINFO` and push to AUR in one step:

```bash
cd pkg
make publish-git   # publish scrollshot-git
make publish-bin   # publish scrollshot-bin
```

Or just regenerate `.SRCINFO` without pushing:

```bash
make srcinfo-git
make srcinfo-bin
```

## Updating scrollshot-bin

After a new GitHub release:

1. Update `pkgver` in `bin/PKGBUILD`
2. Update `sha256sums` — get it with:
   ```bash
   curl -sL https://github.com/jbonney/scrollshot/releases/download/v<VERSION>/scrollshot-v<VERSION>-x86_64-linux.tar.gz | sha256sum
   ```
3. Run `make publish-bin`
