# The installed documentation is the wrong document for one package and points at files that are not installed

- severity: minor
- category: docs
- environment: Linux 7.2.11-arch1-1 (Arch rolling, cloud image), aranicy 0.3.0-1 + aranicy-collector 0.3.0-1, 2 vCPU x86_64, 1958 MiB RAM

## What I was trying to do

Read the documentation that came with the packages — on a headless box with no
network that is the only documentation I have — to find the supported
configuration keys, the compatibility notes for my old ananicy-cpp corpus, and
the documented install/enable steps for the packaged units.

## Steps to reproduce

```
sudo pacman -U --noconfirm ~/payload/aranicy-0.3.0-1-x86_64.pkg.tar.zst \
                         ~/payload/aranicy-collector-0.3.0-1-x86_64.pkg.tar.zst
sha256sum /usr/share/doc/aranicy/README.md /usr/share/doc/aranicy-collector/README.md
find /usr/share/doc/aranicy /usr/share/doc/aranicy-collector -type f
grep -oE 'docs/[A-Za-z]+\.md|contrib/install\.sh|tools/storm/' /usr/share/doc/aranicy/README.md | sort -u
```

## Expected

Each package documents itself. Every relative link in the installed README
resolves to something that is also installed.

## Actual

`aranicy-collector` ships a byte-identical copy of the daemon's README
(`aranicy-collectord` is not described anywhere in it except as a row in a
table). The README's own links point at `docs/BENCHMARKS.md`,
`docs/COMPATIBILITY.md`, `contrib/install.sh` and `tools/storm/`, none of
which exist in either package — the compatibility document is the one that
would answer my questions about an old corpus and about what happens with no
`scx` scheduler, and it is the one that is missing.

## Evidence

```
$ sha256sum /usr/share/doc/aranicy/README.md /usr/share/doc/aranicy-collector/README.md
8c9077a266deb69ab99d885ff2c5b9c838419aa6211eff74cdac8e5f5170d239  /usr/share/doc/aranicy/README.md
8c9077a266deb69ab99d885ff2c5b9c838419aa6211eff74cdac8e5f5170d239  /usr/share/doc/aranicy-collector/README.md

$ find /usr/share/doc/aranicy /usr/share/doc/aranicy-collector -type f
/usr/share/doc/aranicy/README.md
/usr/share/doc/aranicy-collector/README.md

$ grep -oE 'docs/[A-Za-z]+\.md|contrib/install\.sh|tools/storm/' /usr/share/doc/aranicy/README.md | sort -u
contrib/install.sh
docs/BENCHMARKS.md
docs/COMPATIBILITY.md
tools/storm/

$ for f in /usr/share/doc/aranicy/docs /usr/share/doc/aranicy/contrib /usr/share/doc/aranicy/tools; do echo "$f: $(test -e $f && echo present || echo MISSING)"; done
/usr/share/doc/aranicy/docs: MISSING
/usr/share/doc/aranicy/contrib: MISSING
/usr/share/doc/aranicy/tools: MISSING
```

`pacman -Qlp` confirms the package contents are only the binary, unit, config,
README and LICENSE for the daemon, and only the binary, unit, README and
LICENSE for the collector.

## What I could not determine

- Whether those documents exist in the project and were simply not packaged,
  or whether they were never written. I cannot tell from the packages alone.
- Whether the README shipped in `aranicy-collector` is a packaging mistake or
  an intentional "one README for the pair" choice — it is wrong either way,
  because it never mentions the collector's own sockets, cadence or BPF
  requirements.
