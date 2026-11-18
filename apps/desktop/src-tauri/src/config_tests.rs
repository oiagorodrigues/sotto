//! Security checklist items 2, 3 and 13 as tests over the shipped configuration files.

use std::collections::BTreeSet;
use std::fs;
use std::path::PathBuf;

use serde_json::Value;

fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn read(path: &str) -> String {
    fs::read_to_string(root().join(path)).unwrap_or_else(|e| panic!("{path}: {e}"))
}

fn json(path: &str) -> Value {
    serde_json::from_str(&read(path)).unwrap_or_else(|e| panic!("{path}: {e}"))
}

fn directives(csp: &str) -> Vec<(String, Vec<String>)> {
    csp.split(';')
        .filter_map(|d| {
            let mut parts = d.split_whitespace();
            let name = parts.next()?.to_string();
            Some((name, parts.map(str::to_string).collect()))
        })
        .collect()
}

fn window_labels() -> Vec<String> {
    json("tauri.conf.json")["app"]["windows"]
        .as_array()
        .expect("windows array")
        .iter()
        .map(|w| w["label"].as_str().expect("label").to_string())
        .collect()
}

#[test]
fn csp_is_strict_and_has_no_remote_origin() {
    let conf = json("tauri.conf.json");
    let csp = conf["app"]["security"]["csp"]
        .as_str()
        .expect("csp is a string");
    let d = directives(csp);
    let get = |name: &str| d.iter().find(|(n, _)| n == name).map(|(_, v)| v.clone());

    assert_eq!(get("default-src"), Some(vec!["'self'".to_string()]));
    for name in ["script-src", "style-src", "font-src"] {
        assert_eq!(get(name), Some(vec!["'self'".to_string()]), "{name}");
    }
    assert!(!csp.contains("unsafe-eval"));
    assert!(!csp.contains("unsafe-inline"));
    assert!(!csp.contains("https:"));
    assert_eq!(
        get("connect-src"),
        Some(vec!["ipc:".to_string(), "http://ipc.localhost".to_string()])
    );
}

#[test]
fn dev_csp_only_adds_the_local_dev_server() {
    let conf = json("tauri.conf.json");
    let dev = conf["app"]["security"]["devCsp"].as_str().expect("devCsp");
    assert!(!dev.contains("unsafe-eval"));
    for (_, sources) in directives(dev) {
        for s in sources {
            if s.contains("://") {
                assert!(
                    s.contains("ipc.localhost") || s.contains("localhost:1420"),
                    "unexpected dev origin {s}"
                );
            }
        }
    }
}

#[test]
fn webview_cannot_reach_remote_content_or_a_global_api() {
    let conf = json("tauri.conf.json");
    assert_eq!(conf["app"]["withGlobalTauri"], Value::Bool(false));
    for w in conf["app"]["windows"].as_array().expect("windows") {
        let url = w["url"].as_str().unwrap_or("");
        assert!(
            !url.contains("://"),
            "window {} loads a remote url",
            w["label"]
        );
    }
    let dist = conf["build"]["frontendDist"]
        .as_str()
        .expect("frontendDist");
    assert!(!dist.contains("://"));
}

#[test]
fn every_window_has_its_own_capability_with_only_the_allowed_permissions() {
    let allowed: BTreeSet<&str> = [
        "core:event:allow-listen",
        "core:event:allow-unlisten",
        "allow-dispatch",
        "allow-current-snapshot",
        "allow-report-notch-size",
    ]
    .into_iter()
    .collect();

    let mut seen = BTreeSet::new();
    for entry in fs::read_dir(root().join("capabilities")).expect("capabilities dir") {
        let path = entry.expect("entry").path();
        if path.extension().and_then(|e| e.to_str()) != Some("json") {
            continue;
        }
        let cap: Value =
            serde_json::from_str(&fs::read_to_string(&path).expect("read")).expect("json");
        let windows: Vec<&str> = cap["windows"]
            .as_array()
            .expect("windows")
            .iter()
            .filter_map(Value::as_str)
            .collect();
        assert_eq!(windows.len(), 1, "{path:?} must target exactly one window");
        assert!(!windows[0].contains('*'), "{path:?} uses a wildcard window");
        assert!(
            cap.get("remote").is_none(),
            "{path:?} grants access to a remote origin"
        );
        assert!(cap.get("webviews").is_none());
        seen.insert(windows[0].to_string());

        for p in cap["permissions"].as_array().expect("permissions") {
            let p = p.as_str().expect("permission is a plain string");
            assert!(allowed.contains(p), "{path:?} grants {p}");
            if p == "allow-report-notch-size" {
                assert_eq!(windows[0], "notch", "only the notch may resize itself");
            }
        }
    }
    let expected: BTreeSet<String> = window_labels().into_iter().collect();
    assert_eq!(seen, expected);
}

