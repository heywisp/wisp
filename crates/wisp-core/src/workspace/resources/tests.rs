use std::collections::BTreeMap;

use super::*;

struct Fake(BTreeMap<&'static str, &'static str>);

impl Files for Fake {
    fn read(&self, path: &str) -> Option<String> {
        self.0.get(path).map(|s| (*s).to_owned())
    }
}

const STATUS: (&str, &str) = (
    "proc/self/status",
    "Name:\tcat\nCpus_allowed:\tffffff\nCpus_allowed_list:\t0-23\nMems_allowed_list:\t0\n",
);
const MEMINFO: (&str, &str) = (
    "proc/meminfo",
    "MemTotal:       49283724 kB\nMemFree:        1000 kB\n",
);
const GIB_47: u64 = 49283724 * 1024;

fn fake(files: &[(&'static str, &'static str)]) -> Fake {
    Fake(
        [STATUS, MEMINFO]
            .into_iter()
            .chain(files.iter().copied())
            .collect(),
    )
}

fn host(cpu_quota: Option<f64>, memory_limit: Option<u64>) -> Resources {
    Resources {
        cpus_visible: Some(24),
        cpu_quota,
        memory_total: Some(GIB_47),
        memory_limit,
    }
}

#[test]
fn a_v2_limit_at_the_leaf_is_read() {
    let root = fake(&[
        ("proc/self/cgroup", "0::/job\n"),
        ("sys/fs/cgroup/job/cpu.max", "100000 100000\n"),
        ("sys/fs/cgroup/job/memory.max", "2147483648\n"),
    ]);
    let facts = read(&root, None);
    assert_eq!(facts, host(Some(1.0), Some(2 << 30)));
    assert_eq!(
        line(&facts).unwrap(),
        "Resources: 1 CPU and 2 GiB memory (cgroup limits; nproc and free show the machine's 24 CPUs and 47 GiB)"
    );
}

#[test]
fn a_limit_at_a_parent_wins_over_max_at_the_leaf() {
    let root = fake(&[
        ("proc/self/cgroup", "0::/a/b\n"),
        ("sys/fs/cgroup/a/b/cpu.max", "max 100000\n"),
        ("sys/fs/cgroup/a/b/memory.max", "max\n"),
        ("sys/fs/cgroup/a/cpu.max", "200000 100000\n"),
        ("sys/fs/cgroup/a/memory.max", "4294967296\n"),
        ("sys/fs/cgroup/memory.max", "8589934592\n"),
    ]);
    assert_eq!(read(&root, None), host(Some(2.0), Some(4 << 30)));
}

#[test]
fn v2_without_limits_reads_none() {
    let root = fake(&[
        ("proc/self/cgroup", "0::/init.scope\n"),
        ("sys/fs/cgroup/init.scope/cpu.max", "max 100000\n"),
        ("sys/fs/cgroup/init.scope/memory.max", "max\n"),
    ]);
    let facts = read(&root, None);
    assert_eq!(facts, host(None, None));
    assert_eq!(line(&facts).unwrap(), "Resources: 24 CPUs, 47 GiB memory");
}

#[test]
fn a_root_without_cgroup_files_reads_only_the_machine() {
    let root = fake(&[("proc/self/cgroup", "0::/\n")]);
    assert_eq!(read(&root, None), host(None, None));
}

#[test]
fn a_missing_leaf_falls_back_to_the_mount_top() {
    let root = fake(&[
        ("proc/self/cgroup", "0::/docker/abc\n"),
        ("sys/fs/cgroup/cpu.max", "50000 100000\n"),
        ("sys/fs/cgroup/memory.max", "536870912\n"),
    ]);
    let facts = read(&root, None);
    assert_eq!(facts, host(Some(0.5), Some(512 << 20)));
    assert_eq!(
        line(&facts).unwrap(),
        "Resources: 0.5 CPUs and 512 MiB memory (cgroup limits; nproc and free show the machine's 24 CPUs and 47 GiB)"
    );
}

#[test]
fn the_cgroup_text_can_be_given() {
    let root = fake(&[
        ("proc/self/cgroup", "0::/wispd\n"),
        ("sys/fs/cgroup/wispd/cpu.max", "max 100000\n"),
        ("sys/fs/cgroup/tools/cpu.max", "300000 100000\n"),
    ]);
    assert_eq!(read(&root, Some("0::/tools\n")).cpu_quota, Some(3.0));
    assert_eq!(read(&root, None).cpu_quota, None);
}

