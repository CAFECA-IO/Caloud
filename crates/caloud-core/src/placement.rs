//! 依容量選擇節點（FR-OPS-08）。
use serde::{Deserialize, Serialize};

/// 偵測規格保留給主機自己的比例（10%）
pub const HOST_RESERVE_PERCENT: u64 = 10;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Resources {
    pub cpu_millis: u64,
    pub memory_bytes: u64,
}

impl Resources {
    fn fits(&self, need: Resources) -> bool {
        self.cpu_millis >= need.cpu_millis && self.memory_bytes >= need.memory_bytes
    }

    fn minus(&self, other: Resources) -> Resources {
        Resources {
            cpu_millis: self.cpu_millis.saturating_sub(other.cpu_millis),
            memory_bytes: self.memory_bytes.saturating_sub(other.memory_bytes),
        }
    }
}

/// 由偵測規格算出可配置量：保留 10% 給主機（控制面計算，不信任 agent 自報）
pub fn allocatable(cpu_cores: u32, memory_bytes: u64) -> Resources {
    let keep = 100 - HOST_RESERVE_PERCENT;
    Resources {
        cpu_millis: u64::from(cpu_cores) * 1000 * keep / 100,
        memory_bytes: memory_bytes / 100 * keep,
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum NodeStatus {
    Active,
    Draining,
    Offline,
}

impl NodeStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            NodeStatus::Active => "active",
            NodeStatus::Draining => "draining",
            NodeStatus::Offline => "offline",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        [NodeStatus::Active, NodeStatus::Draining, NodeStatus::Offline]
            .into_iter()
            .find(|k| k.as_str() == s)
    }
}

/// 調度用的節點快照
#[derive(Debug, Clone)]
pub struct Candidate {
    pub id: String,
    /// None：平臺節點；Some：用戶自有設備
    pub owner: Option<String>,
    pub status: NodeStatus,
    pub allocatable: Resources,
    pub allocated: Resources,
}

/// 工作要放在哪類節點
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Target<'a> {
    /// 平臺節點
    Platform,
    /// 某位用戶的自有設備
    OwnDevice(&'a str),
}

/// FR-OPS-08：在符合目標、`active`、放得下的節點中，選放入後剩餘比例最高者；同分取 id 較小者
pub fn choose_node<'a>(nodes: &'a [Candidate], need: Resources, target: Target<'_>) -> Option<&'a Candidate> {
    let ratio = |free: u64, total: u64| if total == 0 { 0.0 } else { free as f64 / total as f64 };
    nodes
        .iter()
        .filter(|n| n.status == NodeStatus::Active)
        .filter(|n| match target {
            Target::Platform => n.owner.is_none(),
            Target::OwnDevice(account) => n.owner.as_deref() == Some(account),
        })
        .filter(|n| n.allocatable.minus(n.allocated).fits(need))
        .map(|n| {
            let left = n.allocatable.minus(n.allocated).minus(need);
            let score = ratio(left.cpu_millis, n.allocatable.cpu_millis).min(ratio(left.memory_bytes, n.allocatable.memory_bytes));
            (n, score)
        })
        .max_by(|(a, sa), (b, sb)| sa.total_cmp(sb).then_with(|| b.id.cmp(&a.id)))
        .map(|(n, _)| n)
}

#[cfg(test)]
mod tests {
    use super::*;

    const GB: u64 = 1 << 30;

    fn node(id: &str, owner: Option<&str>, status: NodeStatus, cores: u32, mem_gb: u64, used_cpu: u64, used_gb: u64) -> Candidate {
        Candidate {
            id: id.into(),
            owner: owner.map(Into::into),
            status,
            allocatable: allocatable(cores, mem_gb * GB),
            allocated: Resources {
                cpu_millis: used_cpu,
                memory_bytes: used_gb * GB,
            },
        }
    }

    // FR-OPS-08
    #[test]
    fn allocatable_keeps_ten_percent() {
        let r = allocatable(16, 64 * GB);
        assert_eq!(r.cpu_millis, 14_400);
        assert_eq!(r.memory_bytes, 64 * GB / 100 * 90);
    }

    // FR-OPS-08、FR-OPS-06、FR-DEV-02
    #[test]
    fn picks_most_room_and_respects_status_and_owner() {
        let need = Resources {
            cpu_millis: 1000,
            memory_bytes: GB,
        };
        let nodes = vec![
            node("a", None, NodeStatus::Active, 16, 64, 12_000, 50),
            node("b", None, NodeStatus::Active, 8, 32, 1_000, 4),
            node("c", None, NodeStatus::Draining, 32, 128, 0, 0),
            node("d", None, NodeStatus::Offline, 32, 128, 0, 0),
            node("e", Some("0xaa"), NodeStatus::Active, 32, 128, 0, 0),
        ];
        assert_eq!(choose_node(&nodes, need, Target::Platform).unwrap().id, "b");
        assert_eq!(choose_node(&nodes, need, Target::OwnDevice("0xaa")).unwrap().id, "e");
        assert!(choose_node(&nodes, need, Target::OwnDevice("0xbb")).is_none());
        // 放不下就不選
        let huge = Resources {
            cpu_millis: 20_000,
            memory_bytes: GB,
        };
        assert!(choose_node(&nodes, huge, Target::Platform).is_none());
        // 同分取 id 較小者
        let twins = vec![
            node("y", None, NodeStatus::Active, 4, 8, 0, 0),
            node("x", None, NodeStatus::Active, 4, 8, 0, 0),
        ];
        assert_eq!(choose_node(&twins, need, Target::Platform).unwrap().id, "x");
    }
}
