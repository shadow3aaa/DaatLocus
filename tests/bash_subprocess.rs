//! Exercise our process-only Rubash host, including reexec after relocation.
use std::{path::Path, process::Output, time::Duration};
use tokio::{io::AsyncWriteExt, process::Command};

async fn run(program: &Path, args: &[&str], input: &[u8]) -> Output {
    let directory = tempfile::tempdir().unwrap();
    let profile = directory.path().join("must-not-create-profile");
    let startup = directory.path().join("startup.sh");
    std::fs::write(&startup, "printf unexpected-startup-output").unwrap();
    let mut child = Command::new(program)
        .arg("--internal-bash")
        .args(args)
        .env("BASH_ENV", &startup)
        .env("DAAT_LOCUS_HOME", &profile)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .unwrap();
    child.stdin.take().unwrap().write_all(input).await.unwrap();
    let output = tokio::time::timeout(Duration::from_secs(20), child.wait_with_output())
        .await
        .expect("shell invocation timed out")
        .unwrap();
    assert!(
        !profile.exists(),
        "shell entry initialized an application profile"
    );
    output
}

#[tokio::test]
async fn relocated_service_keeps_background_coproc_and_pipeline_execution_inside_rubash() {
    let directory = tempfile::tempdir().unwrap();
    let binary = directory
        .path()
        .join(format!("daat-locus{}", std::env::consts::EXE_SUFFIX));
    std::fs::copy(env!("CARGO_BIN_EXE_daat-locus"), &binary).unwrap();
    let output = run(
        &binary,
        &[
            "-c",
            r#"
        f() { printf '%s\n' "worker:$1"; }
        f background &
        wait $!
        coproc { printf 'coprocess\n'; }
        read -r value <&"${COPROC[0]}"
        printf '%s\n' "$value"
        printf 'one\ntwo\nthree\n' | head -n 2 | wc -l
        printf '%s\n' '中文输出'
        "$BASH" -c 'printf "nested-rubash\n"'
        env -i "$BASH" -c 'printf "cleared-environment\n"'
    "#,
        ],
        b"",
    )
    .await;
    let text = String::from_utf8(output.stdout).unwrap();
    assert!(
        output.status.success(),
        "{text}\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        text.lines().collect::<Vec<_>>(),
        [
            "worker:background",
            "coprocess",
            "2",
            "中文输出",
            "nested-rubash",
            "cleared-environment"
        ]
    );
}

#[tokio::test]
async fn stdin_script_and_exit_trap_preserve_input_stream_and_exit_status() {
    let binary = Path::new(env!("CARGO_BIN_EXE_daat-locus"));
    let output = run(
        binary,
        &["-s"],
        b"trap 'printf cleanup' EXIT\nprintf '%s' streamed\nexit 7\n",
    )
    .await;
    assert_eq!(output.status.code(), Some(7));
    assert_eq!(output.stdout, b"streamedcleanup");
}
