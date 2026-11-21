//! サービスマネージャの設定ファイル(TOML の小さな部分集合)を読む。
//!
//! ```toml
//! [service.名前]
//! exec = "/bin/プログラム 引数"
//! trigger = "startup"            # 起動時に 1 回
//! trigger = { every = "50ms" }   # 一定間隔で。単位は ms か s
//! trigger = { gpio = 3 }         # GPIO の割り込みのたびに
//! restart = "on-failure"         # 0 以外で終わったら起動し直す
//! max_runs = 3                   # 動かす回数の上限(周期と再起動の両方に効く)
//! ```

/// サービスを動かすきっかけ
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Trigger {
    Startup,
    /// ミリ秒ごと
    Every(u64),
    /// GPIO の線の番号
    Gpio(u32),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Service {
    pub name: String,
    /// プログラムと引数
    pub exec: Vec<String>,
    pub trigger: Trigger,
    pub restart_on_failure: bool,
    /// 0 なら無制限
    pub max_runs: u32,
}

/// 設定ファイル全体を読む。間違いがあれば「何行目の何が悪いか」を返す。
pub fn parse(text: &str) -> Result<Vec<Service>, String> {
    let mut services: Vec<Service> = Vec::new();
    for (i, raw) in text.lines().enumerate() {
        let at = |msg: &str| format!("line {}: {msg}: {}", i + 1, raw.trim());
        let line = raw.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if let Some(name) = line
            .strip_prefix("[service.")
            .and_then(|r| r.strip_suffix(']'))
        {
            services.push(Service {
                name: name.to_string(),
                exec: Vec::new(),
                trigger: Trigger::Startup,
                restart_on_failure: false,
                max_runs: 0,
            });
            continue;
        }
        let service = services
            .last_mut()
            .ok_or_else(|| at("expected [service.name] first"))?;
        let (key, value) = line
            .split_once('=')
            .ok_or_else(|| at("expected key = value"))?;
        let value = value.trim();
        match key.trim() {
            "exec" => {
                let cmd = quoted(value).ok_or_else(|| at("exec needs a quoted command"))?;
                service.exec = cmd.split_whitespace().map(String::from).collect();
            }
            "trigger" => service.trigger = trigger(value).ok_or_else(|| at("unknown trigger"))?,
            "restart" => {
                service.restart_on_failure = match quoted(value) {
                    Some("on-failure") => true,
                    Some("no") => false,
                    _ => return Err(at("restart is \"on-failure\" or \"no\"")),
                }
            }
            "max_runs" => {
                service.max_runs = value.parse().map_err(|_| at("max_runs is a number"))?
            }
            _ => return Err(at("unknown key")),
        }
    }
    if let Some(s) = services.iter().find(|s| s.exec.is_empty()) {
        return Err(format!("service {}: no exec", s.name));
    }
    Ok(services)
}

/// `"..."` の中身
fn quoted(value: &str) -> Option<&str> {
    value.strip_prefix('"')?.strip_suffix('"')
}

/// `"startup"`、`{ every = "50ms" }`、`{ gpio = 3 }`
fn trigger(value: &str) -> Option<Trigger> {
    if quoted(value) == Some("startup") {
        return Some(Trigger::Startup);
    }
    let inner = value.strip_prefix('{')?.strip_suffix('}')?;
    let (key, v) = inner.split_once('=')?;
    let v = v.trim();
    match key.trim() {
        "every" => {
            let d = quoted(v)?;
            let ms = if let Some(n) = d.strip_suffix("ms") {
                n.parse().ok()?
            } else {
                d.strip_suffix('s')?.parse::<u64>().ok()? * 1000
            };
            (ms > 0).then_some(Trigger::Every(ms))
        }
        "gpio" => v.parse().ok().map(Trigger::Gpio),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_services_with_each_kind_of_trigger() {
        let text = r#"
# コメント
[service.hello]
exec = "/bin/hello"
trigger = "startup"

[service.tick]
exec = "/bin/qjs /bin/greet.js"
trigger = { every = "30ms" }
max_runs = 3

[service.button]
exec = "/bin/hello"
trigger = { gpio = 3 }
restart = "on-failure"
"#;
        let s = parse(text).unwrap();
        assert_eq!(s.len(), 3);
        assert_eq!(
            s[0],
            Service {
                name: "hello".into(),
                exec: vec!["/bin/hello".into()],
                trigger: Trigger::Startup,
                restart_on_failure: false,
                max_runs: 0,
            }
        );
        assert_eq!(s[1].exec, ["/bin/qjs", "/bin/greet.js"]);
        assert_eq!(s[1].trigger, Trigger::Every(30));
        assert_eq!(s[1].max_runs, 3);
        assert_eq!(s[2].trigger, Trigger::Gpio(3));
        assert!(s[2].restart_on_failure);
    }

    #[test]
    fn durations_accept_ms_and_s() {
        let s = parse("[service.a]\nexec = \"/x\"\ntrigger = { every = \"2s\" }\n").unwrap();
        assert_eq!(s[0].trigger, Trigger::Every(2000));
    }

    #[test]
    fn mistakes_are_reported_with_the_line_number() {
        let bad = [
            ("exec = \"/x\"\n", "line 1"),          // [service.] より前
            ("[service.a]\nexec = /x\n", "line 2"), // 引用符がない
            (
                "[service.a]\nexec = \"/x\"\ntrigger = \"sometimes\"\n",
                "line 3",
            ),
            (
                "[service.a]\nexec = \"/x\"\ntrigger = { every = \"5m\" }\n",
                "line 3",
            ),
            ("[service.a]\nexec = \"/x\"\ncolour = \"red\"\n", "line 3"),
            ("[service.a]\ntrigger = \"startup\"\n", "no exec"), // exec がない
        ];
        for (text, want) in bad {
            let e = parse(text).unwrap_err();
            assert!(e.contains(want), "{text:?}: {e}");
        }
    }
}
