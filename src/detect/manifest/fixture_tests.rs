//! fixture-based agent state regression tests（Issue #1）。
//!
//! `tests/fixtures/agent-state/*.toml` を走査し、各 fixture の期待 state と
//! 検知結果を突き合わせる。エンジン意味論（priority、all/any/not、
//! skip、source precedence、reload）の単体検証は `tests.rs` が担い、
//! こちらは実機由来 snapshot の回帰のみを担当する。
//!
//! fixture 追加時の約束:
//! - `herdr agent read <pane> --source detection --format text` で取得した
//!   bottom-buffer の切り出しを使う（装飾が関与する場合のみ `--format ansi`）。
//!   想像で合成した画面は追加しない。
//! - `evidence` に根拠（元テスト・issue・取得条件）を必ず残す。
//! - `blocked` は `expected_rule` まで厳密一致、`working` / `idle` は原則
//!   state 主体で検証し、ルール改善時の脆さを抑える。
//! - `expected_visible_*` は未指定時 `false` として検証される。state だけを
//!   見て visible signal を無視する fixture にはしない。`blocked` / `working` /
//!   `idle` の誤判定検出力を保つための意図的な契約である。

use super::{explain_with_input, DetectionInput};

#[derive(Debug, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct AgentStateFixture {
    agent: String,
    description: String,
    evidence: String,
    #[serde(default)]
    screen: String,
    #[serde(default)]
    osc_title: String,
    #[serde(default)]
    osc_progress: String,
    expected_state: String,
    expected_rule: Option<String>,
    #[serde(default)]
    expected_visible_blocker: bool,
    #[serde(default)]
    expected_visible_working: bool,
    #[serde(default)]
    expected_visible_idle: bool,
    expected_fallback_reason: Option<String>,
}

fn fixture_dir() -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/agent-state")
}

fn parse_expected_state(name: &str, file: &str) -> Result<super::super::AgentState, String> {
    match name {
        "idle" => Ok(super::super::AgentState::Idle),
        "working" => Ok(super::super::AgentState::Working),
        "blocked" => Ok(super::super::AgentState::Blocked),
        "unknown" => Ok(super::super::AgentState::Unknown),
        _ => Err(format!("fixture {file}: 不正な expected_state {name:?}")),
    }
}

fn check_fixture(path: &std::path::Path) -> Result<(), String> {
    let file = path.display().to_string();
    let content =
        std::fs::read_to_string(path).map_err(|err| format!("fixture {file} を読めない: {err}"))?;
    let fixture: AgentStateFixture =
        toml::from_str(&content).map_err(|err| format!("fixture {file} を解釈できない: {err}"))?;

    let agent = super::super::parse_agent_label(&fixture.agent)
        .ok_or_else(|| format!("fixture {file}: 不正な agent {:?}", fixture.agent))?;
    let result = explain_with_input(
        agent,
        DetectionInput {
            screen: &fixture.screen,
            osc_title: &fixture.osc_title,
            osc_progress: &fixture.osc_progress,
        },
    );

    let mut problems = Vec::new();
    let expected_state = parse_expected_state(&fixture.expected_state, &file)?;
    // blocked は matched rule まで厳密一致させる契約のため、expected_rule の
    // 未指定は fixture 定義エラーとして fail させる（working / idle は任意）。
    if expected_state == super::super::AgentState::Blocked && fixture.expected_rule.is_none() {
        return Err(format!(
            "fixture {file}: expected_state が blocked の場合は expected_rule が必須"
        ));
    }
    if result.state != expected_state {
        problems.push(format!(
            "state が不一致: 期待 {} 実際 {}",
            super::agent_state_label(expected_state),
            super::agent_state_label(result.state),
        ));
    }
    if let Some(expected_rule) = fixture.expected_rule.as_deref() {
        let actual = result
            .matched_rule
            .as_ref()
            .map(|rule| rule.id.as_str())
            .unwrap_or("(none)");
        if actual != expected_rule {
            problems.push(format!(
                "matched rule が不一致: 期待 {expected_rule} 実際 {actual}"
            ));
        }
    }
    for (name, expected, actual) in [
        (
            "visible_blocker",
            fixture.expected_visible_blocker,
            result.visible_blocker,
        ),
        (
            "visible_working",
            fixture.expected_visible_working,
            result.visible_working,
        ),
        (
            "visible_idle",
            fixture.expected_visible_idle,
            result.visible_idle,
        ),
    ] {
        if expected != actual {
            problems.push(format!("{name} が不一致: 期待 {expected} 実際 {actual}"));
        }
    }
    if let Some(expected_fallback) = fixture.expected_fallback_reason.as_deref() {
        if result.fallback_reason.as_deref() != Some(expected_fallback) {
            problems.push(format!(
                "fallback_reason が不一致: 期待 {expected_fallback} 実際 {:?}",
                result.fallback_reason
            ));
        }
    }

    if problems.is_empty() {
        return Ok(());
    }
    Err(format!(
        "fixture {file} ({})\n  evidence: {}\n  matched_rule: {:?}\n  fallback_reason: {:?}\n  {}",
        fixture.description,
        fixture.evidence,
        result.matched_rule,
        result.fallback_reason,
        problems.join("\n  ")
    ))
}

#[test]
fn agent_state_fixtures() {
    // with_manifest_dirs 系テストと manifest cache を競合させないため共有 lock を取る。
    // 一時 XDG 配下で reload し、bundled manifest だけを見て評価する。
    let _guard = crate::config::test_config_env_lock().lock().unwrap();
    let old_config = std::env::var_os("XDG_CONFIG_HOME");
    let old_state = std::env::var_os("XDG_STATE_HOME");
    let base =
        std::env::temp_dir().join(format!("herdr-agent-state-fixtures-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    std::env::set_var("XDG_CONFIG_HOME", base.join("config"));
    std::env::set_var("XDG_STATE_HOME", base.join("state"));
    super::reload_manifests();

    let mut files: Vec<_> = std::fs::read_dir(fixture_dir())
        .unwrap_or_else(|err| panic!("fixture dir を読めない: {}: {err}", fixture_dir().display()))
        .map(|entry| entry.expect("fixture entry を読めない").path())
        .filter(|path| path.extension().is_some_and(|ext| ext == "toml"))
        .collect();
    files.sort();
    assert!(
        !files.is_empty(),
        "fixture が0件: {}",
        fixture_dir().display()
    );

    let mut failures = Vec::new();
    for path in &files {
        if let Err(report) = check_fixture(path) {
            failures.push(report);
        }
    }

    match old_config {
        Some(value) => std::env::set_var("XDG_CONFIG_HOME", value),
        None => std::env::remove_var("XDG_CONFIG_HOME"),
    }
    match old_state {
        Some(value) => std::env::set_var("XDG_STATE_HOME", value),
        None => std::env::remove_var("XDG_STATE_HOME"),
    }
    super::reload_manifests();
    let _ = std::fs::remove_dir_all(&base);

    assert!(
        failures.is_empty(),
        "{} 件中 {} 件が不一致:\n\n{}",
        files.len(),
        failures.len(),
        failures.join("\n\n")
    );
}
