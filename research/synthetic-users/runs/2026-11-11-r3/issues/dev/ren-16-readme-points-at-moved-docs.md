# The README tells me the packaged docs are `/usr/share/doc/aranicy/COMPATIBILITY.md` and `BENCHMARKS.md`; in this build those files are in `docs/` and the paths in the README do not exist

- severity: minor
- category: docs
- environment: Arch Linux cloud image, kernel 7.2.11-arch1-1, aranicy 0.3.0-1 +
  aranicy-collector 0.3.0-1 (build 2026-11-11 09:52 UTC), 2 vCPU / 1958 MiB / no display

## What I was trying to do

Read the compatibility contract and the benchmarks **from the installed
package** — I do not have the source tree, and the README is the only index I
have. It ends the install section with a paragraph that names where the docs
are.

## Steps to reproduce

```sh
sudo pacman -U --noconfirm ~/payload/aranicy-collector-0.3.0-1-x86_64.pkg.tar.zst \
                            ~/payload/aranicy-0.3.0-1-x86_64.pkg.tar.zst
ls -R /usr/share/doc/aranicy
grep -n "packaged documentation" -A2 /usr/share/doc/aranicy/README.md
ls -l /usr/share/doc/aranicy/COMPATIBILITY.md /usr/share/doc/aranicy/BENCHMARKS.md
```

## Expected

The three paths the README names exist.

## Actual

The two files the paragraph names are inside `docs/`; the paths it prints do
not exist. The README's own inline links (`[docs/COMPATIBILITY.md](docs/COMPATIBILITY.md)`)
are correct, so the prose paragraph is the odd one out.

## Evidence

```
$ ls -R /usr/share/doc/aranicy
/usr/share/doc/aranicy:
README.md
docs

/usr/share/doc/aranicy/docs:
BENCHMARKS.md
COMPATIBILITY.md
```

```
$ grep -n "packaged documentation" -A2 /usr/share/doc/aranicy/README.md
117:The packaged documentation is `/usr/share/doc/aranicy/README.md`,
118-`COMPATIBILITY.md` and `BENCHMARKS.md`.
```

```
$ ls -l /usr/share/doc/aranicy/COMPATIBILITY.md /usr/share/doc/aranicy/BENCHMARKS.md
ls: cannot access '/usr/share/doc/aranicy/COMPATIBILITY.md': No such file or directory
ls: cannot access '/usr/share/doc/aranicy/BENCHMARKS.md': No such file or directory
```

The previous build of the same version had the same sentence and the files at
the paths it named, so the packaging moved and the paragraph did not:

```
$ ls ~/x/prev/usr/share/doc/aranicy/
BENCHMARKS.md
COMPATIBILITY.md
README.md
$ grep -n "packaged documentation" -A2 ~/x/prev/usr/share/doc/aranicy/README.md
113:The packaged documentation is `/usr/share/doc/aranicy/README.md`,
114-`COMPATIBILITY.md` and `BENCHMARKS.md`.
```

## What I could not determine

- Whether `docs/` is the intended layout going forward. If it is, the paragraph
  needs the new paths; if it is not, the package is in the wrong shape. Either
  way one of the two is wrong, and I cannot tell which from outside.
