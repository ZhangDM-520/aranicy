# Installing the package leaves the daemon stopped and the only install instructions in the package are for a source tree that is not on the machine

- severity: major
- category: install
- environment: Arch Linux rolling, kernel 7.2.11-arch1-1, aranicy 0.3.0-1 + aranicy-collector 0.3.0-1, 2 vCPU x86_64, 1958 MiB RAM

## What I was trying to do

Install the two packages I was given the way any Arch user installs a package, expect
them to end up with a working daemon, and know what to do next. I asked for the docs
that came with the package, because there is no source tree here and I do not build
software anyway.

## Steps to reproduce

```
./machine 'cd ~/payload && sudo pacman -U --noconfirm aranicy-0.3.0-1-x86_64.pkg.tar.zst aranicy-collector-0.3.0-1-x86_64.pkg.tar.zst'
./machine 'systemctl is-enabled aranicy; systemctl is-active aranicy'
./machine 'cat /usr/share/doc/aranicy/README.md'
./machine 'ls /usr/share/doc/aranicy/'
./machine 'ls /usr/bin | grep -i "contrib\|install.sh"'
./machine 'man aranicy'
```

## Expected

After `pacman -U`: either the service is enabled and running, or the package tells me the
one command to run. The README that ships in `/usr/share/doc/aranicy/` should contain a
quick start I can follow on *this* machine.

## Actual

1. `pacman -U` reports success and says nothing else. The unit is **not enabled and not
   running**. (`pacman -Qi aranicy` → `Install Script  : No`; the symlink only appears
   when I run `systemctl enable` myself, which prints `Created symlink ...`.)
2. The README's "Install" section consists of `./contrib/install.sh` invocations
   (`--dry-run`, `--keep-ananicy-cpp`, `--uninstall`). `contrib/` is not in either
   package. The only sentence aimed at a package install is: *"After install the daemon
   lives in systemd like ananicy-cpp did: `systemctl status aranicy`, `systemctl reload
   aranicy`"* — which tells me how to inspect it, not how to start it.
3. `/usr/share/doc/aranicy/` contains exactly one file, `README.md`. Three of its four
   relative links point at things that are not installed (`docs/BENCHMARKS.md`,
   `docs/COMPATIBILITY.md`, `tools/storm/`) and the banner image is missing too.
4. No man page is packaged.

I got it working in one guess (`systemctl enable --now aranicy`), which is why I am
filing this as *major* and not *blocker*: the software does the job once started. But the
shipped quick start is a dead end on a machine that installed the package, and a user
who has not run ananicy-cpp before has nothing to go on.

## Evidence

Install succeeds, silently:

```
$ ./machine 'cd ~/payload && sudo pacman -U --noconfirm aranicy-0.3.0-1-x86_64.pkg.tar.zst aranicy-collector-0.3.0-1-x86_64.pkg.tar.zst 2>&1; echo "EXIT=$?"'
loading packages...
resolving dependencies...
looking for conflicting packages...

Packages (2) aranicy-0.3.0-1  aranicy-collector-0.3.0-1

Total Installed Size:  6.19 MiB

:: Proceed with installation? [Y/n] 
checking keyring...
checking package integrity...
loading package files...
checking for file conflicts...
checking available disk space...
:: Processing package changes...
installing aranicy-collector...
installing aranicy...
:: Running post-transaction hooks...
(1/2) Reloading system manager configuration...
(2/2) Arming ConditionNeedsUpdate...
EXIT=0
```

A package install (here: a reinstall of the same file, after `disable --now`, so the machine
is in the post-install state) leaves the unit disabled and stopped, and prints nothing
about it:

```
$ ./machine 'cd ~/payload && sudo pacman -U --noconfirm aranicy-0.3.0-1-x86_64.pkg.tar.zst | tail -6; echo "after reinstall: is-enabled=$(systemctl is-enabled aranicy 2>&1) is-active=$(systemctl is-active aranicy 2>&1)"'
:: Processing package changes...
reinstalling aranicy...
:: Running post-transaction hooks...
(1/3) Reloading system manager configuration...
(2/3) Enqueuing marked services...
(3/3) Arming ConditionNeedsUpdate...
after reinstall: is-enabled=disabled is-active=inactive
```

The symlink only exists because I made it:

```
$ ./machine 'sudo systemctl enable --now aranicy'
Created symlink '/etc/systemd/system/multi-user.target.wants/aranicy.service' → '/usr/lib/systemd/system/aranicy.service'.
```

The packaged documentation is one file, and its install path is a script that is not
in the package:

```
$ ./machine 'ls -la /usr/share/doc/aranicy/'
total 8
drwxr-xr-x 1 root root   18 Nov 11 08:15 .
drwxr-xr-x 1 root root  676 Nov 11 08:15 ..
-rw-r--r-- 1 root root 4694 Nov 11 07:57 README.md
$ ./machine 'grep -nE "install.sh|systemctl enable" /usr/share/doc/aranicy/README.md'
91:./contrib/install.sh                  # build if needed, install, enable --now
```

```
$ ./machine 'ls /usr/bin | grep -i "contrib\|install.sh"; echo "EXIT=$?"'
EXIT=1
$ ./machine 'man aranicy'
bash: line 1: man: command not found
```

(No `man` binary in this image, so I checked the files directly — and neither package ships a
page:)

```
$ ./machine 'find /usr/share/man -iname "*aranicy*"; echo "EXIT=$?"; ls /usr/share/man/man8/aranicy* 2>&1'
EXIT=0
ls: cannot access '/usr/share/man/man8/aranicy*': No such file or directory
```

## What I could not determine

- Whether the packaging is meant to ship a pacman hook or an install message (this is a
  packaging choice I cannot see from outside) — I only know that as shipped, nothing
  enables or starts the unit and nothing says to.
- Whether `systemctl status aranicy` on a fresh install is supposed to be the "quick
  start". It prints `inactive (dead)`, which a new user reads as "broken", not as "your
  turn".
