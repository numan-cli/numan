# Hook profiles P2 design

P2 adds explicit runtime activation for the `nu-hook` capability from issue #16.
Installing, inspecting, updating, removing, and ordinary `activate` never execute
hook handlers. Users opt in with `numan hook enable <owner/name>` and remove the
owned bindings with `numan hook disable [owner/name ...]`.

## Binding contract

- Supported events: `pre_prompt`, `pre_execution`, `env_change`, and
  `display_output`. Unknown events fail closed.
- Each binding ID is unique within a package and across the active profile.
- Handler paths must be relative `.nu` files contained in the installed payload.
  Numan resolves and canonicalizes each handler before enabling it.
- For `env_change`, `filter` names the environment variable to watch, such as
  `PWD`; it is required and accepts only letters, digits, and underscore.
- For the other events, `filter` is an optional Nushell boolean expression.
  Numan validates the generated candidate with the installed Nu before commit.
- Hook handlers and filters run only when Nushell emits their event in an
  interactive session. They do not run during install, enable, inspect, update,
  disable, or rollback.

## Composition and state

Numan owns only `numan-hooks.nu` in the selected vendor-autoload directory. The
file carries the ownership marker. It appends Numan closures to existing
`$env.config.hooks` lists and preserves user-owned entries. It never edits
`config.nu` or replaces the hooks record wholesale.

`nu_state/hook_profile.json` is the active profile. Lockfile `hook_activation`
records are its per-package Nu-identity projection. A pre-mutation snapshot
captures both lockfile and profile. The generated file embeds the expected
profile and registers callbacks only when it exactly matches the atomic profile
file. A crash between profile and integration-file replacement therefore
disables Numan hooks until a later hook command repairs the projection; it cannot
leave a partially merged callback set.

Duplicate IDs, missing handlers, unsafe paths, empty declarations, unsupported
events, invalid filters, and unresolved dependencies fail before activation.
Active hook packages must be disabled before update or removal. Ordinary
`numan activate` and `numan deactivate` do not manage the hook capability.

## User boundary and current limits

Activation writes the persistent vendor-autoload integration for future Nu
sessions. A child `numan` process cannot mutate the caller's current Nu session.
Disable takes effect in new sessions; an already-open Nu process retains the
callbacks loaded at startup.

`numan info` shows declared events and handlers before activation. `numan hook
list` and `numan list` show active state. Cross-platform real-Nu acceptance on
Linux, macOS, and Windows remains a CI requirement.
