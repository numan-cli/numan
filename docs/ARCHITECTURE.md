# Numan Architecture & State Management Reference

This document provides a technical deep-dive into Numan's internal architecture, directory layout, lockfile schemas, journal recovery state machines, and lifecycle management.

## Installation & Path Layout

Install paths are immutable. Packages are placed strictly under:
`<root>/packages/<type>/<owner>/<name>/<version>-<sha8prefix>/`

- **Inert Install**: Downloading and extracting packages writes exclusively to `$NUMAN_ROOT`. No Nushell config modification occurs during `install`.
- **Lockfile Pins**: Installed payloads are retained and tracked via the lockfile (`lockfile.json`).

## State Management & Invariants

1. **Lockfile (v2)**: Authoritative record of installed packages and activations (`PluginActivation`, `ModuleActivation`, `revision_id`, `payload_sha256`).
2. **Autoload Projection**: `autoload-state.json` is a fast projection of active modules. It is **not** authoritative; `lockfile.json` is the sole ground truth.
3. **Active Version Marker**: `nu_state/active-version.json` stores `{ "version": "X.Y.Z" }` (and optional `binary_path`). Sole authority for which managed Nushell version under `tools/nushell/<v>/` is active.
4. **Activation Profiles**: `state/activation-profile.json` tracks desired per-Nu-minor package activation sets, allowing seamless cross-minor version switches (`numan use`).
5. **Mutation Safety**:
   - Every mutation must acquire `acquire_mutation_lock(root)`.
   - All state JSON updates must use `write_json_atomic` (tempfile write + atomic persist).
   - Pre-mutation activation snapshots (`create_snapshot`) are taken before state modifications (`install`, `update`, `remove`, `activate`, `deactivate`, `use`, `nupm import`).

## Recovery Journals

Numan uses journal files under `<root>/state/` for fail-closed crash recovery:
- `pending-activation.json`: Plugin activation journal (`Prepared` -> `Registered`).
- `pending-plugin-deactivate.json`: Plugin deactivation journal (`Prepared` -> `Unregistered`).
- `autoload-journal.json`: Module autoload journal (`PendingAutoload`, `Prepared` -> `Replaced`).
- `pending-lifecycle.json`: General lifecycle mutations (`update`, `remove`, `nupm import`).
- `migration-journal.json`: Legacy single-binary -> versioned-layout transition (`Prepared` -> `Renamed` -> `Active`).

Every well-formed journal is reconciled automatically on CLI execution or via `numan doctor --fix`.

## Nu Lifecycle Boundaries

- **Nu Integration Ownership**: Only `activate` and `deactivate` subcommands directly execute Nushell plugin registration/unregistration.
- **Active Plugin Update Gate**: Updating an active plugin orchestrates deactivate -> upgrade -> activate only when `NUMAN_ENABLE_ACTIVE_PLUGIN_MUTATION=1` is explicitly set. Defaults off to prevent unexpected process mutation.
- **Managed File Ownership**: Nushell configuration files managed by Numan contain the `OWNERSHIP_MARKER` header. Numan will assert ownership before modifying external files.
