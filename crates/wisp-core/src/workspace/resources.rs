pub(crate) trait Files {
    fn read(&self, path: &str) -> Option<String>;
}

pub(crate) struct Host;

impl Files for Host {
    fn read(&self, path: &str) -> Option<String> {
        std::fs::read_to_string(format!("/{path}")).ok()
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub(crate) struct Resources {
    pub(crate) cpus_visible: Option<u32>,
    pub(crate) cpu_quota: Option<f64>,
    pub(crate) memory_total: Option<u64>,
    pub(crate) memory_limit: Option<u64>,
}

type Limit<T> = Option<Option<T>>;

const MOUNT: &str = "sys/fs/cgroup";
const GIB: u64 = 1 << 30;
const MIB: u64 = 1 << 20;

pub(crate) fn read(root: &dyn Files, cgroup: Option<&str>) -> Resources {
    let cgroup = cgroup
        .map(str::to_owned)
        .or_else(|| root.read("proc/self/cgroup"))
        .unwrap_or_default();
    let dirs = v2_dirs(&cgroup);
    let cpu_quota = smallest(root, &dirs, "cpu.max", cpu_max)
        .or_else(|| v1_cpu(root, &cgroup))
        .flatten();
    let memory_limit = smallest(root, &dirs, "memory.max", memory_max)
        .or_else(|| v1_memory(root, &cgroup))
        .flatten();
    Resources {
        cpus_visible: root
            .read("proc/self/status")
            .as_deref()
            .and_then(cpus_allowed),
        cpu_quota,
        memory_total: root.read("proc/meminfo").as_deref().and_then(mem_total),
        memory_limit,
    }
}

fn v2_dirs(cgroup: &str) -> Vec<String> {
    let Some(path) = cgroup.lines().find_map(|l| l.strip_prefix("0::")) else {
        return Vec::new();
    };
    let leaf = format!("{MOUNT}{}", path.trim_end_matches('/'));
    let mut dirs = vec![leaf];
    while let Some(parent) = dirs
        .last()
        .and_then(|d| d.rsplit_once('/'))
        .map(|(parent, _)| parent.to_owned())
        .filter(|parent| parent.len() >= MOUNT.len())
    {
        dirs.push(parent);
    }
    dirs
}

fn smallest<T: PartialOrd + Copy>(
    root: &dyn Files,
    dirs: &[String],
    file: &str,
    parse: fn(&str) -> Limit<T>,
) -> Limit<T> {
    let found: Vec<Option<T>> = dirs
        .iter()
        .filter_map(|d| parse(&root.read(&format!("{d}/{file}"))?))
        .collect();
    if found.is_empty() {
        return None;
    }
    Some(
        found
            .into_iter()
            .flatten()
            .reduce(|a, b| if b < a { b } else { a }),
    )
}

fn v1_dirs(cgroup: &str, controller: &str) -> Vec<String> {
    cgroup
        .lines()
        .find_map(|l| {
            let mut fields = l.splitn(3, ':');
            let (_, names, path) = (fields.next()?, fields.next()?, fields.next()?);
            names.split(',').any(|n| n == controller).then_some(path)
        })
        .map(|path| {
            vec![
                format!("{MOUNT}/{controller}{}", path.trim_end_matches('/')),
                format!("{MOUNT}/{controller}"),
            ]
        })
        .unwrap_or_default()
}

fn v1_cpu(root: &dyn Files, cgroup: &str) -> Limit<f64> {
    v1_dirs(cgroup, "cpu").iter().find_map(|d| {
        cfs(
            &root.read(&format!("{d}/cpu.cfs_quota_us"))?,
            &root.read(&format!("{d}/cpu.cfs_period_us"))?,
        )
    })
}

fn v1_memory(root: &dyn Files, cgroup: &str) -> Limit<u64> {
    v1_dirs(cgroup, "memory").iter().find_map(|d| {
        let text = root.read(&format!("{d}/memory.limit_in_bytes"))?;
        text.trim().parse().ok().map(Some)
    })
}

fn cpus_allowed(status: &str) -> Option<u32> {
    let list = status
        .lines()
        .find_map(|l| l.strip_prefix("Cpus_allowed_list:"))?
        .trim();
    if list.is_empty() {
        return None;
    }
    list.split(',').try_fold(0u32, |count, part| {
        let (low, high) = part.split_once('-').unwrap_or((part, part));
        let (low, high): (u32, u32) = (low.parse().ok()?, high.parse().ok()?);
        count.checked_add(high.checked_sub(low)?.checked_add(1)?)
    })
}

fn cpu_max(text: &str) -> Limit<f64> {
    let (quota, period) = text.trim().split_once(' ')?;
    if quota == "max" {
        return period.parse::<u64>().ok().map(|_| None);
    }
    cfs(quota, period)
}

fn cfs(quota: &str, period: &str) -> Limit<f64> {
    let quota: i64 = quota.trim().parse().ok()?;
    let period: u64 = period.trim().parse().ok()?;
    if quota == -1 {
        return Some(None);
    }
    (quota > 0 && period > 0).then(|| Some(quota as f64 / period as f64))
}

fn memory_max(text: &str) -> Limit<u64> {
    match text.trim() {
        "max" => Some(None),
        bytes => bytes.parse().ok().map(Some),
    }
}

fn mem_total(meminfo: &str) -> Option<u64> {
    let kib: u64 = meminfo
        .lines()
        .find_map(|l| l.strip_prefix("MemTotal:"))?
        .trim()
        .strip_suffix("kB")?
        .trim()
        .parse()
        .ok()?;
    kib.checked_mul(1024)
}

pub(crate) fn line(facts: &Resources) -> Option<String> {
    let all_cpus = facts.cpus_visible.map(|visible| cpus(f64::from(visible)));
    let cpu = facts
        .cpus_visible
        .zip(all_cpus.clone())
        .map(|(visible, machine)| {
            match facts
                .cpu_quota
                .filter(|quota| *quota < f64::from(visible))
                .map(cpus)
                .filter(|shown| *shown != machine)
            {
                Some(shown) => Half::Limited {
                    shown,
                    shows: visible.to_string(),
                },
                None => Half::Machine(machine),
            }
        });
    let memory = facts.memory_total.map(|total| {
        let machine = size(total);
        match facts
            .memory_limit
            .filter(|limit| *limit < total)
            .map(size)
            .filter(|shown| *shown != machine)
        {
            Some(shown) => Half::Limited {
                shown: format!("{shown} memory"),
                shows: machine,
            },
            None => Half::Machine(format!("{machine} memory")),
        }
    });
    let text = match (cpu, memory, all_cpus) {
        (
            Some(Half::Limited { shown: cpus, .. }),
            Some(Half::Limited {
                shown: memory,
                shows: all_memory,
            }),
            Some(all_cpus),
        ) => format!(
            "{cpus} and {memory} (cgroup limits; nproc and free show the machine's {all_cpus} and {all_memory})"
        ),
        (cpu, memory, _) => {
            let parts: Vec<String> = [cpu.map(|c| c.text("nproc")), memory.map(|m| m.text("free"))]
                .into_iter()
                .flatten()
                .collect();
            if parts.is_empty() {
                return None;
            }
            parts.join(", ")
        }
    };
    Some(format!("Resources: {text}"))
}

enum Half {
    Machine(String),
    Limited { shown: String, shows: String },
}

impl Half {
    fn text(self, tool: &str) -> String {
        match self {
            Self::Machine(text) => text,
            Self::Limited { shown, shows } => {
                format!("{shown} (cgroup limit; {tool} shows {shows})")
            }
        }
    }
}

fn cpus(count: f64) -> String {
    let tenths = ((count * 10.0).round() as u64).max(1);
    match (tenths / 10, tenths % 10) {
        (1, 0) => "1 CPU".to_owned(),
        (whole, 0) => format!("{whole} CPUs"),
        (whole, tenth) => format!("{whole}.{tenth} CPUs"),
    }
}

fn size(bytes: u64) -> String {
    let divide = |unit: u64, scale: u128| u128::from(bytes) * scale / u128::from(unit);
    if bytes < GIB {
        return format!("{} MiB", divide(MIB, 1));
    }
    let tenths = divide(GIB, 10);
    if tenths < 100 && tenths % 10 != 0 {
        format!("{}.{} GiB", tenths / 10, tenths % 10)
    } else {
        format!("{} GiB", divide(GIB, 1))
    }
}

#[cfg(test)]
mod tests;
