# The packaged README still names `/usr/share/doc/aranicy/COMPATIBILITY.md` and `BENCHMARKS.md`, which this build moved into `docs/`

- severity: nit
- category: docs
- environment: Linux 7.2.11-arch1-1 (Arch rolling, cloud image), aranicy 0.3.0-1 + aranicy-collector 0.3.0-1, 2 vCPU x86_64, 1958 MiB RAM

## What I was trying to do

Read the shipped compatibility document on a box with no network. The README is
the only index I have, so I copy the path it gives me.

## Steps to reproduce

```
sudo pacman -U --noconfirm ~/payload/aranicy-collector-0.3.0-1-x86_64.pkg.tar.zst \
                         ~/payload/aranicy-0.3.0-1-x86_64.pkg.tar.zst
sed -n '115,122p' /usr/share/doc/aranicy/README.md
ls -la /usr/share/doc/aranicy/
cat /usr/share/doc/aranicy/COMPATIBILITY.md
```

## Expected

The paths the README names exist. On the previous build they did.

## Actual

```
$ sed -n '115,122p' /usr/share/doc/aranicy/README.md
reports `0 rules` until you do, and the daemon warns about it at startup.

The packaged documentation is `/usr/share/doc/aranicy/README.md`,
`COMPATIBILITY.md` and `BENCHMARKS.md`.

$ ls -la /usr/share/doc/aranicy/
total 8
drwxr-xr-x 1 root root   26 Nov 11 10:28 .
drwxr-xr-x 1 root root  676 Nov 11 10:28 ..
-rw-r--r-- 1 root root 6653 Nov 11 09:52 README.md
drwxr-xr-x 1 root root   58 Nov 11 10:28 docs

$ cat /usr/share/doc/aranicy/COMPATIBILITY.md
cat: /usr/share/doc/aranicy/COMPATIBILITY.md: No such file or directory
[exit=1]
```

The files are one directory deeper:

```
$ ls -la /usr/share/doc/aranicy/docs
total 48
drwxr-xr-x 1 root root    58 Nov 11 10:28 .
drwxr-xr-x 1 root root    26 Nov 11 10:28 ..
-rw-r--r-- 1 root root 25420 Nov 11 09:52 BENCHMARKS.md
-rw-r--r-- 1 root root 19716 Nov 11 09:52 COMPATIBILITY.md
```

## Evidence

The two blocks above are from `raw/focus-cur.txt` (section FOCUS5). For
comparison, the previous build installed them flat and the README's sentence
was accurate:

```
$ ls -la /usr/share/doc/aranicy
-rw-r--r-- 1 root root 25420 Nov 11 08:59 BENCHMARKS.md
-rw-r--r-- 1 root root 18632 Nov 11 08:59 COMPATIBILITY.md
-rw-r--r-- 1 root root  6346 Nov 11 08:59 README.md
$ ls: cannot access '/usr/share/doc/aranicy/docs': No such file or directory
```

So this is a small regression introduced by the documentation move:
the README sentence (and the internal link in the README's first paragraph,
`[docs/COMPATIBILITY.md](docs/COMPATIBILITY.md)`) were not updated for the
packaged layout.

## What I could not determine

- Whether the move into `docs/` is intentional (the README in the build tree
  links `docs/...`, which suggests the packaged copy is meant to mirror the
  tree). If so, only the "The packaged documentation is ..." sentence needs to
  change.
