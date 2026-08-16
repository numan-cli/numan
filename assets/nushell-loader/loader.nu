# Generated and managed by Numan. Do not edit.
# Numan autoload schema: 1
# Vendored from https://github.com/aidnem/nushell-loader (MIT, Copyright (c) 2026 aidnem)
# Installed by `numan setup loader`. Re-run with --force to update.

let autoload_dir: path = $nu.data-dir | path join "vendor/autoload"
mkdir $autoload_dir

# Configuration is loaded from `loader-config.nu` in the same directory as loader.nu
let loader_config_file = ($nu.config-path | path dirname | path join 'loader-config.nu')

let aidnem_loader_configs: list<record> = if ($loader_config_file | path exists) {
  source $loader_config_file
  $aidnem_loader_configs
} else {
  []
}

def _aidnem_loader_get_file_from_name [name: string] {
  { parent: $autoload_dir, stem: $name, extension: 'nu' } | path join
}

for item in $aidnem_loader_configs {
  let target = _aidnem_loader_get_file_from_name $item.name
  if not ($target | path exists) {
    print $"[Aidnem Loader] Generating cache for ($item.name)..."
    try {
      let res = (nu -n -c $item.command | complete)
      if $res.exit_code == 0 and not ($res.stdout | is-empty) {
        $res.stdout | save -f $target
        print $"[Aidnem Loader] Successfully cached ($item.name) -> ($target)"
      } else {
        print -e $"[Aidnem Loader] Warning: Failed to generate ($item.name) (exit code ($res.exit_code))"
        if not ($res.stderr | is-empty) {
          print -e $"[Aidnem Loader] ($res.stderr)"
        }
      }
    } catch { |err|
      print -e $"[Aidnem Loader] Error generating ($item.name): ($err.msg)"
    }
  }
}

def _aidnem_loader_completer [context: string, position: int]: nothing -> list<string> {
  $aidnem_loader_configs | get -i name | default []
}

# Remove a cached init file so that it will be regenerated on next startup.
# Configs are listed in $aidnem_loader_configs (from loader-config.nu)
def aidnem_loader_remove_file [...names: string@_aidnem_loader_completer]: nothing -> nothing {
  for name in $names {
    let target = _aidnem_loader_get_file_from_name $name
    if ($target | path exists) {
      print $"[Aidnem Loader] Removing ($target)"
      rm -f $target
    }
  }
}
