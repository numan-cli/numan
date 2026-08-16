# Official Nushell Release & Stewardship Log

Chronological ledger of official Nushell releases, changelogs, ABI bands, and ecosystem impact checklists.

---

## Nushell 0.115.0 (2026-08-15)

- **Release Notes**: [https://github.com/nushell/nushell/releases/tag/0.115.0](https://github.com/nushell/nushell/releases/tag/0.115.0)
- **Official Blog & Detailed Changelog**: [https://www.nushell.sh/blog/2026-08-15-nushell_v0_115_0.html](https://www.nushell.sh/blog/2026-08-15-nushell_v0_115_0.html)
- **Plugin Protocol / ABI Band**: `nu-plugin` `0.115.x` (`nu_version: ">=0.115.0 <0.116.0"`)

### Stewardship & Ecosystem Checklist
- [ ] Review breaking engine/protocol changes affecting plugins
- [ ] Audit `numan-maintained/*` fork dependencies (`Cargo.toml`)
- [ ] Update `manifest.json` active plugin versions in `numan-plugins`
- [ ] Rebuild & test active plugins for `0.115.x` band via `build.yml`
- [ ] Update `numan` CI acceptance matrix (`.github/workflows/ci.yml`)

---
