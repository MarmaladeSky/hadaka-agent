use std::{
    path::PathBuf,
    process::{Child, Command, Output, Stdio},
    thread,
    time::{Duration, Instant},
};

use tempfile::TempDir;

fn config(extra: &str) -> TempDir {
    let mut source: toml::Table = toml::from_str(include_str!("../agent.example.toml")).unwrap();
    let provider = source.get_mut("providers").unwrap().as_array_mut().unwrap()[0]
        .as_table_mut()
        .unwrap();
    provider.insert("enabled".into(), toml::Value::Boolean(true));
    provider.insert("api_key".into(), toml::Value::String("test-key".into()));
    source.extend(toml::from_str::<toml::Table>(extra).unwrap());
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("agent.toml"), source.to_string()).unwrap();
    let config_dir = dir.path().join(".config").join("hadaka-agent");
    std::fs::create_dir_all(&config_dir).unwrap();
    std::fs::write(config_dir.join("config.toml"), source.to_string()).unwrap();
    dir
}

fn command(dir: &TempDir) -> Command {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_hadaka-agent"));
    cmd.current_dir(dir.path())
        .env("XDG_CONFIG_HOME", dir.path().join(".config"))
        .env_remove("DEEPSEEK_API_KEY")
        .env_remove("HADAKA_API_KEY")
        .env_remove("FIXTURE_COLLISION")
        .env_remove("FIXTURE_HANG_ON_EOF")
        .env_remove("FIXTURE_HANG_ON_START")
        .env_remove("FIXTURE_PID_FILE")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .stdin(Stdio::piped());
    cmd
}

fn wait(mut child: Child) -> Output {
    drop(child.stdin.take());
    let deadline = Instant::now() + Duration::from_secs(15);
    while child.try_wait().unwrap().is_none() {
        if Instant::now() > deadline {
            child.kill().unwrap();
            let output = child.wait_with_output().unwrap();
            panic!(
                "agent timed out: {}",
                String::from_utf8_lossy(&output.stderr)
            );
        }
        thread::sleep(Duration::from_millis(10));
    }
    child.wait_with_output().unwrap()
}

#[test]
fn only_task_execution_is_available() {
    let dir = tempfile::tempdir().unwrap();
    for args in [vec![], vec!["--unknown"], vec!["--format"]] {
        let output = wait(command(&dir).args(args).spawn().unwrap());
        assert_eq!(output.status.code(), Some(2));
        assert!(!dir.path().join(".config").exists());
    }
}

#[test]
fn help_lists_tools_and_permissions() {
    let dir = tempfile::tempdir().unwrap();
    let output = command(&dir).arg("--help").output().unwrap();
    assert!(output.status.success());
    let help = String::from_utf8_lossy(&output.stdout);
    for text in [
        "echo",
        "list_directory",
        "read_file",
        "search_files",
        "text_editor",
        "fetch_url",
        "--allow-net",
        "--allow-read",
        "--allow-write",
        "denied by default",
    ] {
        assert!(help.contains(text), "help is missing {text:?}: {help}");
    }
}

#[test]
fn top_level_help_lists_tools_and_permissions() {
    let dir = tempfile::tempdir().unwrap();
    let output = command(&dir).arg("--help").output().unwrap();
    assert!(output.status.success());
    let help = String::from_utf8_lossy(&output.stdout);
    assert!(help.contains("AVAILABLE TOOLS:"));
    assert!(help.contains("list_directory"));
    assert!(help.contains("--allow-read PATH"));
    assert!(help.contains("--allow-write PATH"));
}

#[cfg(unix)]
#[test]
fn ctrl_c_during_task_startup_reaps_mcp_process() {
    let dir = config(&mcp_config());
    let pid_file = dir.path().join("pid");
    let mut child = command(&dir)
        .env("FIXTURE_PID_FILE", &pid_file)
        .env("FIXTURE_HANG_ON_START", "1")
        .args(["hello"])
        .spawn()
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(10);
    while !pid_file.exists() {
        if Instant::now() > deadline {
            child.kill().unwrap();
            child.wait().unwrap();
            panic!("MCP fixture did not start");
        }
        thread::sleep(Duration::from_millis(10));
    }
    assert!(
        Command::new("kill")
            .args(["-INT", &child.id().to_string()])
            .status()
            .unwrap()
            .success()
    );
    assert_eq!(wait(child).status.code(), Some(130));
    #[cfg(target_os = "linux")]
    assert!(
        !PathBuf::from(format!(
            "/proc/{}",
            std::fs::read_to_string(pid_file).unwrap()
        ))
        .exists()
    );
}

#[test]
fn missing_default_config_is_created() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join(".config/hadaka-agent/config.toml");
    let output = wait(
        command(&dir)
            .args(["hello"])
            .env_remove("DEEPSEEK_API_KEY")
            .spawn()
            .unwrap(),
    );
    assert_eq!(output.status.code(), Some(1));
    let error = String::from_utf8_lossy(&output.stderr);
    assert!(
        error.contains("A provider needs to be configured first"),
        "{error}"
    );
    let created = std::fs::read_to_string(path).unwrap();
    let config: toml::Table = toml::from_str(&created).unwrap();
    let providers = config["providers"].as_array().unwrap();
    assert_eq!(providers.len(), 1);
    assert_eq!(providers[0]["provider_name"].as_str(), Some("deepseek"));
    assert_eq!(providers[0]["enabled"].as_bool(), Some(false));
    assert_eq!(
        providers[0]["api_key"].as_str(),
        Some("invalid-placeholder-key")
    );
    assert!(output.stdout.is_empty());
}

