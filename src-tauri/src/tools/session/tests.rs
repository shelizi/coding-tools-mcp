use super::*;

#[tokio::test]
async fn active_session_admission_is_bounded_and_recoverable() {
    const TEST_LIMIT: usize = 4;
    let store = SessionStore::with_active_session_limit(TEST_LIMIT);
    let mut permits = Vec::new();
    for _ in 0..TEST_LIMIT {
        permits.push(
            store
                .acquire_active_slot()
                .await
                .expect("active session permit"),
        );
    }
    assert_eq!(store.active_slots_available(), 0);

    let started = Instant::now();
    let error = store
        .acquire_active_slot()
        .await
        .expect_err("session limit should reject overload");
    assert!(started.elapsed() >= Duration::from_millis(900));
    assert!(matches!(
        error,
        WorkspaceError::ToolDetails {
            code: "SESSION_LIMIT_REACHED",
            ..
        }
    ));

    permits.pop();
    let recovered = store
        .acquire_active_slot()
        .await
        .expect("capacity should recover after permit release");
    assert_eq!(store.active_slots_available(), 0);
    drop(recovered);
    drop(permits);
    assert_eq!(store.active_slots_available(), TEST_LIMIT);
}

#[tokio::test]
#[serial_test::serial(process_runtime)]
async fn finalizing_a_session_releases_its_active_slot() {
    let store = SessionStore::new();
    let permit = store
        .acquire_active_slot()
        .await
        .expect("active session permit");

    #[cfg(windows)]
    let child = tokio::process::Command::new("cmd")
        .args(["/d", "/c", "exit", "0"])
        .spawn()
        .expect("spawn test child");
    #[cfg(unix)]
    let child = tokio::process::Command::new("sh")
        .args(["-c", "exit 0"])
        .spawn()
        .expect("spawn test child");

    let session = store.insert(ExecSession::new(child).with_active_slot(permit));
    assert_eq!(
        store.active_slots_available(),
        DEFAULT_ACTIVE_SESSION_LIMIT - 1
    );
    session.kill_and_wait().await;
    session.mark_finalized();
    assert_eq!(store.active_slots_available(), DEFAULT_ACTIVE_SESSION_LIMIT);
    assert!(session.finalized_at().is_some());
}

#[tokio::test]
#[serial_test::serial(process_runtime)]
async fn backend_kill_hook_is_available_while_exit_waiter_owns_the_child() {
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[cfg(windows)]
    let child = tokio::process::Command::new("cmd")
        .args(["/d", "/c", "ping 127.0.0.1 -n 2 >nul"])
        .spawn()
        .expect("spawn test child");
    #[cfg(unix)]
    let child = tokio::process::Command::new("sh")
        .args(["-c", "sleep 0.2"])
        .spawn()
        .expect("spawn test child");

    let calls = Arc::new(AtomicUsize::new(0));
    let hook_calls = Arc::clone(&calls);
    let child = ProcessChild::from_tokio(child).with_kill_hook(Arc::new(move || {
        hook_calls.fetch_add(1, Ordering::AcqRel);
        Ok(())
    }));
    let session = Arc::new(ExecSession::new_with_mode_and_checks(child, false, false));
    session.spawn_exit_waiter();
    session.kill_and_wait().await;

    assert_eq!(calls.load(Ordering::Acquire), 1);
    assert!(session.has_exited());
}

#[tokio::test]
#[serial_test::serial(process_runtime)]
async fn event_waiter_observes_exit_and_uses_output_uri_refs() {
    #[cfg(windows)]
    let child = tokio::process::Command::new("cmd")
        .args(["/d", "/c", "exit", "0"])
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .expect("spawn test child");
    #[cfg(unix)]
    let child = tokio::process::Command::new("sh")
        .args(["-c", "exit 0"])
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .expect("spawn test child");

    let session = Arc::new(ExecSession::new(child));
    session.spawn_readers().await;
    session.spawn_exit_waiter();
    tokio::time::timeout(Duration::from_secs(5), session.wait_until_exited())
        .await
        .expect("event waiter timeout");
    assert!(session.has_exited());

    let snapshot = session.snapshot_with_options(OutputOptions::tail(4096));
    assert!(snapshot["output_refs"]["stdout"]
        .as_str()
        .expect("stdout ref")
        .starts_with("output://"));
    assert!(snapshot["output_refs"]["stderr"]
        .as_str()
        .expect("stderr ref")
        .ends_with("/stderr"));
}

