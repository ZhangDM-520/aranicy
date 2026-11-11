# The collector package ships the daemon's README byte for byte, and every cross-reference in both READMEs is dead

- severity: minor
- category: docs
- environment: Arch Linux rolling, kernel 7.2.11-arch1-1, aranicy 0.3.0-1 + aranicy-collector 0.3.0-1, 2 vCPU x86_64, 1958 MiB RAM

## What I was trying to do

Find out what `aranicy-collectord` is, whether I need it, and what it is doing with the
BPF programs I can see in its unit. I installed two packages, so I read two READMEs.

## Steps to reproduce

```
./machine 'cat /usr/share/doc/aranicy-collector/README.md'
./machine 'cmp /usr/share/doc/aranicy/README.md /usr/share/doc/aranicy-collector/README.md; echo "EXIT=$?"'
./machine 'ls /usr/share/doc/aranicy/ /usr/share/doc/aranicy-collector/'
./machine 'grep -o "\](\([^)]*\))" /usr/share/doc/aranicy/README.md | sort -u'
./machine 'ls /usr/share/doc/aranicy/docs /usr/share/doc/aranicy/banner.svg'
```

## Expected

The `aranicy-collector` doc says something about the collector: what it samples, whether
I have to enable it, whether it needs anything from my kernel.

## Actual

`/usr/share/doc/aranicy-collector/README.md` is the daemon README, identical, with the
daemon's title. It says "the collector owns every BPF program" once, in a table, and that
is the whole of the collector documentation. Every other sentence is about the daemon,
the build, or the bench.

The links in it (and in the daemon copy) point at files that are not installed:
`docs/BENCHMARKS.md`, `docs/COMPATIBILITY.md`, `tools/storm/`, `banner.svg`. `LICENSE` is
referenced next to the README but lives in `/usr/share/licenses/aranicy/LICENSE`.

## Evidence

Same file, twice:

```
$ ./machine 'cmp /usr/share/doc/aranicy/README.md /usr/share/doc/aranicy-collector/README.md; echo "EXIT=$?"'
EXIT=0
$ ./machine 'ls -l /usr/share/doc/aranicy/README.md /usr/share/doc/aranicy-collector/README.md'
-rw-r--r-- 1 root root 4694 Nov 11 07:57 /usr/share/doc/aranicy-collector/README.md
-rw-r--r-- 1 root root 4694 Nov 11 07:57 /usr/share/doc/aranicy/README.md
```
Four links, none of them present:

```
$ ./machine 'grep -o "\](\([^)]*\))" /usr/share/doc/aranicy/README.md | sort -u'
](LICENSE)
](docs/BENCHMARKS.md)
](docs/COMPATIBILITY.md)
](tools/storm/)
$ ./machine 'ls -la /usr/share/doc/aranicy/'
total 8
drwxr-xr-x 1 root root   18 Nov 11 08:15 .
drwxr-xr-x 1 root root  676 Nov 11 08:15 ..
-rw-r--r-- 1 root root 4694 Nov 11 07:57 README.md
$ ./machine 'ls /usr/share/doc/aranicy/docs; echo "EXIT=$?"; ls /usr/share/doc/aranicy/banner.svg; echo "EXIT=$?"'
ls: cannot access '/usr/share/doc/aranicy/docs': No such file or directory
EXIT=2
ls: cannot access '/usr/share/doc/aranicy/banner.svg': No such file or directory
EXIT=2
```

## What I could not determine

Nothing much — this one is clear-cut. I did not check whether the source tree has those
files (I have no source tree and did not go looking); I only know they are not in the
package, so the packaged doc cannot be followed past the first screen.