#[test]
fn v1_limits_are_read() {
    let root = fake(&[
        (
            "proc/self/cgroup",
            "12:memory:/docker/abc\n4:cpu,cpuacct:/docker/abc\n0::/\n",
        ),
        ("sys/fs/cgroup/cpu/cpu.cfs_quota_us", "150000\n"),
        ("sys/fs/cgroup/cpu/cpu.cfs_period_us", "100000\n"),
        (
            "sys/fs/cgroup/memory/docker/abc/memory.limit_in_bytes",
            "3221225472\n",
        ),
    ]);
    let facts = read(&root, None);
    assert_eq!(facts, host(Some(1.5), Some(3 << 30)));
    assert_eq!(
        line(&facts).unwrap(),
        "Resources: 1.5 CPUs and 3 GiB memory (cgroup limits; nproc and free show the machine's 24 CPUs and 47 GiB)"
    );
}

#[test]
fn v1_without_limits_shows_the_machine() {
    let root = fake(&[
        ("proc/self/cgroup", "12:memory:/\n4:cpu,cpuacct:/\n"),
        ("sys/fs/cgroup/cpu/cpu.cfs_quota_us", "-1\n"),
        ("sys/fs/cgroup/cpu/cpu.cfs_period_us", "100000\n"),
        (
            "sys/fs/cgroup/memory/memory.limit_in_bytes",
            "9223372036854771712\n",
        ),
    ]);
    let facts = read(&root, None);
    assert_eq!(facts.cpu_quota, None);
    assert_eq!(line(&facts).unwrap(), "Resources: 24 CPUs, 47 GiB memory");
}

#[test]
fn a_hybrid_host_takes_each_fact_from_where_it_is() {
    let root = fake(&[
        (
            "proc/self/cgroup",
            "12:memory:/user.slice\n4:cpu,cpuacct:/user.slice\n0::/\n",
        ),
        ("sys/fs/cgroup/cpu/user.slice/cpu.cfs_quota_us", "200000\n"),
        ("sys/fs/cgroup/cpu/user.slice/cpu.cfs_period_us", "100000\n"),
        (
            "sys/fs/cgroup/memory/user.slice/memory.limit_in_bytes",
            "1073741824\n",
        ),
    ]);
    assert_eq!(read(&root, None), host(Some(2.0), Some(1 << 30)));
}

#[test]
fn a_v2_file_wins_and_a_missing_one_falls_back_to_v1() {
    let root = fake(&[
        ("proc/self/cgroup", "4:cpu:/\n12:memory:/\n0::/\n"),
        ("sys/fs/cgroup/memory.max", "max\n"),
        ("sys/fs/cgroup/cpu/cpu.cfs_quota_us", "100000\n"),
        ("sys/fs/cgroup/cpu/cpu.cfs_period_us", "100000\n"),
        ("sys/fs/cgroup/memory/memory.limit_in_bytes", "1073741824\n"),
    ]);
    assert_eq!(read(&root, None), host(Some(1.0), None));
}

#[test]
fn cpus_allowed_lists_are_counted() {
    for (list, count) in [
        ("0", Some(1)),
        ("0-23", Some(24)),
        ("0-3,8,10-11", Some(7)),
        ("", None),
        ("3-1", None),
        ("0-x", None),
    ] {
        let status = format!("Cpus_allowed_list:\t{list}\n");
        assert_eq!(cpus_allowed(&status), count, "{list:?}");
    }
    assert_eq!(cpus_allowed("Name:\tcat\n"), None);
}

#[test]
fn garbage_in_one_file_loses_only_that_fact() {
    let good = [
        ("proc/self/cgroup", "0::/job\n"),
        ("sys/fs/cgroup/job/cpu.max", "100000 100000\n"),
        ("sys/fs/cgroup/job/memory.max", "2147483648\n"),
    ];
    let mut root = fake(&good);
    root.0.insert("sys/fs/cgroup/job/cpu.max", "lots\n");
    assert_eq!(read(&root, None), host(None, Some(2 << 30)));
    let mut root = fake(&good);
    root.0.insert("sys/fs/cgroup/job/memory.max", "2G\n");
    assert_eq!(read(&root, None), host(Some(1.0), None));
    let mut root = fake(&good);
    root.0
        .insert("proc/self/status", "Cpus_allowed_list:\tall\n");
    assert_eq!(read(&root, None).cpus_visible, None);
    assert_eq!(read(&root, None).cpu_quota, Some(1.0));
    let mut root = fake(&good);
    root.0.insert("proc/meminfo", "MemTotal: lots\n");
    assert_eq!(read(&root, None).memory_total, None);
    assert_eq!(read(&root, None).memory_limit, Some(2 << 30));
    let mut root = fake(&good);
    root.0.insert("proc/self/cgroup", "nonsense");
    assert_eq!(read(&root, None), host(None, None));
}

