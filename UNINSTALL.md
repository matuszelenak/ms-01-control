# Uninstalling

## The ms01ctl service

ms01ctl is installed as a Debian package. Stopping/removing it returns the
fans to hardware automatic control.

```bash
apt remove ms01ctl     # remove the service and binary, keep the data
apt purge ms01ctl      # ... and also delete /etc/ms01ctl (config, CPU
                        #     profile, chip snapshot) and /var/lib/ms01ctl
                        #     (NVMe wear history database)
```

## Build toolchains

Rust (rustup) and Node.js were **not** installed system-wide. They live
entirely inside this project at `.toolchain/` (`RUSTUP_HOME` and `CARGO_HOME`
point there; Node is an unpacked official tarball). Nothing was added to your
`PATH`, shell profiles, `~/.cargo`, `~/.rustup`, or the system package manager.

To remove them:

```bash
rm -rf <project dir>/.toolchain
```

Removing the whole project directory also removes them, since they are
contained within it.
