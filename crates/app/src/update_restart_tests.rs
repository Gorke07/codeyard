use super::*;

fn scratch(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("codeyard-restart-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn at(secs: u64) -> SystemTime {
    UNIX_EPOCH + Duration::from_secs(secs)
}

fn handover() -> Handover {
    Handover {
        to: "0.2.0".into(),
        windows: vec![
            Folder { host: None, path: "/Users/a/My \"Project\"; rm -rf $HOME".into() },
            Folder { host: Some("build-box".into()), path: "/srv/app".into() },
            Folder { host: None, path: "/tmp/ü ı".into() },
        ],
        focused: 1,
    }
}

fn raw(dir: &Path, text: &str) {
    std::fs::write(file(dir), text).unwrap();
}

#[test]
fn update_restart_hands_the_windows_over_exactly_and_only_once() {
    let dir = scratch("roundtrip");
    write(&dir, &handover(), at(1000)).unwrap();
    assert_eq!(take(&dir, "0.2.0", at(1005)).unwrap(), Some(handover()));
    assert!(!file(&dir).exists(), "consumed");
    assert_eq!(take(&dir, "0.2.0", at(1006)).unwrap(), None, "a second start finds nothing");
}

#[test]
fn update_restart_without_a_file_is_not_an_error() {
    assert_eq!(take(&scratch("none"), "0.2.0", at(1)).unwrap(), None);
}

#[test]
fn update_restart_write_replaces_an_earlier_file_and_leaves_no_temp_file() {
    let dir = scratch("replace");
    write(&dir, &Handover { windows: vec![Folder { host: None, path: "/old".into() }], focused: 0, ..handover() }, at(1)).unwrap();
    write(&dir, &handover(), at(2)).unwrap();
    assert_eq!(take(&dir, "0.2.0", at(3)).unwrap(), Some(handover()));
    assert_eq!(std::fs::read_dir(&dir).unwrap().count(), 0, "the atomic write's temp file is gone");
}

#[test]
fn update_restart_ignores_a_file_older_than_ten_minutes_or_from_the_future_and_removes_it() {
    for (now, why) in [(1000 + 601, "stale"), (999, "from the future")] {
        let dir = scratch("age");
        write(&dir, &handover(), at(1000)).unwrap();
        assert!(matches!(take(&dir, "0.2.0", at(now)), Err(HandoverError::Stale)), "{why}");
        assert!(!file(&dir).exists(), "{why}: removed all the same");
    }
    let dir = scratch("age-edge");
    write(&dir, &handover(), at(1000)).unwrap();
    assert!(take(&dir, "0.2.0", at(1600)).unwrap().is_some(), "ten minutes exactly is still fresh");
}

#[test]
fn update_restart_ignores_a_file_written_for_another_version() {
    let dir = scratch("version");
    write(&dir, &handover(), at(1000)).unwrap();
    let err = take(&dir, "0.3.0", at(1001)).unwrap_err();
    assert!(matches!(&err, HandoverError::WrongVersion { found, running } if found == "0.2.0" && running == "0.3.0"), "{err}");
    assert!(!file(&dir).exists());
}

#[test]
fn update_restart_treats_every_malformed_file_as_corrupt_and_removes_it() {
    let ok = |extra: &str| format!(r#"{{"schema":1,"to":"0.2.0","written_ms":1000000,{extra}}}"#);
    let cases = [
        "".to_string(),
        "not json".to_string(),
        "[]".to_string(),
        r#"{"schema":2,"to":"0.2.0","written_ms":1000000,"windows":[{"host":null,"path":"/a"}]}"#.to_string(),
        r#"{"to":"0.2.0","written_ms":1000000,"windows":[{"host":null,"path":"/a"}]}"#.to_string(),
        r#"{"schema":1,"written_ms":1000000,"windows":[{"host":null,"path":"/a"}]}"#.to_string(),
        r#"{"schema":1,"to":"0.2.0","windows":[{"host":null,"path":"/a"}]}"#.to_string(),
        ok(r#""windows":[]"#),
        ok(r#""windows":"/a""#),
        ok(r#""windows":[{"host":null,"path":""}]"#),
        ok(r#""windows":[{"host":null}]"#),
        ok(r#""windows":[{"host":null,"path":"/a\u0000b"}]"#),
        ok(r#""windows":[{"host":7,"path":"/a"}]"#),
        ok(r#""windows":[{"host":"","path":"/a"}]"#),
        ok(r#""windows":[{"host":"-oProxyCommand=x","path":"/a"}]"#),
        ok(r#""windows":[{"host":null,"path":"/a"},{"host":null,"path":5}]"#),
    ];
    for (i, text) in cases.iter().enumerate() {
        let dir = scratch("corrupt");
        raw(&dir, text);
        let r = take(&dir, "0.2.0", at(1000));
        assert!(matches!(r, Err(HandoverError::Corrupt(_))), "case {i}: {text}: {r:?}");
        assert!(!file(&dir).exists(), "case {i}: removed");
    }
}

#[test]
fn update_restart_reads_what_the_window_list_allows_and_falls_back_on_the_focus() {
    let dir = scratch("lenient");
    raw(&dir, r#"{"schema":1,"to":"0.2.0","written_ms":1000000,"focused":9,"windows":[{"host":null,"path":"/a"},{"host":"h","path":"/b"}],"extra":true}"#);
    let h = take(&dir, "0.2.0", at(1000)).unwrap().unwrap();
    assert_eq!((h.windows.len(), h.focused), (2, 0), "a focus outside the list is the first window");

    let many: Vec<String> = (0..40).map(|i| format!(r#"{{"host":null,"path":"/w{i}"}}"#)).collect();
    raw(&dir, &format!(r#"{{"schema":1,"to":"0.2.0","written_ms":1000000,"focused":39,"windows":[{}]}}"#, many.join(",")));
    let h = take(&dir, "0.2.0", at(1000)).unwrap().unwrap();
    assert_eq!((h.windows.len(), h.focused), (MAX_WINDOWS, 0), "a runaway list is cut and the focus follows");
}

#[test]
fn update_restart_discard_removes_the_file_and_tolerates_none() {
    let dir = scratch("discard");
    discard(&dir);
    write(&dir, &handover(), at(1)).unwrap();
    discard(&dir);
    assert!(!file(&dir).exists());
}

#[test]
fn update_restart_unreadable_file_is_an_io_error_not_a_panic() {
    let dir = scratch("io");
    std::fs::create_dir(file(&dir)).unwrap(); // a directory where the file should be
    assert!(matches!(take(&dir, "0.2.0", at(1)), Err(HandoverError::Io(_))));
}
