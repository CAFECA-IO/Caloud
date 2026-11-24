//! 平臺環境沙箱（FR-OPS-11、ADR 0015）：每個環境以 gVisor 執行，獨立的 `cld<索引>` bridge 與固定網段。
//!
//! gVisor 下 Docker 內建 DNS 不能用，所以服務使用固定 IP，名稱以 `extra_hosts` 寫入，
//! 並掛載 agent 產生的 `resolv.conf`。控制面與 agent 共用這裡的計算，結果一致。
use crate::policy::ValidatedCompose;
use serde_yaml::value::{Tag, TaggedValue};
use serde_yaml::{Mapping, Value};
use std::net::Ipv4Addr;

/// gVisor runtime 在 daemon.json 的名稱
pub const RUNTIME: &str = "runsc";
/// GPU 服務的 runtime：同一個 runsc，帶 `--nvproxy`（ADR 0022）
pub const RUNTIME_GPU: &str = "runsc-gpu";
/// GPU 以 CDI 裝置掛入（`runner-setup.sh` 產生 `/etc/cdi/caloud-gpu.json`，ADR 0022）
pub const GPU_DEVICE: &str = "caloud.dev/gpu=all";

/// 是否有服務宣告 GPU（ADR 0022）
pub fn needs_gpu(compose: &ValidatedCompose) -> bool {
    !compose.gpu_services.is_empty()
}

/// 未經政策檢查的 compose 是否有服務宣告 GPU（agent 端用；compose 已由控制面檢查過）
pub fn declares_gpu(yaml: &str) -> bool {
    serde_yaml::from_str::<Value>(yaml)
        .ok()
        .and_then(|d| d.get("services").and_then(Value::as_mapping).cloned())
        .is_some_and(|svcs| {
            svcs.values().any(|v| {
                v.get("deploy")
                    .and_then(|d| d.get("resources"))
                    .and_then(|r| r.get("reservations"))
                    .and_then(|r| r.get("devices"))
                    .is_some()
            })
        })
}

/// 清掉用戶的 GPU 宣告（Docker 的 `--gpus` 要重啟 daemon 才能用），改由 CDI 裝置掛入所有 GPU（分時共用）
fn gpu_reservation() -> Value {
    let mut reservations = Mapping::new();
    reservations.insert(
        "devices".into(),
        Value::Tagged(Box::new(TaggedValue {
            tag: Tag::new("reset"),
            value: Value::Sequence(vec![]),
        })),
    );
    let mut resources = Mapping::new();
    resources.insert("reservations".into(), Value::Mapping(reservations));
    let mut deploy = Mapping::new();
    deploy.insert("resources".into(), Value::Mapping(resources));
    Value::Mapping(deploy)
}
/// 環境網段的索引上限（`10.240.0.0/12` 共 4096 個 /24）
pub const MAX_INDEX: u16 = 4095;
/// 保留給自我檢查探測環境的索引（控制面不配置）
pub const PROBE_INDEXES: [u16; 2] = [4094, 4095];
/// 控制面可配置的最大索引
pub const MAX_ALLOC_INDEX: u16 = 4093;
/// 環境內服務的固定 IP 從 `.10` 起
const FIRST_HOST: u8 = 10;
/// 環境內最多服務數（.10–.253）
pub const MAX_SERVICES: usize = 244;
/// override 中環境網路的鍵
const NET_KEY: &str = "caloud_env";

/// 一個環境的網段
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EnvNet {
    pub index: u16,
}

impl EnvNet {
    pub fn new(index: u16) -> Option<Self> {
        (index <= MAX_INDEX).then_some(Self { index })
    }

    fn octets(&self) -> (u8, u8) {
        (240 + (self.index >> 8) as u8, (self.index & 0xff) as u8)
    }

    /// 例：索引 0 → `10.240.0.0/24`，索引 300 → `10.241.44.0/24`
    pub fn subnet(&self) -> String {
        let (b, c) = self.octets();
        format!("10.{b}.{c}.0/24")
    }