#[tokio::test]
#[serial_test::serial(process_runtime)]
async fn sensitive_sessions_redact_snapshots_and_retained_output() {
    #[cfg(windows)]
    let child = tokio::process::Command::new("cmd")
        .args(["/d", "/c", "echo bare-secret-value"])
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .expect("spawn test child");
    #[cfg(unix)]
    let child = tokio::process::Command::new("sh")
        .args(["-c", "printf bare-secret-value"])
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .expect("spawn test child");

    let session = Arc::new(ExecSession::new(child).with_sensitive_output(true));
    session.spawn_readers().await;
    session.spawn_exit_waiter();
    tokio::time::timeout(Duration::from_secs(5), session.wait_until_exited())
        .await
        .expect("sensitive session timeout");
    session.wait_for_readers().await;

    let snapshot = session.snapshot(4096);
    assert_eq!(snapshot["stdout"], "[REDACTED]", "{snapshot}");
    assert_eq!(snapshot["sensitive_data_redacted"], true, "{snapshot}");
    assert!(!snapshot.to_string().contains("bare-secret-value"));

    let (retained, _) = session.retained_stream_bytes("stdout");
    assert_eq!(retained, b"[REDACTED]");
}

#[test]
fn process_output_decoder_preserves_utf8() {
    assert_eq!(
        decode_process_output("WSL UTF-8 測試 ✓".as_bytes()),
        "WSL UTF-8 測試 ✓"
    );
}

#[test]
fn process_output_decoder_handles_utf16le_with_and_without_bom() {
    let text = "預設發行版本: Ubuntu-24.04\r\n";
    let encoded = text
        .encode_utf16()
        .flat_map(u16::to_le_bytes)
        .collect::<Vec<_>>();
    let mut with_bom = vec![0xFF, 0xFE];
    with_bom.extend_from_slice(&encoded);

    assert_eq!(decode_process_output(&with_bom), text);
    assert_eq!(decode_process_output(&encoded), text);
    assert_eq!(
        truncate_tail(&with_bom, 4096, ProcessOutputEncoding::Utf16Le).content,
        text
    );
    assert_eq!(
        summarize_stream(&with_bom, 4096, 10, ProcessOutputEncoding::Utf16Le,).content,
        text.trim_end()
    );
}

#[test]
fn process_output_decoder_reconstructs_split_utf8_character() {
    let text = "前綴✓後綴";
    let bytes = text.as_bytes();
    let split = bytes
        .windows(3)
        .position(|window| window == "✓".as_bytes())
        .expect("check mark bytes")
        + 1;
    let prefix = process_output_prefix(&bytes[..split], ProcessOutputEncoding::Unknown, 0);
    let mut second = prefix;
    second.extend_from_slice(&bytes[split..]);

    assert_eq!(
        decode_process_output_with_encoding(&second, ProcessOutputEncoding::Unknown),
        "✓後綴"
    );
    assert_eq!(
        complete_output_boundary(&bytes[..split], ProcessOutputEncoding::Unknown),
        "前綴".len()
    );
}

#[test]
fn process_output_decoder_reconstructs_split_utf16_surrogate_pair() {
    let text = "A😀B";
    let encoded = text
        .encode_utf16()
        .flat_map(u16::to_le_bytes)
        .collect::<Vec<_>>();
    let split = 4;
    let prefix = process_output_prefix(&encoded[..split], ProcessOutputEncoding::Utf16Le, 0);
    let mut second = prefix;
    second.extend_from_slice(&encoded[split..]);

    assert_eq!(
        decode_process_output_with_encoding(&second, ProcessOutputEncoding::Utf16Le),
        "😀B"
    );
    assert_eq!(
        complete_output_boundary(&encoded[..5], ProcessOutputEncoding::Utf16Le),
        2
    );
}

