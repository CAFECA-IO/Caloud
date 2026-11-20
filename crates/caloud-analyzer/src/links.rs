//! 服務間位址（FR-AN-11，M5-09）：程式以 `localhost`／`127.0.0.1` 連到同一份 compose 中其他服務的 port。
//!
//! 在平臺上每個服務是獨立容器，127.0.0.1 指向自己。可由環境變數設定時，在 App 服務的
//! `environment` 填入服務名稱位址（中信心，需確認）；寫死時列為 High 並附依據。
use crate::runtime::Loopback;
use crate::{Confidence, Ctx};
use serde_yaml::{Mapping, Value};
use std::collections::BTreeMap;

/// 已知映像的預設 port
const IMAGE_PORTS: &[(&str, &[u16])] = &[
    ("postgres", &[5432]),
    ("postgis", &[5432]),
    ("timescaledb", &[5432]),
    ("mysql", &[3306]),
    ("mariadb", &[3306]),
    ("redis", &[6379]),
    ("valkey", &[6379]),
    ("keydb", &[6379]),
    ("mongo", &[27017]),
    ("memcached", &[11211]),
    ("kubo", &[5001, 8080, 4001]),
    ("go-ipfs", &[5001, 8080, 4001]),
    ("rabbitmq", &[5672, 15672]),
    ("minio", &[9000, 9001]),
    ("elasticsearch", &[9200]),
    ("meilisearch", &[7700]),
    ("nats", &[4222]),
];

fn image_base(image: &str) -> &str {
    image
        .rsplit('/')
        .next()
        .unwrap_or(image)
        .split([':', '@'])
        .next()
        .unwrap_or_default()
}

/// `"5001:5001"`、`"127.0.0.1:8080:80/tcp"`、`5432`、`{target: 80}` → container port
fn container_port(v: &Value) -> Option<u16> {
    match v {
        Value::Number(n) => n.as_u64().and_then(|n| u16::try_from(n).ok()),
        Value::String(s) => s.split('/').next()?.rsplit(':').next()?.split('-').next()?.trim().parse().ok(),
        Value::Mapping(m) => m.get("target").and_then(container_port),
        _ => None,
    }
}

/// 服務 → 它在容器內提供的 port
fn service_ports(svc: &Mapping) -> Vec<u16> {
    let mut out: Vec<u16> = ["ports", "expose"]
        .iter()
        .filter_map(|k| svc.get(*k).and_then(Value::as_sequence))
        .flatten()
        .filter_map(container_port)
        .collect();
    if let Some(img) = svc.get("image").and_then(Value::as_str) {
        let base = image_base(img);
        if let Some((_, ports)) = IMAGE_PORTS.iter().find(|(n, _)| *n == base) {
            out.extend_from_slice(ports);
        }
    }
    out.sort_unstable();
    out.dedup();
    out
}

/// `http://127.0.0.1:5001/api` → `http://ipfs:5001/api`
fn rewrite(url: &str, service: &str) -> String {
    let (scheme, rest) = match url.split_once("://") {
        Some((s, r)) => (format!("{s}://"), r),
        None => (String::new(), url),
    };
    let (userinfo, hostpart) = match rest.rsplit_once('@') {
        Some((u, h)) => (format!("{u}@"), h),
        None => (String::new(), rest),
    };
    let split = hostpart.find(['/', '?', '#']).unwrap_or(hostpart.len());
    let (hostport, tail) = hostpart.split_at(split);
    let port = hostport.rsplit_once(':').map(|(_, p)| p).unwrap_or_default();
    format!("{scheme}{userinfo}{service}:{port}{tail}")
}

fn has_env(svc: &Mapping, name: &str) -> bool {
    match svc.get("environment") {
        Some(Value::Mapping(m)) => m.contains_key(name),
        Some(Value::Sequence(l)) => l.iter().filter_map(Value::as_str).any(|e| e.split('=').next() == Some(name)),
        _ => false,
    }
}

fn set_env(svc: &mut Mapping, name: &str, value: &str) {
    let env = svc.entry("environment".into()).or_insert_with(|| Value::Mapping(Mapping::new()));
    match env {
        Value::Mapping(m) => {
            m.insert(name.into(), value.into());
        }
        Value::Sequence(l) => l.push(format!("{name}={value}").into()),
        other => {
            let mut m = Mapping::new();
            m.insert(name.into(), value.into());
            *other = Value::Mapping(m);
        }
    }
}

