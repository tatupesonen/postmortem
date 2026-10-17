<p align="center">
  <picture>
    <source media="(prefers-color-scheme: dark)" srcset="assets/brand/postmortem-wordmark-on-dark.png">
    <img alt="postmortem" src="assets/brand/postmortem-wordmark-on-light.svg" width="360">
  </picture>
</p>

<p align="center">
  A fast digital forensics workbench in pure Rust, a port of <a href="https://www.autopsy.com">Autopsy</a>.
</p>

<p align="center">
  <a href="https://github.com/tatupesonen/postmortem/actions/workflows/ci.yml"><img alt="CI" src="https://github.com/tatupesonen/postmortem/actions/workflows/ci.yml/badge.svg"></a>
  <a href="https://github.com/tatupesonen/postmortem/releases/latest"><img alt="Download" src="https://img.shields.io/badge/download-latest-D2462B"></a>
</p>

---

- **Evidence:** raw, split and E01 disk images; NTFS, FAT, exFAT and ext file systems, including deleted files
- **Analysis:** web activity, Windows registry and event logs, hash lookup (MD5, SHA-1, SHA-256)
- **Review:** timeline, file discovery, image gallery, sortable views, pop-out viewers

Evidence is only ever read, never modified.

## Get it

Download a build for Windows, macOS or Linux from [Releases](https://github.com/tatupesonen/postmortem/releases), or build it yourself:

```sh
cargo run --release -p postmortem
```

## Status

Early, under active development. [`docs/PARITY.md`](docs/PARITY.md) tracks progress towards feature parity with Autopsy.

## License

[Apache-2.0](https://www.apache.org/licenses/LICENSE-2.0)
