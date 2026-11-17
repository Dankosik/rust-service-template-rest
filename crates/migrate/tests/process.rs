#[test]
#[expect(
    clippy::disallowed_methods,
    clippy::disallowed_types,
    reason = "the process test captures the shipped binary's startup refusal"
)]
fn postgres_disabled_uses_the_configured_format_for_one_terminal_record() -> std::io::Result<()> {
    let output = std::process::Command::new(env!("CARGO_BIN_EXE_migrate"))
        .env_clear()
        .env("APP__POSTGRES__ENABLED", "false")
        .env("APP__LOG__LEVEL", "off")
        .env("APP__LOG__FORMAT", "text")
        .output()?;
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert_eq!(output.status.code(), Some(1), "{stdout}");
    assert!(output.stderr.is_empty(), "{:?}", output.stderr);
    assert_eq!(stdout.lines().count(), 1, "{stdout}");
    for field in [
        "migration_run",
        "\"outcome\"=\"error\"",
        "\"stage\"=\"config\"",
        "postgres.enabled must be true to run migrations",
    ] {
        assert!(stdout.contains(field), "missing {field}: {stdout}");
    }
    Ok(())
}
