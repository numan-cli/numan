# Generated and managed by Numan. Do not edit by hand.
const __NUMAN_OVERLAY_ROOT = @ROOT@

# Records are intentionally not exported to child processes. A nested Nu gets
# its own session identity when this file is sourced by its config.
if ($env.__NUMAN_SESSION? | default {} | get -o pid) != $nu.pid {
    $env.__NUMAN_SESSION = {id: (random uuid), pid: $nu.pid}
}

def __numan_overlay_live [] {
    let markers = ($env | transpose key value | where {|row|
        ($row.key | str starts-with '__NUMAN_OVERLAY__') and (($row.value | describe) | str starts-with 'record')
    } | get value | where {|m| $m.session? == $env.__NUMAN_SESSION.id and $m.name? != null})
    let active = (overlay list | where active | get name)
    let names = ($active | append ($markers | get name) | uniq | sort)
    $names | each {|name|
        let marker = ($markers | where {|m|
            $m.name? == $name and $m.session? == $env.__NUMAN_SESSION.id
        } | first 1)
        {name: $name, txn: ($marker | get -o 0.txn), active: ($name in $active)}
    }
}

def __numan_overlay_context [] {
    ['--session' $env.__NUMAN_SESSION.id '--pid' ($nu.pid | into string)
        '--live' (__numan_overlay_live | to json --raw)]
}

def __numan_overlay_call [args: list<string>] {
    let result = (^numan --root $__NUMAN_OVERLAY_ROOT overlay ...$args | complete)
    if $result.exit_code != 0 {
        error make {msg: ($result.stderr | str trim)}
    }
    $result.stdout | from json
}

def --env __numan_overlay_reconcile [] {
    let signature = (__numan_overlay_live | to json --raw)
    let pending = ($env.__NUMAN_OVERLAY_PENDING? | default {})
    if ($pending.armed? | default false) { return }
    if $signature != ($env.__NUMAN_OVERLAY_SEEN? | default {} | get -o signature) or not ($pending | is-empty) {
        let records = (__numan_overlay_call (['reconcile'] | append (__numan_overlay_context)))
        if ($records | any {|r| $r.status == 'degraded'}) {
            print --stderr 'Numan overlay activation left partial or unverified Nu state. Start a new shell before retrying; its payload remains protected.'
        }
        # A failed CLI call leaves both pending and seen state unchanged so the
        # next prompt retries even if the marker set has not changed.
        $env.__NUMAN_OVERLAY_SEEN = {signature: $signature}
        hide-env --ignore-errors __NUMAN_OVERLAY_PENDING
    }
}

def --env __numan_overlay_arm [prepared: record] {
    let txn = $prepared.txn
    let code = $prepared.code
    if not ($code | nu-check) {
        __numan_overlay_call (['abort' $txn] | append (__numan_overlay_context)) | ignore
        error make {msg: 'Overlay statement failed nu-check. Nothing was armed.'}
    }
    let previous = $env.config.hooks.pre_prompt
    $env.__NUMAN_OVERLAY_PENDING = {txn: $txn, armed: true}
    # Cleanup must precede the parser-changing string. A parse error can skip
    # all later hooks, so neither cleanup nor retries may depend on verification.
    let cleanup = {||
        $env.config.hooks.pre_prompt = $previous
        $env.__NUMAN_OVERLAY_PENDING = {txn: $txn, armed: false}
    }
    let verify = {|| __numan_overlay_reconcile }
    $env.config.hooks.pre_prompt = ($previous | append [$cleanup $code $verify])
}

def __numan_overlay_require_interactive [] {
    if not $nu.is-interactive {
        error make {msg: 'Numan overlay wrappers require an interactive Nu REPL. Use the native overlay statement in scripts.'}
    }
    if ($env.__NUMAN_OVERLAY_PENDING? | default {} | get -o armed | default false) {
        error make {msg: 'An overlay operation is already armed. Wait for the next prompt.'}
    }
}

def --env 'numan overlay use' [package: string, --project-root: path] {
    __numan_overlay_require_interactive
    let args = if $project_root == null { ['prepare' $package] } else { ['prepare' $package '--project-root' $project_root] }
    let prepared = (__numan_overlay_call ($args | append (__numan_overlay_context)))
    if $prepared.already_active { return }
    __numan_overlay_arm $prepared
}

def --env 'numan overlay hide' [target: string] {
    __numan_overlay_require_interactive
    let prepared = (__numan_overlay_call (['prepare-hide' $target] | append (__numan_overlay_context)))
    if ($prepared.code | is-empty) {
        print 'Overlay is not active in this shell.'
        return
    }
    # Hiding must run before its record is removed. Reconciliation observes the
    # missing marker and active-overlay entry after the native statement runs.
    let previous = $env.config.hooks.pre_prompt
    let code = $prepared.code
    if not ($code | nu-check) { error make {msg: 'Overlay hide failed nu-check.'} }
    $env.__NUMAN_OVERLAY_PENDING = {txn: $prepared.txn, armed: true}
    let cleanup = {||
        $env.config.hooks.pre_prompt = $previous
        $env.__NUMAN_OVERLAY_PENDING = {txn: $prepared.txn, armed: false}
    }
    let verify = {|| __numan_overlay_reconcile }
    $env.config.hooks.pre_prompt = ($previous | append [$cleanup $code $verify])
}

def --env 'numan overlay list' [] {
    __numan_overlay_require_interactive
    __numan_overlay_call (['list'] | append (__numan_overlay_context))
}

# Use a conditional record with a stable tag so repeated sourcing is idempotent
# without discarding user hooks or closure values that cannot be serialized.
let retained = ($env.config.hooks.pre_prompt | where {|hook|
    if (($hook | describe) | str starts-with 'record') {
        ($hook.numan_overlay? | default false) == false
    } else { true }
})
$env.config.hooks.pre_prompt = ($retained | append {
    numan_overlay: true,
    code: {|| try { __numan_overlay_reconcile } catch {|error| print --stderr $error.msg} }
})
