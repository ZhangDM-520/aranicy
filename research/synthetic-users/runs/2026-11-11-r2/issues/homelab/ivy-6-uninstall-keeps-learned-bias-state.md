# Removing both packages leaves `/var/lib/aranicy/bias-state` behind, and the next install re-applies the learned nice deltas

- severity: minor
- category: packaging
- environment: Linux 7.2.11-arch1-1 (Arch rolling, cloud image), aranicy 0.3.0-1 + aranicy-collector 0.3.0-1, 2 vCPU x86_64, 1958 MiB RAM

## What I was trying to do

Uninstall cleanly, then reinstall later without the box silently resuming
priorities that were learned before the uninstall. My rule is "after `pacman
-R`, nothing that was installed may still influence priority". Round 1's
version of this (processes, units, symlink) is fixed; this is what is left.

## Steps to reproduce

```
sudo pacman -U --noconfirm ~/payload/aranicy-collector-0.3.0-1-x86_64.pkg.tar.zst \
                         ~/payload/aranicy-0.3.0-1-x86_64.pkg.tar.zst
sudo systemctl enable --now aranicy
aranicy dump rules                       # empty: no rule names openssl
setsid timeout 150 openssl speed -seconds 150 -multi 2 sha256 >/dev/null 2>&1 &
# let it run ~90 s so the bias engine learns openssl
sudo pacman -R --noconfirm aranicy aranicy-collector
sudo ls -la /var/lib/aranicy/
sudo python3 -c "import json;d=json.load(open('/var/lib/aranicy/bias-state'));print('version',d['version'],'entries',len(d['entries']));[print(e['name'],'bias',e['bias'],'class',e['class'],'conf',round(e['confidence'],3)) for e in d['entries'] if e['bias']]"
# now reinstall, with a rule set that does NOT mention openssl:
sudo pacman -U --noconfirm ~/payload/aranicy-collector-0.3.0-1-x86_64.pkg.tar.zst \
                         ~/payload/aranicy-0.3.0-1-x86_64.pkg.tar.zst
sudo systemctl enable --now aranicy; sleep 4
setsid timeout 60 openssl speed -seconds 60 -multi 2 sha256 >/dev/null 2>&1 & sleep 5
ps -eo ni,comm --no-headers | grep openssl
```

## Expected

Either the state file is removed with the package, or the reinstall starts from
a blank slate, or at least the reinstall says that it is re-applying learned
state that predates the current installation.

## Actual

The state survives `pacman -R` (only the
`/usr/lib/systemd/system/*.service` files and the binaries are removed;
`bias_state_path` is not in the package's file list and `StateDirectory=` is
not cleaned up by `systemctl disable --now`). On the next install the daemon
logs only `INFO bias: restored history entries=55` — no hint that those
entries came from a previous installation — and a process the current rule set
does not mention is given `rule_nice + restored_bias`.

## Evidence

```
$ sudo pacman -R --noconfirm aranicy aranicy-collector
...
removing aranicy...
removing aranicy-collector...
REMOVE_EXIT=0
$ sudo ls -la /var/lib/aranicy/
total 12
drwxr-xr-x 1 root root   20 Nov 11 09:10 .
drwxr-xr-x 1 root root  188 Nov 11 09:06 ..
-rw-r--r-- 1 root root 8981 Nov 11 09:10 bias-state

$ sudo python3 -c "import json;d=json.load(open('/var/lib/aranicy/bias-state'));print('version',d['version'],'entries',len(d['entries']));[print(e['name'],'bias',e['bias'],'class',e['class'],'conf',round(e['confidence'],3)) for e in d['entries'] if e['bias']]"
version 1 entries 55
openssl bias 3 class bulk conf 0.87

# reinstall + enable, rules present are {"name": "sleep", "nice": 5} and
# {"name": "openssl", "nice": 6, "oom_score_adj": 500}
2026-11-11T09:17:41.393784Z  INFO bias: restored history entries=55

$ setsid timeout 60 openssl speed -seconds 60 -multi 2 sha256 >/dev/null 2>&1 & sleep 5
$ ps -eo ni,comm --no-headers | grep openssl
9,9,9,          # 6 from the rule + 3 restored from before the uninstall
```

With the `openssl` rule then removed and the daemon restarted, a fresh
`openssl` gets the restored bias alone:

```
$ aranicy dump rules
{
  "exact": [
    { "match": "exact", "name": "sleep", "nice": 5 }
  ],
  "regex": []
}
$ setsid timeout 40 openssl speed -seconds 40 -multi 2 sha256 >/dev/null 2>&1 & sleep 6
openssl nice after reinstall-only state: 9,9,9,3,3,3,
#                                                 ^ the fresh processes: bias 3, no rule
```

## What I could not determine

- Whether removing learned state on uninstall is considered wrong (it is
  *state*, not configuration, and keeping it across an upgrade is reasonable).
  It is the silent re-application after a *remove then install* that worries
  me, and there is no way to tell the two apart from the outside.
- Whether `bias_state_path` was deliberately left out of the unit's
  `StateDirectory` cleanup. `StateDirectory=aranicy` is what creates
  `/var/lib/aranicy`; nothing in the package removes it.