/// 依 port 對應服務並改寫 compose；回傳由平臺填入的變數名稱
pub(crate) fn apply(cx: &mut Ctx, doc: &mut Value) -> Vec<String> {
    let loops: Vec<Loopback> = cx.code().loopback.clone();
    if loops.is_empty() {
        return vec![];
    }
    let Some(services) = doc.get_mut("services").and_then(Value::as_mapping_mut) else {
        return vec![];
    };
    let apps: Vec<String> = services
        .iter()
        .filter(|(_, s)| s.get("build").is_some())
        .filter_map(|(n, _)| n.as_str().map(str::to_string))
        .collect();
    if apps.is_empty() {
        return vec![];
    }
    // port → 提供它的服務（App 自己除外；多個服務提供同一 port 時不猜）
    let mut by_port: BTreeMap<u16, Vec<String>> = BTreeMap::new();
    for (n, s) in services.iter() {
        let Some(name) = n.as_str() else { continue };
        if apps.iter().any(|a| a == name) {
            continue;
        }
        for p in service_ports(s.as_mapping().unwrap_or(&Mapping::new())) {
            by_port.entry(p).or_default().push(name.to_string());
        }
    }
    let target = |port: u16| by_port.get(&port).filter(|v| v.len() == 1).map(|v| v[0].clone());

    let mut filled: Vec<String> = Vec::new();
    // 寫死的位址依 (網址, 服務) 彙整
    let mut hard: BTreeMap<(String, String), Vec<crate::Evidence>> = BTreeMap::new();
    for l in &loops {
        let Some(svc_name) = target(l.port) else { continue };
        match &l.var {
            Some(var) => {
                if filled.contains(var) {
                    continue;
                }
                let value = rewrite(&l.url, &svc_name);
                let mut done = Vec::new();
                for app in &apps {
                    let Some(svc) = services.get_mut(app.as_str()).and_then(Value::as_mapping_mut) else {
                        continue;
                    };
                    if !has_env(svc, var) {
                        set_env(svc, var, &value);
                        done.push(app.clone());
                    }
                }
                if done.is_empty() {
                    continue;
                }
                filled.push(var.clone());
                let evidence: Vec<_> = loops
                    .iter()
                    .filter(|x| x.var.as_deref() == Some(var))
                    .map(|x| x.evidence.clone())
                    .take(3)
                    .collect();
                cx.note(
                    "service_link",
                    Confidence::Medium,
                    format!(
                        "`{var}` 預設為 `{}`，在容器中會連到自己；已為 `{}` 填入 `{var}={value}`（`{svc_name}` 提供 port {}），請確認。",
                        l.url,
                        done.join("`、`"),
                        l.port
                    ),
                    evidence,
                );
            }
            None => {
                let e = hard.entry((l.url.clone(), svc_name)).or_default();
                if e.len() < 3 && !e.contains(&l.evidence) {
                    e.push(l.evidence.clone());
                }
            }
        }
    }
    for ((url, svc_name), evidence) in hard {
        cx.note(
            "service_link",
            Confidence::High,
            format!(
                "程式寫死 `{url}`：在平臺上每個服務是獨立容器，127.0.0.1 指向自己，連不到 `{svc_name}`。請改為讀環境變數並保留原值為預設（例：`process.env.X ?? '{url}'`），Caloud 會填入 `{}`。",
                rewrite(&url, &svc_name)
            ),
            evidence,
        );
    }
    filled
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ports_and_rewrite() {
        let svc: Mapping =
            serde_yaml::from_str("image: ipfs/kubo:latest\nports: ['5001:5001', '4001:4001/udp', {target: 8081}]\nexpose: [9999]\n")
                .unwrap();
        let ports = service_ports(&svc);
        for p in [5001, 4001, 8081, 9999, 8080] {
            assert!(ports.contains(&p), "{p} {ports:?}");
        }
        assert_eq!(rewrite("http://127.0.0.1:5001", "ipfs"), "http://ipfs:5001");
        assert_eq!(
            rewrite("postgres://u:p@localhost:5432/app?x=1", "db"),
            "postgres://u:p@db:5432/app?x=1"
        );
        assert_eq!(rewrite("localhost:6379", "redis"), "redis:6379");
    }
}
