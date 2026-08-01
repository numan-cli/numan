# Generated and managed by Numan. Do not edit.
# Numan autoload schema: 1
# Vendored from https://github.com/aidnem/nushell-loader (MIT, Copyright (c) 2026 aidnem)
# Installed by `numan setup loader`. Re-run with --force to update.

let autoload_dir: path = $nu.data-dir | path join "vendor/autoload"

mkdir $autoload_dir

# Place init commands here, in the following format:
# let aidnem_loader_configs = [
#   {name: 'starship', command: "starship init nu" }
#   {name: 'zoxide', command: "zoxide init nushell" }
#   {name: 'carapace', command: "carapace _carapace nushell"}
# ]
let aidnem_loader_configs: list<record> = []

def _aidnem_loader_get_file_from_name [name] {
  { parent: $autoload_dir, stem: $name, extension: 'nu' } | path join
}

for item in $aidnem_loader_configs {
  let target = _aidnem_loader_get_file_from_name $item.name
  if not ($target | path exists) {
    print $"[Aidnem Loader] File not found for ($item.name), generating it now."
    print $"[Aidnem Loader] Running `($item.command) | save ($target)`"
    nu -n -c $item.command | save $target
  }
}

def _aidnem_loader_completer [context: string, position: int]: nothing -> list {
  $aidnem_loader_configs | get name
}

# Remove a cached init file so that it will be regenerated on next startup.
# Configs are listed in $aidnem_loader_configs
def aidnem_loader_remove_file [...names: string@_aidnem_loader_completer]: nothing -> nothing {
  for name in $names {
    let target = _aidnem_loader_get_file_from_name $name
    print $"[Aidnem Loader] Removing ($target)"
    rm $target
  }
}
