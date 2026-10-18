# Uninstalling

## The fancurve service

fancurve is installed as a Debian package. Stopping/removing it returns the
fans to hardware automatic control.

```bash
apt remove fancurve     # remove the service and binary, keep the data
apt purge fancurve      # ... and also delete /etc/fancurve (config, CPU
                        #     profile, chip snapshot) and /var/lib/fancurve
                        #     (NVMe wear history database)
```

## Build toolchains

Rust (rustup) and Node.js were **not** installed system-wide. They live
entirely inside this project at `.toolchain/` (`RUSTUP_HOME` and `CARGO_HOME`
point there; Node is an unpacked official tarball). Nothing was added to your
`PATH`, shell profiles, `~/.cargo`, `~/.rustup`, or the system package manager.

To remove them:

```bash
rm -rf /root/fancurve/.toolchain
```

Removing the whole project directory (`rm -rf /root/fancurve`) also removes
them, since they are contained within it.
