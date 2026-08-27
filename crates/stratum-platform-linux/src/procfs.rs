//! Pure parsers for the `/proc` and `/sys` files [`crate::process`] reads.
//!
//! Split out and kept free of I/O for two reasons. The obvious one is that
//! every branch below can then be asserted from any host against a captured
//! fixture, instead of against whatever CPU the CI runner happened to allocate.
//! The load-bearing one is that this is where the "slower on more cores" bug is
//! actually decided, and that decision deserves to be readable:
//!
//! * **Hyperthreads are not cores.** `nproc` on a 12-core desktop says 24.
//!   Four BLAS-shaped thread pools sized from 24 put 96 runnable threads on 12
//!   physical cores.
//! * **A cgroup quota is not a suggestion.** A researcher's job on a shared
//!   cluster runs in a container with `cpu.max = 200000 100000`. The machine has
//!   96 cores; the job may use 2. Sizing from the machine means the scheduler
//!   throttles us for 98 % of every period, and the symptom is multi-second
//!   stalls that look like our code hanging.
//! * **An affinity mask is not a suggestion either.** `taskset`, SLURM's
//!   `--cpus-per-task` and systemd's `AllowedCPUs` all narrow
//!   `sched_getaffinity` without touching anything else.
//!
//! [`effective_cores`] is the one function that composes all three, and it takes
//! the **minimum**, because each of them is independently a hard ceiling.

use std::collections::BTreeSet;

/// Bytes of memory a large allocation could plausibly get, from `/proc/meminfo`.
///
/// `MemAvailable` and nothing else: the kernel computes it from free pages plus
/// the reclaimable share of the page cache and of the slab, with the watermarks
/// subtracted. Reconstructing it from `MemFree + Cached` — the folklore
/// formula — over-counts by the amount of the cache that is dirty or mapped,
/// which is exactly the amount an out-of-core threshold must not spend.
///
/// `None` when the field is absent (kernels before 3.14), because an estimate
/// that is wrong is worse than no estimate: the threshold derived from it
/// decides whether a 40 GB dataset is streamed or loaded.
#[must_use]
pub fn mem_available(meminfo: &str) -> Option<u64> {
    for line in meminfo.lines() {
        let Some(rest) = line.strip_prefix("MemAvailable:") else {
            continue;
        };
        // "MemAvailable:   16351232 kB" — the unit is always kB, and has been
        // since the field was introduced; parse it anyway rather than assume.
        let mut parts = rest.split_whitespace();
        let value: u64 = parts.next()?.parse().ok()?;
        let scale = match parts.next() {
            Some("kB" | "KB") | None => 1024,
            Some("B") => 1,
            _ => return None,
        };
        return value.checked_mul(scale);
    }
    None
}

/// The cgroup v2 CPU ceiling, from `cpu.max`.
///
/// The file is `"<quota> <period>"` in microseconds, or `"max <period>"` for
/// no limit. A quota of 250000/100000 is 2.5 CPUs, and we round **up**: the
/// scheduler will let 3 threads run, and rounding down to 2 leaves a quarter of
/// the allowance unused on a machine the user is paying for.
#[must_use]
pub fn cgroup_v2_cpus(cpu_max: &str) -> Option<usize> {
    let mut parts = cpu_max.split_whitespace();
    let quota = parts.next()?;
    if quota == "max" {
        return None;
    }
    let quota: u64 = quota.parse().ok()?;
    let period: u64 = parts.next().unwrap_or("100000").parse().ok()?;
    quotient_ceil(quota, period)
}

/// The cgroup v1 CPU ceiling, from `cpu.cfs_quota_us` and `cpu.cfs_period_us`.
/// A quota of `-1` is "no limit", spelled differently from v2's `max` for no
/// reason anyone remembers.
#[must_use]
pub fn cgroup_v1_cpus(quota_us: &str, period_us: &str) -> Option<usize> {
    let quota: i64 = quota_us.trim().parse().ok()?;
    if quota <= 0 {
        return None;
    }
    let period: u64 = period_us.trim().parse().ok()?;
    quotient_ceil(quota.unsigned_abs(), period)
}

fn quotient_ceil(quota: u64, period: u64) -> Option<usize> {
    if period == 0 {
        return None;
    }
    // At least 1: a quota below one full period still permits one runnable
    // thread, and returning 0 would make every downstream `max(1)` a lie about
    // what we measured.
    usize::try_from(quota.div_ceil(period))
        .ok()
        .map(|n| n.max(1))
}