#[test]
fn missing_explicit_config_is_not_created() {
    let dir = tempfile::tempdir().unwrap();
    let output = wait(
        command(&dir)
            .args(["hello", "--config", "missing.toml"])
            .spawn()
            .unwrap(),
    );
    assert_eq!(output.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&output.stderr).contains("cannot read config"));
    assert!(!dir.path().join("missing.toml").exists());
    assert!(!dir.path().join(".config").exists());
}

#[test]
fn empty_config_requires_provider_setup() {
    let dir = config("");
    let default_path = dir.path().join(".config/hadaka-agent/config.toml");
    let explicit_path = dir.path().join("agent.toml");
    for source in [
        "",
        " \n\t",
        "# Provider setup is pending.\n",
        "providers = []",
        include_str!("../agent.example.toml"),
    ] {
        std::fs::write(&default_path, source).unwrap();
        std::fs::write(&explicit_path, source).unwrap();
        for explicit in [false, true] {
            for key in [None, Some("test-key")] {
                let mut cmd = command(&dir);
                cmd.args(["hello"]).env_remove("DEEPSEEK_API_KEY");
                if explicit {
                    cmd.arg("--config").arg(&explicit_path);
                }
                if let Some(key) = key {
                    cmd.env("DEEPSEEK_API_KEY", key);
                }
                let output = wait(cmd.spawn().unwrap());
                assert_eq!(output.status.code(), Some(1));
                let error = String::from_utf8_lossy(&output.stderr);
                assert!(
                    error.contains("A provider needs to be configured first"),
                    "{error}"
                );
                assert!(output.stdout.is_empty());
            }
        }
    }
}

#[test]
fn disabled_provider_does_not_start_mcp_servers() {
    let dir = config(&format!(
        "{}\n[[providers]]\nprovider_name = 'deepseek'\nenabled = false\nmodel = 'deepseek-flash'\napi_key = 'invalid-key'\n",
        mcp_config()
    ));
    let pid_file = dir.path().join("pid");
    let output = wait(
        command(&dir)
            .args(["hello"])
            .env("FIXTURE_PID_FILE", &pid_file)
            .spawn()
            .unwrap(),
    );
    assert_eq!(output.status.code(), Some(1));
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("A provider needs to be configured first")
    );
    assert!(!pid_file.exists());
}

#[test]
fn invalid_config_is_not_treated_as_empty() {
    let dir = config("");
    for source in ["model =", "model = 'deepseek-flash'", "unknown = true"] {
        std::fs::write(dir.path().join(".config/hadaka-agent/config.toml"), source).unwrap();
        let output = wait(command(&dir).args(["hello"]).spawn().unwrap());
        assert_eq!(output.status.code(), Some(1));
        let error = String::from_utf8_lossy(&output.stderr);
        assert!(error.contains("invalid agent configuration"), "{error}");
        assert_eq!(
            std::fs::read_to_string(dir.path().join(".config/hadaka-agent/config.toml")).unwrap(),
            source
        );
        assert!(
            !error.contains("A provider needs to be configured first"),
            "{error}"
        );
    }
}

#[test]
fn requires_a_nonblank_config_key_even_when_environment_keys_are_set() {
    let dir = config("");
    for key in [None, Some(""), Some(" \t ")] {
        let mut source: toml::Table =
            toml::from_str(include_str!("../agent.example.toml")).unwrap();
        let provider = source.get_mut("providers").unwrap().as_array_mut().unwrap()[0]
            .as_table_mut()
            .unwrap();
        provider.insert("enabled".into(), toml::Value::Boolean(true));
        provider.remove("api_key");
        if let Some(key) = key {
            provider.insert("api_key".into(), toml::Value::String(key.into()));
        }
        std::fs::write(
            dir.path().join(".config/hadaka-agent/config.toml"),
            source.to_string(),
        )
        .unwrap();
        let mut cmd = command(&dir);
        cmd.args(["hello"])
            .env("DEEPSEEK_API_KEY", "environment-key")
            .env("HADAKA_API_KEY", "legacy-key");
        let output = wait(cmd.spawn().unwrap());
        assert_eq!(output.status.code(), Some(1));
        let error = String::from_utf8_lossy(&output.stderr);
        assert!(error.contains("api_key"), "{error}");
        assert!(!error.contains("environment-key"));
        assert!(!error.contains("legacy-key"));
        assert!(output.stdout.is_empty());
    }
}

#[test]
fn rejects_generic_provider_configuration() {
    for source in ["base_url = 'http://localhost/v1'", "provider = 'other'"] {
        let dir = config(source);
        let output = wait(command(&dir).args(["hello"]).spawn().unwrap());
        assert_eq!(output.status.code(), Some(1));
        assert!(String::from_utf8_lossy(&output.stderr).contains("unknown field"));
    }
}

fn mcp_config() -> String {
    let fixture = std::env::current_exe()
        .unwrap()
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .join("examples")
        .join(format!("mcp_fixture{}", std::env::consts::EXE_SUFFIX));
    assert!(
        fixture.exists(),
        "run cargo build --example mcp_fixture first"
    );
    format!(
        "[[mcp_servers]]\nname = 'fixture'\ncommand = {:?}\nargs = []\nenv = {{}}\n",
        fixture.to_str().unwrap()
    )
}

#[test]
fn mcp_naming_collisions_fail_startup_and_clean_up() {
    let dir = config(&mcp_config());
    let pid_file = dir.path().join("pid");
    let output = wait(
        command(&dir)
            .env("FIXTURE_PID_FILE", &pid_file)
            .env("FIXTURE_COLLISION", "1")
            .args(["task"])
            .spawn()
            .unwrap(),
    );
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("name collision"));
    #[cfg(target_os = "linux")]
    assert!(
        !PathBuf::from(format!(
            "/proc/{}",
            std::fs::read_to_string(pid_file).unwrap()
        ))
        .exists()
    );
}
