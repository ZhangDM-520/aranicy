# The README's "packaged documentation is …" sentence names two files that are not at those paths — the package moved them into `docs/`

- severity: minor
- category: docs
- environment: Arch Linux rolling, kernel 7.2.11-arch1-1, aranicy 0.3.0-1 + aranicy-collector 0.3.0-1, 2 vCPU x86_64, 1958 MiB RAM

## What I was trying to do

Read the shipped documentation. I had read the README, and its Install section
tells me where the rest of the documentation is, so I used that sentence as the
index.

## Steps to reproduce

```
sed -n '117,118p' /usr/share/doc/aranicy/README.md
cat /usr/share/doc/aranicy/COMPATIBILITY.md
cat /usr/share/doc/aranicy/BENCHMARKS.md
ls /usr/share/doc/aranicy/
```

## Expected

The sentence lists paths that exist, or the files are at the paths the sentence
lists.

## Actual

The README says:

```
The packaged documentation is `/usr/share/doc/aranicy/README.md`,
`COMPATIBILITY.md` and `BENCHMARKS.md`.
```

Both of the latter two are actually at `/usr/share/doc/aranicy/docs/`. Following
the sentence gives `No such file or directory` — which, for a file whose name
is the one the daemon's own `Reload liveness` table lives in, is the difference
between "I can read the contract" and "I file a bug about missing docs".

The relative links elsewhere in the README (`docs/COMPATIBILITY.md`,
`docs/BENCHMARKS.md`) *do* resolve, so the move itself is fine; only this one
sentence was not updated with it.

## Evidence

```
$ ./machine 'sed -n "115,120p" /usr/share/doc/aranicy/README.md'
reports `0 rules` until you do, and the daemon warns about it at startup.

The packaged documentation is `/usr/share/doc/aranicy/README.md`,
`COMPATIBILITY.md` and `BENCHMARKS.md`.

### From the tree

$ ./machine 'for f in /usr/share/doc/aranicy/README.md /usr/share/doc/aranicy/COMPATIBILITY.md /usr/share/doc/aranicy/BENCHMARKS.md /usr/share/doc/aranicy/docs/COMPATIBILITY.md /usr/share/doc/aranicy/docs/BENCHMARKS.md; do printf "%-52s " "$f"; test -e "$f" && echo PRESENT || echo MISSING; done'
/usr/share/doc/aranicy/README.md                     PRESENT
/usr/share/doc/aranicy/COMPATIBILITY.md              MISSING
/usr/share/doc/aranicy/BENCHMARKS.md                 MISSING
/usr/share/doc/aranicy/docs/COMPATIBILITY.md         PRESENT
/usr/share/doc/aranicy/docs/BENCHMARKS.md            PRESENT
```

The package contents, straight from the tarball, to show this is the package
and not my install:

```
$ ./machine 'cd ~/pkgcmp; find payload-aranicy/usr/share/doc -type f | sort'
payload-aranicy/usr/share/doc/aranicy/README.md
payload-aranicy/usr/share/doc/aranicy/docs/BENCHMARKS.md
payload-aranicy/usr/share/doc/aranicy/docs/COMPATIBILITY.md
```

For contrast, the previous build had them at the top level and its README used
the same `docs/…` links — so before this build the *links* were broken and now
the *sentence* is. Either way one of the two is wrong.

```
$ ./machine 'cd ~/pkgcmp; find previous-build-aranicy/usr/share/doc -type f | sort'
previous-build-aranicy/usr/share/doc/aranicy/BENCHMARKS.md
previous-build-aranicy/usr/share/doc/aranicy/COMPATIBILITY.md
previous-build-aranicy/usr/share/doc/aranicy/README.md
```

## What I could not determine

- Whether the move was deliberate (it looks like it: `docs/` now matches the
  relative links). If it was, the sentence is a one-line fix.
- Whether the collector's README has the same problem; I did not check it in
  this round.

`workflow this blocks`: "read the docs that came with the package" — the index
sentence points at two files that are not there.

`related`: kaz-10 (round 2 — the collector README's check command also does not
do what its surrounding prose implies).