/// Distinct `(physical package, core)` pairs in `/proc/cpuinfo` — the physical
/// core count, with hyperthreads collapsed.
///
/// `/proc/cpuinfo` rather than `/sys/devices/system/cpu/cpu*/topology/`: one
/// open and one read instead of two per logical CPU (192 opens on a dual-socket
/// 48-core box), and the two agree by construction because the kernel fills both
/// from the same topology.
#[must_use]
pub fn cpuinfo_physical_cores(cpuinfo: &str) -> Option<usize> {
    let mut seen: BTreeSet<(u64, u64)> = BTreeSet::new();
    let mut package: Option<u64> = None;
    let mut core: Option<u64> = None;
    let mut processors = 0usize;

    for line in cpuinfo.lines() {
        let Some((key, value)) = line.split_once(':') else {
            // A blank line ends one processor record.
            if line.trim().is_empty() {
                if let (Some(p), Some(c)) = (package.take(), core.take()) {
                    seen.insert((p, c));
                }
            }
            continue;
        };
        let value = value.trim();
        match key.trim() {
            "processor" => {
                processors += 1;
                package = None;
                core = None;
            }
            "physical id" => package = value.parse().ok(),
            "core id" => core = value.parse().ok(),
            _ => {}
        }
    }
    if let (Some(p), Some(c)) = (package, core) {
        seen.insert((p, c));
    }

    if !seen.is_empty() {
        return Some(seen.len());
    }
    // ARM, RISC-V and most VMs publish no `physical id`/`core id` at all. There
    // the processor count IS the core count as far as anything observable goes,
    // and claiming otherwise would be inventing a topology.
    (processors > 0).then_some(processors)
}

/// Parse a kernel CPU list — `"0-7,16,20-23"` — into the number of CPUs it
/// names. Used for `/sys/devices/cpu_core/cpus` (Intel hybrid) and for
/// `Cpus_allowed_list` in `/proc/self/status`.
#[must_use]
pub fn cpu_list_len(list: &str) -> Option<usize> {
    let mut total = 0usize;
    for span in list.trim().split(',').filter(|s| !s.is_empty()) {
        let n = match span.split_once('-') {
            Some((lo, hi)) => {
                let lo: usize = lo.trim().parse().ok()?;
                let hi: usize = hi.trim().parse().ok()?;
                if hi < lo {
                    return None;
                }
                hi - lo + 1
            }
            None => {
                span.trim().parse::<usize>().ok()?;
                1
            }
        };
        total += n;
    }
    (total > 0).then_some(total)
}

/// The performance-core count of a heterogeneous CPU, from each logical CPU's
/// `cpu_capacity` (ARM DynamIQ, and Intel hybrid parts on kernels that publish
/// it).
///
/// `None` when every core reports the same capacity — a homogeneous CPU, where
/// the honest answer to "which cores should I prefer?" is "there is nothing to
/// prefer", exactly as on an Intel Mac.
#[must_use]
pub fn perf_cores_from_capacities(capacities: &[u32]) -> Option<usize> {
    let max = capacities.iter().copied().max()?;
    let min = capacities.iter().copied().min()?;
    if max == min {
        return None;
    }
    Some(capacities.iter().filter(|c| **c == max).count())
}

/// Compose the three independent ceilings. Each one is hard on its own, so the
/// answer is their minimum, floored at 1.
///
/// `physical` is what the topology says, `affinity` what
/// `sched_getaffinity` says, `cgroup` what the container's quota says. The last
/// two are `None` when unrestricted.
#[must_use]
pub fn effective_cores(physical: usize, affinity: Option<usize>, cgroup: Option<usize>) -> usize {
    let mut n = physical;
    if let Some(a) = affinity {
        n = n.min(a);
    }
    if let Some(c) = cgroup {
        n = n.min(c);
    }
    n.max(1)
}

#[cfg(test)]
mod tests {
    use super::*;

    const MEMINFO: &str = "MemTotal:       32633456 kB\n\
                           MemFree:         1043312 kB\n\
                           MemAvailable:   16351232 kB\n\
                           Buffers:          229188 kB\n";

    #[test]
    fn mem_available_is_read_in_kb_and_returned_in_bytes() {
        assert_eq!(mem_available(MEMINFO), Some(16_351_232 * 1024));
    }

    /// Pre-3.14 kernels have no `MemAvailable`. Synthesising one from
    /// `MemFree + Cached` over-counts by the dirty and mapped share of the page
    /// cache, and the out-of-core threshold is derived from this number.
    #[test]
    fn a_kernel_without_the_field_gets_no_estimate_rather_than_a_guess() {
        assert_eq!(
            mem_available("MemTotal: 32633456 kB\nMemFree: 1043312 kB\n"),
            None
        );
    }

