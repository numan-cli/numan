"""
Apply the unresolved PR 67 review fix to src/nu/bootstrap.rs:

  * `install_from_archive` must write into the versioned layout
    (`<root>/tools/nushell/<version>/nu`), not the legacy single-binary
    layout (`<root>/tools/nushell/nu`). The caller is the only place that
    may update active-marker / activation state — install stays a pure
    payload write.
  * The `install_from_zip_places_managed_binary` unit test must assert
    against the versioned binary path so it doesn't pass via the legacy
    layout it should no longer use.
"""
from pathlib import Path

p = Path('src/nu/bootstrap.rs')
text = p.read_text(encoding='utf-8')

# 1. install_from_archive: switch to versioned layout
old_body = """    let source = locate_extracted_nu_binary(&extract_root)?;
    let dest_dir = managed_nu_dir(root);
    std::fs::create_dir_all(&dest_dir).with_context(|| {
        format!(
            \"Failed to create managed Nushell directory '{}'\",
            dest_dir.display()
        )
    })?;
    let dest = managed_nu_binary(root);

    std::fs::copy(&source, &dest).with_context(|| {
        format!(
            \"Failed to copy Nushell binary from '{}' to '{}'\",
            source.display(),
            dest.display()
        )
    })?;
    make_executable(&dest)?;
    std::fs::write(dest_dir.join(\"VERSION\"), version.as_bytes())?;
    Ok(dest)
}"""
assert text.count(old_body) == 1, f"expected 1 match for body, found {text.count(old_body)}"

new_body = """    let source = locate_extracted_nu_binary(&extract_root)?;
    // Write into the versioned layout (`<root>/tools/nushell/<version>/nu`) so
    // a single install action never clobbers another installed version. Install
    // is a pure payload write: the caller (`execute_nu_setup_with_installer`)
    // owns active-marker persistence; per AGENTS.md, only `activate`/`deactivate`
    // modify Nu integration state.
    let normalized = version_manager::normalize_version(version).with_context(|| {
        format!(\"Invalid Nu version '{}' for installation\", version)
    })?;
    let dest_dir = version_manager::version_install_dir(root, &normalized);
    std::fs::create_dir_all(&dest_dir).with_context(|| {
        format!(
            \"Failed to create managed Nushell version directory '{}'\",
            dest_dir.display()
        )
    })?;
    let dest = version_manager::version_binary(root, &normalized);

    std::fs::copy(&source, &dest).with_context(|| {
        format!(
            \"Failed to copy Nushell binary from '{}' to '{}'\",
            source.display(),
            dest.display()
        )
    })?;
    make_executable(&dest)?;
    std::fs::write(dest_dir.join(\"VERSION\"), normalized.as_bytes())
        .with_context(|| format!(\"Failed to write VERSION file in '{}'\", dest_dir.display()))?;
    Ok(dest)
}"""

text = text.replace(old_body, new_body, 1)

# 2. Update the unit test to assert against version_binary
old_test = """        let installed = install_from_archive(&zip_path, root, \"0.0.0-test\").unwrap();
        assert_eq!(installed, managed_nu_binary(root));
        assert!(installed.is_file());
    }"""
assert text.count(old_test) == 1, f"expected 1 match for test assert, found {text.count(old_test)}"

new_test = """        let installed = install_from_archive(&zip_path, root, \"0.0.0-test\").unwrap();
        // install_from_archive must place the binary in the versioned layout
        // (`tools/nushell/<version>/nu`), not the legacy single-binary
        // `managed_nu_binary` location. Asserting against `version_binary`
        // here means the test would catch a regression that flips us back to
        // clobbering every installed version on every install.
        assert_eq!(
            installed,
            version_manager::version_binary(root, \"0.0.0-test\")
        );
        assert!(installed.is_file());
    }"""

text = text.replace(old_test, new_test, 1)

p.write_text(text, encoding='utf-8')
print("OK: install_from_archive + unit test updated.")