#[test]
fn only_three_commands_exist_and_no_powerful_plugin_is_linked() {
    let build: String = read("build.rs").split_whitespace().collect();
    assert!(build.contains(r#"&["dispatch","current_snapshot","report_notch_size",]"#));

    let cargo = read("Cargo.toml");
    for banned in [
        "plugin-shell",
        "plugin-fs",
        "plugin-http",
        "plugin-opener",
        "plugin-process",
    ] {
        assert!(!cargo.contains(banned), "{banned} must not be a dependency");
    }
    let tauri_line = cargo
        .lines()
        .find(|l| l.starts_with("tauri = "))
        .expect("tauri dependency");
    let version = tauri_line
        .split('"')
        .nth(1)
        .expect("version")
        .trim_start_matches('=');
    let parts: Vec<u32> = version.split('.').filter_map(|p| p.parse().ok()).collect();
    assert!(
        parts >= vec![2, 11, 1] && parts[0] == 2,
        "tauri {version} is below 2.11.1"
    );
}

#[test]
fn entitlements_grant_audio_input_only() {
    let plist = read("entitlements.plist");
    let keys: Vec<&str> = plist
        .lines()
        .filter_map(|l| l.trim().strip_prefix("<key>")?.strip_suffix("</key>"))
        .collect();
    assert_eq!(keys, vec!["com.apple.security.device.audio-input"]);
}

#[test]
fn info_plist_explains_every_permission() {
    let plist = read("Info.plist");
    for key in [
        "NSMicrophoneUsageDescription",
        "NSAudioCaptureUsageDescription",
        "NSSpeechRecognitionUsageDescription",
    ] {
        assert!(plist.contains(key), "{key} missing");
    }
}

#[test]
fn info_plist_registers_only_the_sotto_url_scheme() {
    let plist = read("Info.plist");
    let compact: String = plist.split_whitespace().collect();
    assert!(compact.contains("<key>CFBundleURLSchemes</key><array><string>sotto</string></array>"));
    assert_eq!(compact.matches("<key>CFBundleURLSchemes</key>").count(), 1);
}

#[test]
fn bundle_requires_the_macos_version_the_sidecar_needs() {
    let conf = json("tauri.conf.json");
    assert_eq!(
        conf["bundle"]["macOS"]["minimumSystemVersion"].as_str(),
        Some("26.0")
    );
}

#[test]
fn the_notch_is_the_only_overlay_window_and_is_a_transparent_fixed_size_panel() {
    let labels = window_labels();
    assert!(!labels.iter().any(|l| l == "overlay"));
    assert!(!root().join("capabilities/overlay.json").exists());

    let conf = json("tauri.conf.json");
    let notch = conf["app"]["windows"]
        .as_array()
        .expect("windows")
        .iter()
        .find(|w| w["label"] == "notch")
        .expect("notch window");
    assert_eq!(notch["transparent"], Value::Bool(true));
    assert_eq!(notch["decorations"], Value::Bool(false));
    assert_eq!(notch["resizable"], Value::Bool(false));
    assert_eq!(notch["alwaysOnTop"], Value::Bool(true));
}

#[test]
fn only_the_notch_capability_may_resize_or_drag() {
    for entry in fs::read_dir(root().join("capabilities")).expect("capabilities dir") {
        let path = entry.expect("entry").path();
        let text = fs::read_to_string(&path).expect("read");
        assert!(!text.contains("start-dragging"), "{path:?} can drag");
        if !path.ends_with("notch.json") {
            assert!(!text.contains("report-notch-size"), "{path:?} can resize");
        }
    }
}