    #[test]
    fn cgroup_v2_quota_rounds_up_and_max_means_unlimited() {
        assert_eq!(cgroup_v2_cpus("200000 100000"), Some(2));
        assert_eq!(cgroup_v2_cpus("250000 100000"), Some(3));
        assert_eq!(cgroup_v2_cpus("max 100000"), None);
        // A quota smaller than one period still permits one runnable thread.
        assert_eq!(cgroup_v2_cpus("50000 100000"), Some(1));
        // The period is optional in the wild; 100 ms is the kernel default.
        assert_eq!(cgroup_v2_cpus("400000"), Some(4));
    }

    #[test]
    fn cgroup_v1_spells_unlimited_as_minus_one() {
        assert_eq!(cgroup_v1_cpus("-1", "100000"), None);
        assert_eq!(cgroup_v1_cpus("150000", "100000"), Some(2));
        assert_eq!(cgroup_v1_cpus("garbage", "100000"), None);
        assert_eq!(cgroup_v1_cpus("100000", "0"), None);
    }

    /// A 4-core, 8-thread part. Sizing four thread pools from the 8 is the bug
    /// this whole module exists to prevent.
    #[test]
    fn hyperthreads_collapse_onto_their_physical_core() {
        let cpuinfo = "\
processor\t: 0
physical id\t: 0
core id\t\t: 0

processor\t: 1
physical id\t: 0
core id\t\t: 1

processor\t: 2
physical id\t: 0
core id\t\t: 0

processor\t: 3
physical id\t: 0
core id\t\t: 1
";
        assert_eq!(cpuinfo_physical_cores(cpuinfo), Some(2));
    }

    #[test]
    fn two_sockets_do_not_alias_onto_each_other() {
        let cpuinfo = "\
processor\t: 0
physical id\t: 0
core id\t\t: 0

processor\t: 1
physical id\t: 1
core id\t\t: 0
";
        assert_eq!(cpuinfo_physical_cores(cpuinfo), Some(2));
    }

    /// Most ARM parts and most VMs publish no topology fields. Inventing one
    /// would be worse than reporting what is observable.
    #[test]
    fn a_cpuinfo_without_topology_falls_back_to_the_processor_count() {
        let cpuinfo = "processor\t: 0\nBogoMIPS\t: 108.00\n\nprocessor\t: 1\nBogoMIPS\t: 108.00\n";
        assert_eq!(cpuinfo_physical_cores(cpuinfo), Some(2));
        assert_eq!(cpuinfo_physical_cores(""), None);
    }

    #[test]
    fn cpu_lists_parse_ranges_singletons_and_mixtures() {
        assert_eq!(cpu_list_len("0-7"), Some(8));
        assert_eq!(cpu_list_len("0-7,16,20-23"), Some(13));
        assert_eq!(cpu_list_len("3"), Some(1));
        assert_eq!(cpu_list_len("0-7\n"), Some(8));
        assert_eq!(cpu_list_len(""), None);
        assert_eq!(cpu_list_len("7-0"), None);
        assert_eq!(cpu_list_len("a-b"), None);
    }

    /// An Intel 12th-gen part: 8 P-cores at capacity 1024, 8 E-cores at 620.
    #[test]
    fn a_heterogeneous_cpu_reports_only_its_fast_cores() {
        let mut caps = vec![1024u32; 8];
        caps.extend(std::iter::repeat_n(620u32, 8));
        assert_eq!(perf_cores_from_capacities(&caps), Some(8));
    }

    #[test]
    fn a_homogeneous_cpu_has_nothing_to_prefer() {
        assert_eq!(perf_cores_from_capacities(&[1024, 1024, 1024]), None);
        assert_eq!(perf_cores_from_capacities(&[]), None);
    }

    /// The scenario this exists for: a 96-core cluster node, a job pinned to 8
    /// CPUs by SLURM and quota'd to 2 by the container runtime.
    #[test]
    fn every_ceiling_is_hard_so_the_answer_is_their_minimum() {
        assert_eq!(effective_cores(48, Some(8), Some(2)), 2);
        assert_eq!(effective_cores(48, Some(8), None), 8);
        assert_eq!(effective_cores(48, None, None), 48);
        // Never zero: a caller sizing a thread pool from this must not have to
        // check.
        assert_eq!(effective_cores(0, Some(0), Some(0)), 1);
    }
}