    pub fn gateway(&self) -> Ipv4Addr {
        let (b, c) = self.octets();
        Ipv4Addr::new(10, b, c, 1)
    }

    /// Linux 介面名稱（≤ 15 字元），防火牆以 `cld+` 比對
    pub fn bridge(&self) -> String {
        format!("cld{:x}", self.index)
    }

    /// 第 n 個服務（從 0 起）的固定 IP
    pub fn service_ip(&self, n: usize) -> Option<Ipv4Addr> {
        (n < MAX_SERVICES).then(|| {
            let (b, c) = self.octets();
            Ipv4Addr::new(10, b, c, FIRST_HOST + n as u8)
        })
    }
}

/// 每個服務的資源上限
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Limits {
    pub cpus: f64,
    pub memory_mb: u64,
    pub pids: u32,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            cpus: 1.0,
            memory_mb: 1024,
            pids: 512,
        }
    }
}

/// 環境的 compose project 名稱
pub fn project(env_id: &str) -> String {
    format!("caloud-env-{env_id}")
}

/// M4 的環境只接受 image（建置在 M6）
pub fn check_image_only(compose: &ValidatedCompose) -> Result<(), String> {
    for name in &compose.services {
        let svc = &compose.doc["services"][name.as_str()];
        if svc.get("build").is_some() {
            return Err(format!("service `{name}`：目前環境只支援 image，建置將在之後開放。"));
        }
        if svc.get("image").and_then(Value::as_str).is_none() {
            return Err(format!("service `{name}`：請指定 image。"));
        }
    }
    if compose.services.len() > MAX_SERVICES {
        return Err(format!("一個環境最多 {MAX_SERVICES} 個 service。"));
    }
    Ok(())
}

/// 服務名稱與固定 IP（依 compose 中的順序）
pub fn service_ips(compose: &ValidatedCompose, net: &EnvNet) -> Vec<(String, Ipv4Addr)> {
    compose
        .services
        .iter()
        .enumerate()
        .filter_map(|(i, s)| net.service_ip(i).map(|ip| (s.clone(), ip)))
        .collect()
}

fn overriding(v: Value) -> Value {
    Value::Tagged(Box::new(TaggedValue {
        tag: Tag::new("override"),
        value: v,
    }))
}

/// 產生環境的 override：gVisor、只接環境網路、固定 IP、名稱解析、資源上限
pub fn env_override(env_id: &str, compose: &ValidatedCompose, net: &EnvNet, limits: Limits) -> Result<String, String> {
    env_override_with(env_id, compose, net, limits, &[])
}

