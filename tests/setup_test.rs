//! `numan setup loader` integration tests.

use numan_cli::cmd::setup::{
    config_already_sources_loader, execute_loader_with_probe, execute_loader_with_probe_and_root,
    parse_loader_config, read_loader_config, render_loader_config, LoaderArgs, LoaderConfigEntry,
};

#[test]
fn setup_loader_install_and_configure_without_live_nu() {
    let dir = tempfile::tempdir().unwrap();
    let config_path = dir.path().join("config.nu");
    std::fs::write(&config_path, "# user config\n").unwrap();

    let args = LoaderArgs {
        force: false,
        configure: true,
        yes: true,
        ..Default::default()
    };

    execute_loader_with_probe(&args, || Ok(config_path.clone())).unwrap();

    let loader_path = dir.path().join("loader.nu");
    assert!(loader_path.is_file());
    let loader = std::fs::read_to_string(&loader_path).unwrap();
    assert!(loader.contains("aidnem_loader_configs"));
    assert!(loader.contains("github.com/aidnem/nushell-loader"));

    let loader_config_path = dir.path().join("loader-config.nu");
    assert!(loader_config_path.is_file());

    let config = std::fs::read_to_string(&config_path).unwrap();
    assert!(config_already_sources_loader(&config));
}

#[test]
fn setup_loader_add_and_remove_tool() {
    let dir = tempfile::tempdir().unwrap();
    let config_path = dir.path().join("config.nu");
    std::fs::write(&config_path, "# user config\n").unwrap();

    // 1. Initial setup
    let args = LoaderArgs {
        configure: true,
        yes: true,
        ..Default::default()
    };
    execute_loader_with_probe(&args, || Ok(config_path.clone())).unwrap();

    // 2. Add preset starship
    let add_args = LoaderArgs {
        add: Some("starship".to_string()),
        yes: true,
        ..Default::default()
    };
    execute_loader_with_probe(&add_args, || Ok(config_path.clone())).unwrap();

    let loader_config_path = dir.path().join("loader-config.nu");
    let configs = read_loader_config(&loader_config_path).unwrap();
    assert_eq!(configs.len(), 1);
    assert_eq!(configs[0].name, "starship");
    assert_eq!(configs[0].command, "starship init nu");

    // 3. Add custom tool
    let add_custom = LoaderArgs {
        add: Some("custom=echo custom_init".to_string()),
        yes: true,
        ..Default::default()
    };
    execute_loader_with_probe(&add_custom, || Ok(config_path.clone())).unwrap();

    let configs = read_loader_config(&loader_config_path).unwrap();
    assert_eq!(configs.len(), 2);
    assert_eq!(configs[1].name, "custom");
    assert_eq!(configs[1].command, "echo custom_init");

    // 4. Remove starship
    let remove_args = LoaderArgs {
        remove: Some("starship".to_string()),
        yes: true,
        ..Default::default()
    };
    execute_loader_with_probe(&remove_args, || Ok(config_path.clone())).unwrap();

    let configs = read_loader_config(&loader_config_path).unwrap();
    assert_eq!(configs.len(), 1);
    assert_eq!(configs[0].name, "custom");
}

#[test]
fn setup_loader_add_rejects_traversal_names() {
    let dir = tempfile::tempdir().unwrap();
    let config_path = dir.path().join("config.nu");
    std::fs::write(&config_path, "# user config\n").unwrap();

    let args = LoaderArgs {
        yes: true,
        ..Default::default()
    };
    execute_loader_with_probe(&args, || Ok(config_path.clone())).unwrap();

    // Attempt to add a tool with path traversal in the name
    let add_args = LoaderArgs {
        add: Some("../../../escape=echo bad".to_string()),
        yes: true,
        ..Default::default()
    };
    let result = execute_loader_with_probe(&add_args, || Ok(config_path.clone()));
    assert!(result.is_err(), "path traversal name should be rejected");
    let msg = format!("{:#}", result.unwrap_err());
    assert!(
        msg.contains("may only contain")
            || msg.contains("must be 1-64")
            || msg.contains("must start with"),
        "expected name validation error, got: {msg}"
    );
}

