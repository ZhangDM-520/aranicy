# The installed README's three documentation links point at a `docs/` directory that is not installed, so the files it links to are one level up and unreachable by the link

- severity: nit
- category: docs
- environment: Linux 7.2.11-arch1-1 (Arch rolling), aranicy 0.3.0-1, 2 vCPU x86_64, 1958 MiB

## What I was trying to do

Follow the installed README literally, as in round 1 (dana-6): the package now
ships `BENCHMARKS.md` and `COMPATIBILITY.md`, so I opened
`/usr/share/doc/aranicy/README.md` and followed its links.

## Steps to reproduce

```
sudo pacman -U --noconfirm ~/payload/*.pkg.tar.zst
ls -l /usr/share/doc/aranicy/
grep -n '](docs/' /usr/share/doc/aranicy/README.md
ls /usr/share/doc/aranicy/docs
```

## Expected

The links resolve, since all three targets are shipped in the same directory.

## Actual

All three are relative links to `docs/BENCHMARKS.md` (and `docs/COMPATIBILITY.md`),
i.e. `/usr/share/doc/aranicy/docs/...`, which does not exist. The actual files
are `/usr/share/doc/aranicy/BENCHMARKS.md` and `.../COMPATIBILITY.md`. The links
are correct for the source tree and wrong for the package, and the package is
what a user has. This is exactly round 1's dana-6 in a new shape: the file list
was fixed, the links were not.

## Evidence

```
$ pacman -Ql aranicy | grep -v -E "licenses|/$"
aranicy /etc/ananicy.d/ananicy.conf
aranicy /usr/bin/aranicy
aranicy /usr/lib/systemd/system/aranicy.service
aranicy /usr/share/doc/aranicy/BENCHMARKS.md
aranicy /usr/share/doc/aranicy/COMPATIBILITY.md
aranicy /usr/share/doc/aranicy/README.md
$ ls -l /usr/share/doc/aranicy/docs
ls: cannot access '/usr/share/doc/aranicy/docs': No such file or directory
$ readlink -f /usr/share/doc/aranicy/docs/BENCHMARKS.md || echo MISSING
MISSING
$ grep -n -E "docs/BENCHMARKS|docs/COMPATIBILITY" /usr/share/doc/aranicy/README.md
13:[docs/BENCHMARKS.md](docs/BENCHMARKS.md)). The measured Rust candidate
42:([docs/COMPATIBILITY.md](docs/COMPATIBILITY.md)).
55:[docs/BENCHMARKS.md](docs/BENCHMARKS.md) for the full measurement protocol
```

## What I could not determine

- Nothing. This is a path check; either the directory exists in the package or
  it does not.