/// 同 [`env_override`]，並在 `inject` 列出的服務加上 `environment: [名稱…]`（只有名稱，值由
/// `docker compose` 的行程環境變數提供，ADR 0019）
pub fn env_override_with(
    env_id: &str,
    compose: &ValidatedCompose,
    net: &EnvNet,
    limits: Limits,
    inject: &[(String, Vec<String>)],
) -> Result<String, String> {
    check_image_only(compose)?;
    let ips = service_ips(compose, net);
    let hosts: Vec<Value> = ips.iter().map(|(n, ip)| Value::String(format!("{n}:{ip}"))).collect();
    let mut services = Mapping::new();
    for (name, ip) in &ips {
        let orig = compose.doc["services"][name.as_str()].as_mapping().cloned().unwrap_or_default();
        let mut svc = Mapping::new();
        if compose.gpu_services.contains(name) {
            svc.insert("runtime".into(), RUNTIME_GPU.into());
            svc.insert("devices".into(), Value::Sequence(vec![GPU_DEVICE.into()]));
            svc.insert("deploy".into(), gpu_reservation());
        } else {
            svc.insert("runtime".into(), RUNTIME.into());
        }
        let mut addr = Mapping::new();
        addr.insert("ipv4_address".into(), ip.to_string().into());
        let mut nets = Mapping::new();
        nets.insert(NET_KEY.into(), Value::Mapping(addr));
        // 不論原本接哪些網路，一律只接環境網路
        svc.insert("networks".into(), overriding(Value::Mapping(nets)));
        svc.insert("extra_hosts".into(), Value::Sequence(hosts.clone()));
        svc.insert("dns".into(), overriding(Value::Sequence(vec![])));
        svc.insert("volumes".into(), Value::Sequence(vec!["./resolv.conf:/etc/resolv.conf:ro".into()]));
        svc.insert("mem_limit".into(), format!("{}m", limits.memory_mb).into());
        svc.insert("cpus".into(), limits.cpus.into());
        svc.insert("pids_limit".into(), u64::from(limits.pids).into());
        if !orig.contains_key("restart") {
            svc.insert("restart".into(), "unless-stopped".into());
        }
        let mut labels = Mapping::new();
        labels.insert("io.caloud.env".into(), env_id.into());
        svc.insert("labels".into(), Value::Mapping(labels));
        if let Some((_, names)) = inject.iter().find(|(s, _)| s == name).filter(|(_, n)| !n.is_empty()) {
            svc.insert(
                "environment".into(),
                Value::Sequence(names.iter().map(|n| Value::String(n.clone())).collect()),
            );
        }
        services.insert(name.as_str().into(), Value::Mapping(svc));
    }
    let mut opts = Mapping::new();
    opts.insert("com.docker.network.bridge.name".into(), net.bridge().into());
    let mut ipam_cfg = Mapping::new();
    ipam_cfg.insert("subnet".into(), net.subnet().into());
    ipam_cfg.insert("gateway".into(), net.gateway().to_string().into());
    let mut ipam = Mapping::new();
    ipam.insert("config".into(), Value::Sequence(vec![Value::Mapping(ipam_cfg)]));
    let mut env_net = Mapping::new();
    env_net.insert("name".into(), project(env_id).into());
    env_net.insert("driver".into(), "bridge".into());
    env_net.insert("driver_opts".into(), Value::Mapping(opts));
    env_net.insert("ipam".into(), Value::Mapping(ipam));
    let mut networks = Mapping::new();
    networks.insert(NET_KEY.into(), Value::Mapping(env_net));
    let mut root = Mapping::new();
    root.insert("services".into(), Value::Mapping(services));
    root.insert("networks".into(), Value::Mapping(networks));
    serde_yaml::to_string(&Value::Mapping(root)).map_err(|e| e.to_string())
}