#[test]
fn setup_loader_config_isolation_preserves_user_entries_on_force() {
    let dir = tempfile::tempdir().unwrap();
    let config_path = dir.path().join("config.nu");
    std::fs::write(&config_path, "# user config\n").unwrap();

    // Add preset tool
    let add_args = LoaderArgs {
        add: Some("zoxide".to_string()),
        yes: true,
        ..Default::default()
    };
    execute_loader_with_probe(&add_args, || Ok(config_path.clone())).unwrap();

    // Add a custom entry
    let add_custom = LoaderArgs {
        add: Some("mytool=some_command".to_string()),
        yes: true,
        ..Default::default()
    };
    execute_loader_with_probe(&add_custom, || Ok(config_path.clone())).unwrap();

    let loader_config_path = dir.path().join("loader-config.nu");
    let configs = read_loader_config(&loader_config_path).unwrap();
    assert_eq!(configs.len(), 2);
    assert_eq!(configs[0].name, "zoxide");
    assert_eq!(configs[1].name, "mytool");
    assert_eq!(configs[1].command, "some_command");

    // Force re-install loader.nu engine
    let force_args = LoaderArgs {
        force: true,
        yes: true,
        ..Default::default()
    };
    execute_loader_with_probe(&force_args, || Ok(config_path.clone())).unwrap();

    // Both entries must remain intact
    let configs_after = read_loader_config(&loader_config_path).unwrap();
    assert_eq!(configs_after.len(), 2);
    assert_eq!(configs_after[0].name, "zoxide");
    assert_eq!(configs_after[1].name, "mytool");
    assert_eq!(configs_after[1].command, "some_command");
}

#[test]
fn setup_loader_status_runs_cleanly() {
    let dir = tempfile::tempdir().unwrap();
    let config_path = dir.path().join("config.nu");
    std::fs::write(&config_path, "# user config\n").unwrap();

    let setup_args = LoaderArgs {
        add: Some("starship".to_string()),
        configure: true,
        yes: true,
        ..Default::default()
    };
    execute_loader_with_probe(&setup_args, || Ok(config_path.clone())).unwrap();

    let status_args = LoaderArgs {
        status: true,
        ..Default::default()
    };
    execute_loader_with_probe(&status_args, || Ok(config_path.clone())).unwrap();
}

#[test]
fn setup_loader_detect_discovers_installed_tool() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("numan-root");
    let config_path = dir.path().join("config.nu");
    std::fs::write(&config_path, "# user config\n").unwrap();

    // Plant a fake binary in root/tools/bin
    let tools_bin = root.join("tools").join("bin");
    std::fs::create_dir_all(&tools_bin).unwrap();
    let fake_starship = tools_bin.join(if cfg!(windows) {
        "starship.exe"
    } else {
        "starship"
    });
    std::fs::write(&fake_starship, b"fake").unwrap();
    // Ensure starship is executable so Unix systems detect it
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&fake_starship, std::fs::Permissions::from_mode(0o755)).unwrap();
    }

    let detect_args = LoaderArgs {
        detect: true,
        yes: true,
        ..Default::default()
    };

    execute_loader_with_probe_and_root(&detect_args, Some(&root), || Ok(config_path.clone()))
        .unwrap();

    let loader_config_path = dir.path().join("loader-config.nu");
    let configs = read_loader_config(&loader_config_path).unwrap();
    assert!(configs.iter().any(|e| e.name == "starship"));
}

#[test]
fn loader_config_roundtrip_with_escaping() {
    let entries = vec![
        LoaderConfigEntry {
            name: "starship".to_string(),
            command: "starship init nu".to_string(),
        },
        LoaderConfigEntry {
            name: "mytool".to_string(),
            command: r#"echo "hello world""#.to_string(),
        },
        LoaderConfigEntry {
            name: "escaped".to_string(),
            command: r#"run "C:\path\to\app""#.to_string(),
        },
    ];

    let rendered = render_loader_config(&entries);
    let parsed = parse_loader_config(&rendered);
    assert_eq!(entries, parsed);
}