#[test]
fn only_the_limited_half_is_named_as_a_limit() {
    assert_eq!(
        line(&host(Some(1.0), None)).unwrap(),
        "Resources: 1 CPU (cgroup limit; nproc shows 24), 47 GiB memory"
    );
    assert_eq!(
        line(&host(None, Some(2 << 30))).unwrap(),
        "Resources: 24 CPUs, 2 GiB memory (cgroup limit; free shows 47 GiB)"
    );
}

#[test]
fn a_limit_at_or_above_the_machine_is_no_limit() {
    let at = host(Some(24.0), Some(GIB_47));
    assert_eq!(line(&at).unwrap(), "Resources: 24 CPUs, 47 GiB memory");
    let above = host(Some(32.0), Some(GIB_47 * 2));
    assert_eq!(line(&above).unwrap(), "Resources: 24 CPUs, 47 GiB memory");
}

#[test]
fn numbers_are_rounded_to_their_unit() {
    let facts = |quota: f64, limit: u64| line(&host(Some(quota), Some(limit))).unwrap();
    assert!(
        facts(2.5, 3 * (1 << 30) + (1 << 29)).starts_with("Resources: 2.5 CPUs and 3.5 GiB memory")
    );
    assert!(facts(0.04, 1 << 20).starts_with("Resources: 0.1 CPUs and 1 MiB memory"));
    assert!(
        facts(12.0, 12 * (1 << 30) + (1 << 29)).starts_with("Resources: 12 CPUs and 12 GiB memory")
    );
}

#[test]
fn the_non_linux_line_has_only_cpus() {
    let facts = Resources {
        cpus_visible: Some(8),
        ..Resources::default()
    };
    assert_eq!(line(&facts).unwrap(), "Resources: 8 CPUs");
    let one = Resources {
        cpus_visible: Some(1),
        ..Resources::default()
    };
    assert_eq!(line(&one).unwrap(), "Resources: 1 CPU");
}

#[test]
fn nothing_readable_gives_no_line() {
    assert_eq!(read(&Fake(BTreeMap::new()), None), Resources::default());
    assert_eq!(line(&Resources::default()), None);
    let quota_alone = Resources {
        cpu_quota: Some(1.0),
        memory_limit: Some(1 << 30),
        ..Resources::default()
    };
    assert_eq!(line(&quota_alone), None);
}

#[test]
fn the_machine_s_memory_rounds_down_as_free_does() {
    let total = 47 * (1 << 30) + (7 << 27);
    let facts = |memory_limit| Resources {
        memory_total: Some(total),
        memory_limit,
        ..host(None, None)
    };
    assert_eq!(
        line(&facts(None)).unwrap(),
        "Resources: 24 CPUs, 47 GiB memory"
    );
    assert_eq!(
        line(&facts(Some((2 << 30) - (1 << 25)))).unwrap(),
        "Resources: 24 CPUs, 1.9 GiB memory (cgroup limit; free shows 47 GiB)"
    );
    assert_eq!(
        line(&facts(Some(47 * (1 << 30) + 644_245_094))).unwrap(),
        "Resources: 24 CPUs, 47 GiB memory"
    );
    assert_eq!(
        line(&facts(Some(46 * (1 << 30) + 644_245_094))).unwrap(),
        "Resources: 24 CPUs, 46 GiB memory (cgroup limit; free shows 47 GiB)"
    );
    assert_eq!(
        line(&Resources {
            cpu_quota: Some(1.0),
            ..facts(Some(2 << 30))
        })
        .unwrap(),
        "Resources: 1 CPU and 2 GiB memory (cgroup limits; nproc and free show the machine's 24 CPUs and 47 GiB)"
    );
}

#[test]
fn a_limit_that_prints_as_the_machine_s_figure_is_no_limit() {
    let two = Resources {
        cpus_visible: Some(2),
        cpu_quota: Some(1.96),
        ..host(None, None)
    };
    assert_eq!(line(&two).unwrap(), "Resources: 2 CPUs, 47 GiB memory");
    let near = Resources {
        memory_total: Some(47 * (1 << 30) + (7 << 27)),
        memory_limit: Some(47 * (1 << 30) + (1 << 28)),
        ..host(None, None)
    };
    assert_eq!(line(&near).unwrap(), "Resources: 24 CPUs, 47 GiB memory");
    let below = Resources {
        cpu_quota: Some(1.94),
        ..two
    };
    assert_eq!(
        line(&below).unwrap(),
        "Resources: 1.9 CPUs (cgroup limit; nproc shows 2), 47 GiB memory"
    );
}