/// 環境的 resolv.conf（外部 DNS）
pub fn resolv_conf(dns: &[String]) -> String {
    let mut s: String = dns.iter().map(|d| format!("nameserver {d}\n")).collect();
    s.push_str("options ndots:0\n");
    s
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::policy::{validate_with, Policy};

    // FR-OPS-11、ADR 0015
    #[test]
    fn env_net_layout() {
        let n = EnvNet::new(0).unwrap();
        assert_eq!(n.subnet(), "10.240.0.0/24");
        assert_eq!(n.gateway(), Ipv4Addr::new(10, 240, 0, 1));
        assert_eq!(n.bridge(), "cld0");
        assert_eq!(n.service_ip(0), Some(Ipv4Addr::new(10, 240, 0, 10)));
        let n = EnvNet::new(300).unwrap();
        assert_eq!(n.subnet(), "10.241.44.0/24");
        assert_eq!(n.bridge(), "cld12c");
        let last = EnvNet::new(MAX_INDEX).unwrap();
        assert_eq!(last.subnet(), "10.255.255.0/24");
        assert!(last.bridge().len() <= 15);
        assert_eq!(last.service_ip(MAX_SERVICES - 1), Some(Ipv4Addr::new(10, 255, 255, 253)));
        assert_eq!(last.service_ip(MAX_SERVICES), None);
        assert!(EnvNet::new(MAX_INDEX + 1).is_none());
        assert!(PROBE_INDEXES.iter().all(|i| *i > MAX_ALLOC_INDEX));
    }

    // FR-OPS-11、ADR 0015：gVisor、只接環境網路、固定 IP、名稱解析
    #[test]
    fn override_isolates_environment() {
        let y = "services:\n  web:\n    image: nginx:1.27\n    networks: [front]\n  db:\n    image: postgres:16\n    restart: always\nnetworks:\n  front: {}\n";
        let c = validate_with(y, Policy::platform(0)).unwrap();
        let o = env_override("k2m9xq7a4tz1", &c, &EnvNet::new(5).unwrap(), Limits::default()).unwrap();
        // ADR 0019：只有指定的服務加上變數名稱，不含值
        let inj = env_override_with(
            "k2m9xq7a4tz1",
            &c,
            &EnvNet::new(5).unwrap(),
            Limits::default(),
            &[("web".to_string(), vec!["SESSION_SECRET".to_string()])],
        )
        .unwrap();
        let d: Value = serde_yaml::from_str(&inj).unwrap();
        assert_eq!(d["services"]["web"]["environment"][0], "SESSION_SECRET");
        assert!(d["services"]["db"].get("environment").is_none());
        assert!(!o.contains("environment"));
        let doc: Value = serde_yaml::from_str(&o).unwrap();
        let web = &doc["services"]["web"];
        assert_eq!(web["runtime"], "runsc");
        assert_eq!(web["mem_limit"], "1024m");
        assert_eq!(web["restart"], "unless-stopped");
        assert!(doc["services"]["db"].get("restart").is_none(), "保留原本的 restart");
        assert!(o.contains("networks: !override"), "{o}");
        assert!(o.contains("ipv4_address: 10.240.5.10"));
        assert!(o.contains("ipv4_address: 10.240.5.11"));
        assert!(o.contains("web:10.240.5.10") && o.contains("db:10.240.5.11"));
        assert!(o.contains("./resolv.conf:/etc/resolv.conf:ro"));
        assert_eq!(doc["networks"]["caloud_env"]["name"], "caloud-env-k2m9xq7a4tz1");
        assert_eq!(
            doc["networks"]["caloud_env"]["driver_opts"]["com.docker.network.bridge.name"],
            "cld5"
        );
        assert_eq!(doc["networks"]["caloud_env"]["ipam"]["config"][0]["subnet"], "10.240.5.0/24");
        assert_eq!(
            service_ips(&c, &EnvNet::new(5).unwrap()),
            vec![
                ("web".to_string(), Ipv4Addr::new(10, 240, 5, 10)),
                ("db".to_string(), Ipv4Addr::new(10, 240, 5, 11))
            ]
        );

        let b = validate_with("services:\n  web:\n    build: .\n", Policy::platform(0)).unwrap();
        assert!(env_override("x1x1x1", &b, &EnvNet::new(0).unwrap(), Limits::default())
            .unwrap_err()
            .contains("image"));
        assert_eq!(resolv_conf(&["1.1.1.1".into()]), "nameserver 1.1.1.1\noptions ndots:0\n");
        assert!(!needs_gpu(&c));
    }

    // ADR 0022：只有宣告 GPU 的服務用 runsc-gpu，宣告改為 CDI 裝置
    #[test]
    fn gpu_services_use_nvproxy_runtime() {
        let y = "services:\n  infer:\n    image: x\n    deploy: {resources: {limits: {cpus: '2'}, reservations: {devices: [{capabilities: [gpu], device_ids: ['3']}]}}}\n  web:\n    image: y\n";
        let c = validate_with(y, Policy::platform(0)).unwrap();
        assert!(needs_gpu(&c));
        let o = env_override("g1g1g1g1", &c, &EnvNet::new(1).unwrap(), Limits::default()).unwrap();
        let d: Value = serde_yaml::from_str(&o).unwrap();
        assert_eq!(d["services"]["infer"]["runtime"], RUNTIME_GPU);
        assert_eq!(d["services"]["web"]["runtime"], RUNTIME);
        assert!(d["services"]["web"].get("deploy").is_none());
        assert!(o.contains("devices: !reset []"), "{o}");
        assert_eq!(d["services"]["infer"]["devices"][0], GPU_DEVICE);
        assert!(d["services"]["web"].get("devices").is_none());
        assert!(declares_gpu(y) && !declares_gpu("services: {web: {image: y}}") && !declares_gpu("::"));
    }
}