#[test]
fn process_output_events_defer_partial_characters() {
    let bytes = "✓".as_bytes();
    let first = OutputEvent {
        sequence: 1,
        stream: "stdout",
        stream_offset: 0,
        prefix: Vec::new(),
        data: bytes[..1].to_vec(),
    };
    let second = OutputEvent {
        sequence: 2,
        stream: "stdout",
        stream_offset: 1,
        prefix: process_output_prefix(&bytes[..1], ProcessOutputEncoding::Unknown, 0),
        data: bytes[1..].to_vec(),
    };

    assert_eq!(
        decode_output_event(&first, ProcessOutputEncoding::Unknown),
        ""
    );
    assert_eq!(
        truncate_tail(&bytes[..1], 4096, ProcessOutputEncoding::Unknown).content,
        ""
    );
    assert_eq!(
        summarize_stream(&bytes[..1], 4096, 10, ProcessOutputEncoding::Unknown,).content,
        ""
    );
    assert_eq!(
        decode_output_event(&second, ProcessOutputEncoding::Unknown),
        "✓"
    );
}

#[test]
fn process_output_stream_recovers_split_utf16_bom() {
    let mut stream = ProcessOutputStream::default();
    let (_, first_prefix) = stream.append(&[0xFF]);
    let (second_offset, second_prefix) = stream.append(&[0xFE, b'A', 0]);
    let second = OutputEvent {
        sequence: 2,
        stream: "stdout",
        stream_offset: second_offset,
        prefix: second_prefix,
        data: vec![0xFE, b'A', 0],
    };

    assert!(first_prefix.is_empty());
    assert_eq!(stream.encoding, ProcessOutputEncoding::Utf16Le);
    assert_eq!(decode_output_event(&second, stream.encoding), "A");
}

#[test]
fn retained_output_trimming_preserves_character_boundaries() {
    let mut utf8 = "A✓B".as_bytes().to_vec();
    let utf8_total = utf8.len();
    trim_process_buffer(&mut utf8, 4, ProcessOutputEncoding::Unknown, utf8_total);
    assert_eq!(decode_process_output(&utf8), "✓B");

    let mut utf16 = "A😀B"
        .encode_utf16()
        .flat_map(u16::to_le_bytes)
        .collect::<Vec<_>>();
    let utf16_total = utf16.len();
    trim_process_buffer(&mut utf16, 4, ProcessOutputEncoding::Utf16Le, utf16_total);
    assert_eq!(
        decode_process_output_with_encoding(&utf16, ProcessOutputEncoding::Utf16Le),
        "B"
    );
}

#[test]
fn output_pagination_advances_past_small_multibyte_limits() {
    let utf8 = "✓B".as_bytes();
    assert_eq!(
        bounded_output_end(utf8, 0, 1, ProcessOutputEncoding::Unknown),
        "✓".len()
    );
    assert_eq!(
        align_output_start(utf8, 1, ProcessOutputEncoding::Unknown, 0),
        0
    );

    let utf16 = "😀B"
        .encode_utf16()
        .flat_map(u16::to_le_bytes)
        .collect::<Vec<_>>();
    assert_eq!(
        bounded_output_end(&utf16, 0, 2, ProcessOutputEncoding::Utf16Le),
        4
    );
    assert_eq!(
        align_output_start(&utf16, 2, ProcessOutputEncoding::Utf16Le, 0),
        0
    );
}

#[test]
fn output_ref_is_rejected_as_a_session_id_with_a_correction() {
    let error = control::required_session_id(&json!({
        "session_id": "output://abc-123/stdout"
    }))
    .expect_err("output ref must not be accepted as session id");
    let value = error.to_error_value();
    assert_eq!(value["code"], "OUTPUT_REF_USED_AS_SESSION_ID");
    assert_eq!(value["details"]["corrected_session_id"], "abc-123");
}
