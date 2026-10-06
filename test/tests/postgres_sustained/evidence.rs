//! Attempt custody and laboratory arithmetic. No result here accepts delivery.

use std::{
    collections::BTreeMap,
    fs::{self, File, OpenOptions},
    io::{self, BufWriter, Write},
    path::{Path, PathBuf},
};

use serde::{Deserialize, Serialize};

pub(crate) const MAX_EVIDENCE_BYTES: u64 = 2 * 1024 * 1024 * 1024;
const MAX_RECORD_BYTES: usize = 1024 * 1024;
const DEADLINE_NS: u64 = 2_000_000_000;
const SEEDS: [u64; 3] = [41001, 41002, 41003];

#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
pub(crate) enum Policy {
    P0,
    P1,
    P2,
    P3,
}

impl Policy {
    pub(crate) const fn predecessor(self) -> Option<Self> {
        match self {
            Self::P0 => None,
            Self::P1 => Some(Self::P0),
            Self::P2 => Some(Self::P1),
            Self::P3 => Some(Self::P2),
        }
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
pub(crate) enum Regime {
    Resident,
    Pressured,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
pub(crate) enum Segment {
    Warmup,
    Steady,
    CatchUp,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
pub(crate) enum Family {
    Jobs,
    Idempotency,
    Webhook,
    WideReplay,
}

const FAMILIES: [Family; 4] = [
    Family::Jobs,
    Family::Idempotency,
    Family::Webhook,
    Family::WideReplay,
];
const REGIMES: [Regime; 2] = [Regime::Resident, Regime::Pressured];
const SEGMENTS: [Segment; 2] = [Segment::Steady, Segment::CatchUp];

const fn arrivals_per_second(family: Family) -> u64 {
    match family {
        Family::Jobs => 30,
        Family::Idempotency => 50,
        Family::Webhook => 20,
        Family::WideReplay => 64,
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Config {
    pub(crate) attempt_id: String,
    pub(crate) policy: Policy,
    pub(crate) regime: Regime,
    pub(crate) repeat: u8,
    pub(crate) seed: u64,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Manifest {
    pub(crate) config: Config,
    pub(crate) source_tree_hash: String,
    pub(crate) executable_sha256: String,
    pub(crate) policy_patch_sha256: String,
    pub(crate) image_digest: String,
    pub(crate) toolchain: String,
    pub(crate) features: Vec<String>,
    pub(crate) target_identity: String,
    pub(crate) effective_inputs: serde_json::Value,
}

/// One process owns this writer; other roles send records to that owner.
#[derive(Debug)]
pub(crate) struct Evidence {
    directory: PathBuf,
    writer: BufWriter<File>,
    remaining: u64,
    failed: bool,
}

impl Evidence {
    pub(crate) fn create(root: &Path, manifest: &Manifest) -> io::Result<Self> {
        let config = &manifest.config;
        if config.attempt_id.is_empty()
            || !config
                .attempt_id
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_'))
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "attempt_id must be a nonempty ASCII identifier",
            ));
        }
        if !(1..=3).contains(&config.repeat) || config.seed != SEEDS[usize::from(config.repeat - 1)]
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "repeat and fixed seed disagree",
            ));
        }
        for value in [
            &manifest.source_tree_hash,
            &manifest.executable_sha256,
            &manifest.policy_patch_sha256,
            &manifest.image_digest,
            &manifest.toolchain,
            &manifest.target_identity,
        ] {
            if value.is_empty() {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "manifest identity is missing",
                ));
            }
        }
        fs::create_dir_all(root)?;
        let used = directory_bytes(root)?;
        let bytes = record_bytes(manifest)?;
        // Four role writers plus the controller may create attempts concurrently.
        // Each receives at most one fifth of the root's remaining capacity;
        // callers must use the same campaign root for every role and cell.
        let remaining = MAX_EVIDENCE_BYTES
            .checked_sub(used)
            .map(|n| n / 5)
            .and_then(|n| n.checked_sub(bytes.len() as u64))
            .ok_or_else(|| io::Error::other("2 GiB evidence bound reached"))?;
        let directory = root.join(&config.attempt_id);
        // Never reopen an attempt, even when it contains only partial evidence.
        fs::create_dir(&directory)?;
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(directory.join("manifest.json"))?;
        file.write_all(&bytes)?;
        file.sync_all()?;
        let mut permissions = file.metadata()?.permissions();
        permissions.set_readonly(true);
        file.set_permissions(permissions)?;
        let writer = BufWriter::new(
            OpenOptions::new()
                .append(true)
                .create_new(true)
                .open(directory.join("events.jsonl"))?,
        );
        File::open(&directory)?.sync_all()?;
        Ok(Self {
            directory,
            writer,
            remaining,
            failed: false,
        })
    }

    pub(crate) fn directory(&self) -> &Path {
        &self.directory
    }

    pub(crate) fn append(&mut self, record: &impl Serialize) -> io::Result<()> {
        if self.failed {
            return Err(io::Error::other(
                "evidence writer failed; preserve invalid attempt",
            ));
        }
        let bytes = record_bytes(record)?;
        let remaining = self
            .remaining
            .checked_sub(bytes.len() as u64)
            .ok_or_else(|| io::Error::other("2 GiB evidence bound reached; stop arrivals"))?;
        self.failed = true;
        self.remaining = remaining;
        self.writer.write_all(&bytes)?;
        // Flush before returning: an acknowledged planned record precedes its effect.
        self.writer.flush()?;
        self.failed = false;
        Ok(())
    }

    pub(crate) fn finish(mut self) -> io::Result<()> {
        self.writer.flush()?;
        self.writer.get_ref().sync_all()?;
        File::open(&self.directory)?.sync_all()
    }
}

fn directory_bytes(path: &Path) -> io::Result<u64> {
    let mut size = 0_u64;
    for entry in fs::read_dir(path)? {
        let entry = entry?;
        let metadata = entry.metadata()?;
        if entry.file_type()?.is_symlink() {
            return Err(io::Error::other("evidence root contains a symlink"));
        }
        let bytes = if metadata.is_dir() {
            directory_bytes(&entry.path())?
        } else {
            metadata.len()
        };
        size = size
            .checked_add(bytes)
            .ok_or_else(|| io::Error::other("evidence size overflow"))?;
    }
    Ok(size)
}

fn record_bytes(record: &impl Serialize) -> io::Result<Vec<u8>> {
    // A bounded serializer prevents accidental full-payload logging from allocating
    // an arbitrarily large JSON record before the on-disk limit is checked.
    struct Bounded(Vec<u8>);
    impl Write for Bounded {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            if bytes.len() > MAX_RECORD_BYTES.saturating_sub(self.0.len()) {
                return Err(io::Error::other("evidence record exceeds 1 MiB"));
            }
            self.0.extend_from_slice(bytes);
            Ok(bytes.len())
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }
    let mut writer = Bounded(Vec::new());
    serde_json::to_writer(&mut writer, record)?;
    writer.write_all(b"\n")?;
    Ok(writer.0)
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub(crate) enum Outcome {
    Expected,
    Unexpected,
    TimedOut,
    NotStarted,
    Unknown,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub(crate) struct OperationRecord {
    pub(crate) id: u64,
    pub(crate) family: Family,
    pub(crate) segment: Segment,
    pub(crate) planned_ns: u64,
    pub(crate) started_ns: Option<u64>,
    pub(crate) completed_ns: u64,
    pub(crate) outcome: Outcome,
    pub(crate) expected: String,
    pub(crate) actual: String,
}

/// Emit Planned before dispatch and Started before calling the native adapter.
/// Completed also covers operations that never acquired an in-flight slot.
#[derive(Debug, Serialize)]
#[serde(tag = "event", rename_all = "snake_case")]
pub(crate) enum OperationEvent<'a> {
    Planned {
        id: u64,
        family: Family,
        segment: Segment,
        planned_ns: u64,
        expected: &'a str,
    },
    Started {
        id: u64,
        started_ns: u64,
    },
    Completed {
        record: &'a OperationRecord,
    },
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
pub(crate) struct Distribution {
    pub(crate) scheduled: u64,
    pub(crate) expected_within_deadline: u64,
    pub(crate) not_started: u64,
    pub(crate) timed_out: u64,
    pub(crate) unknown: u64,
    pub(crate) unexpected: u64,
    pub(crate) p50_ns: u64,
    pub(crate) p95_ns: u64,
    pub(crate) p99_ns: u64,
    pub(crate) max_ns: u64,
}

impl Distribution {
    fn accounted(&self, scheduled: u64) -> bool {
        self.scheduled == scheduled
            && u128::from(self.expected_within_deadline)
                + u128::from(self.not_started)
                + u128::from(self.timed_out)
                + u128::from(self.unknown)
                + u128::from(self.unexpected)
                == u128::from(scheduled)
            && self.p50_ns <= self.p95_ns
            && self.p95_ns <= self.p99_ns
            && self.p99_ns <= self.max_ns
            && (self.expected_within_deadline == self.scheduled || self.max_ns >= DEADLINE_NS)
    }
}

/// Summarizes one family and segment. Retained failed arrivals receive at least
/// the client deadline in the distribution, so overload cannot lower its tail.
pub(crate) fn summarize_operations<'a>(
    records: impl IntoIterator<Item = &'a OperationRecord>,
) -> Distribution {
    let mut result = Distribution::default();
    let mut latencies = Vec::new();
    for record in records {
        result.scheduled += 1;
        let elapsed = record.completed_ns.saturating_sub(record.planned_ns);
        let ordered = record
            .started_ns
            .is_some_and(|start| start >= record.planned_ns && start <= record.completed_ns);
        let success = record.outcome == Outcome::Expected && ordered && elapsed <= DEADLINE_NS;
        if success {
            result.expected_within_deadline += 1;
        }
        match record.outcome {
            Outcome::NotStarted => result.not_started += 1,
            Outcome::TimedOut => result.timed_out += 1,
            Outcome::Unknown => result.unknown += 1,
            Outcome::Unexpected => result.unexpected += 1,
            Outcome::Expected if !success => result.unexpected += 1,
            Outcome::Expected => {}
        }
        latencies.push(if success {
            elapsed
        } else {
            elapsed.max(DEADLINE_NS)
        });
    }
    latencies.sort_unstable();
    result.p50_ns = percentile(&latencies, 50);
    result.p95_ns = percentile(&latencies, 95);
    result.p99_ns = percentile(&latencies, 99);
    result.max_ns = latencies.last().copied().unwrap_or_default();
    result
}

fn percentile(values: &[u64], percent: usize) -> u64 {
    if values.is_empty() {
        return 0;
    }
    values[(values.len() * percent).div_ceil(100) - 1]
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub(crate) struct Qualification {
    pub(crate) retained_bytes: u64,
    pub(crate) shared_buffers_bytes: u64,
    pub(crate) pg_memory_limit_bytes: u64,
    pub(crate) relation_toast_hits: u64,
    pub(crate) relation_toast_reads: u64,
    pub(crate) distinct_full_body_bytes: u64,
    pub(crate) live_body_cohort_bytes: u64,
    pub(crate) container_block_read_bytes: u64,
    pub(crate) native_inserts: u64,
    pub(crate) native_updates: u64,
    pub(crate) native_deletes: u64,
    pub(crate) full_body_content_checked: bool,
    pub(crate) jobs_and_receipts_observed: bool,
    pub(crate) maintenance_overlapped: bool,
    pub(crate) inputs_unchanged: bool,
    pub(crate) records_complete: bool,
    pub(crate) uncontended_host: bool,
    pub(crate) monotonic_clock: bool,
}

impl Qualification {
    pub(crate) fn unmet(&self, regime: Regime) -> Vec<String> {
        let mut gaps = Vec::new();
        let total = u128::from(self.relation_toast_hits) + u128::from(self.relation_toast_reads);
        let mut check = |holds: bool, reason: &str| {
            if !holds {
                gaps.push(reason.to_owned());
            }
        };
        match regime {
            Regime::Resident => {
                check(
                    self.retained_bytes <= 320 * 1024 * 1024
                        && u128::from(self.retained_bytes) * 100
                            <= u128::from(self.shared_buffers_bytes) * 65,
                    "resident footprint exceeds 320 MiB or 65% of shared_buffers",
                );
                check(
                    total > 0 && u128::from(self.relation_toast_hits) * 100 >= total * 99,
                    "resident relation and TOAST hit fraction below 99% or absent",
                );
                check(
                    self.distinct_full_body_bytes > 0,
                    "resident full-body read coverage missing",
                );
            }
            Regime::Pressured => {
                check(
                    self.pg_memory_limit_bytes > 0
                        && u128::from(self.retained_bytes)
                            >= u128::from(self.pg_memory_limit_bytes) * 3,
                    "pressured footprint below three times PG memory",
                );
                check(
                    self.distinct_full_body_bytes >= 1536 * 1024 * 1024
                        && self.live_body_cohort_bytes > 0
                        && u128::from(self.distinct_full_body_bytes) * 100
                            >= u128::from(self.live_body_cohort_bytes) * 70,
                    "pressured full-body coverage below 1.5 GiB or 70%",
                );
                check(
                    total > 0 && u128::from(self.relation_toast_reads) * 100 >= total * 5,
                    "pressured relation and TOAST miss fraction below 5% or absent",
                );
                check(
                    self.container_block_read_bytes >= 256 * 1024 * 1024,
                    "pressured container block reads below 256 MiB",
                );
            }
        }
        check(
            self.native_inserts > 0 && self.native_updates > 0 && self.native_deletes > 0,
            "native insert/update/delete churn missing",
        );
        for (holds, reason) in [
            (
                self.full_body_content_checked,
                "full-body content not checked",
            ),
            (
                self.jobs_and_receipts_observed,
                "ordinary jobs or receipts traffic missing",
            ),
            (self.maintenance_overlapped, "maintenance overlap missing"),
            (
                self.inputs_unchanged,
                "schema/settings/image/resources changed",
            ),
            (
                self.records_complete,
                "operation or telemetry records incomplete",
            ),
            (self.uncontended_host, "uncontrolled co-tenant saturation"),
            (self.monotonic_clock, "clock discontinuity"),
        ] {
            check(holds, reason);
        }
        gaps
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub(crate) struct Backlog {
    pub(crate) initial: u64,
    pub(crate) final_count: u64,
    pub(crate) delay_p95_ms: u64,
    pub(crate) delay_max_ms: u64,
    pub(crate) completed_cycle_troughs: Vec<u64>,
    pub(crate) initial_cohort_size: u64,
    pub(crate) initial_cohort_remaining_120s: u64,
    pub(crate) initial_cohort_remaining_150s: u64,
    pub(crate) drain_90_percent_ms: Option<u64>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub(crate) struct SegmentReport {
    pub(crate) segment: Segment,
    pub(crate) qualification: Qualification,
    pub(crate) families: BTreeMap<Family, Distribution>,
    pub(crate) first_minute: BTreeMap<Family, Distribution>,
    pub(crate) backlog: BTreeMap<Family, Backlog>,
    pub(crate) correctness_violations: u64,
    pub(crate) hidden_retries: u64,
    pub(crate) maximum_sample_age_ms: Option<u64>,
    pub(crate) all_expected_populations_observed: bool,
    pub(crate) sampling_deadline_failures: u64,
    pub(crate) recovery_readable: bool,
    pub(crate) owners_stopped: bool,
    pub(crate) first_minute_contention_ns: Option<u64>,
}

impl SegmentReport {
    pub(crate) fn unmet(&self, regime: Regime) -> Vec<String> {
        let mut gaps = self.qualification.unmet(regime);
        if self.correctness_violations > 0 {
            gaps.push("correctness violation".into());
        }
        if self.hidden_retries > 0 {
            gaps.push("hidden retries".into());
        }
        let scheduled: u128 = self
            .families
            .values()
            .map(|d| u128::from(d.scheduled))
            .sum();
        let useful: u128 = self
            .families
            .values()
            .map(|d| u128::from(d.expected_within_deadline))
            .sum();
        if scheduled == 0 || useful * 1000 < scheduled * 999 {
            gaps.push("ordinary useful work below 99.9% of scheduled operations".into());
        }
        for family in FAMILIES {
            match self.families.get(&family) {
                Some(d)
                    if d.accounted(arrivals_per_second(family) * 150)
                        && d.p95_ns <= 100_000_000
                        && d.p99_ns <= 250_000_000 => {}
                _ => gaps.push(format!(
                    "{family:?}: ordinary latency criterion unmet or distribution missing"
                )),
            }
            if !self
                .first_minute
                .get(&family)
                .is_some_and(|d| d.accounted(arrivals_per_second(family) * 60))
            {
                gaps.push(format!("{family:?}: first-minute distribution missing"));
            }
        }
        for family in [Family::Jobs, Family::Idempotency, Family::Webhook] {
            let Some(b) = self.backlog.get(&family) else {
                gaps.push(format!("{family:?}: independent backlog inventory missing"));
                continue;
            };
            if self.segment == Segment::Steady {
                if b.delay_p95_ms > 120_000
                    || b.delay_max_ms > 150_000
                    || b.final_count > b.initial.saturating_add(2400)
                {
                    gaps.push(format!(
                        "{family:?}: steady backlog delay/count criterion unmet"
                    ));
                }
                if b.completed_cycle_troughs.len() < 2
                    || b.completed_cycle_troughs
                        .windows(2)
                        .any(|w| u128::from(w[1]) * 5 > u128::from(w[0]) * 6 + 2500)
                {
                    gaps.push(format!(
                        "{family:?}: successive completed-cycle trough criterion unmet"
                    ));
                }
            } else if self.segment == Segment::CatchUp
                && (b.initial_cohort_size != 10_000
                    || b.initial_cohort_remaining_120s > 1000
                    || b.initial_cohort_remaining_150s != 0
                    || !b.drain_90_percent_ms.is_some_and(|ms| ms <= 120_000))
            {
                gaps.push(format!(
                    "{family:?}: initial 10000-row cohort drain criterion unmet"
                ));
            }
        }
        if !self.all_expected_populations_observed
            || !self.maximum_sample_age_ms.is_some_and(|ms| ms <= 90_000)
            || self.sampling_deadline_failures != 0
        {
            gaps.push("population sample coverage/age/deadline criterion unmet".into());
        }
        if !self.recovery_readable || !self.owners_stopped {
            gaps.push("recovery readability or joined owner stop not observed".into());
        }
        gaps
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub(crate) struct CellReport {
    pub(crate) config: Config,
    #[serde(default)]
    pub(crate) manifest: Option<Manifest>,
    pub(crate) segments: Vec<SegmentReport>,
    pub(crate) invalid_reason: Option<String>,
}

impl CellReport {
    pub(crate) fn unmet(&self) -> Vec<String> {
        let mut gaps = Vec::new();
        if let Some(reason) = &self.invalid_reason {
            gaps.push(format!("invalid attempt: {reason}"));
        }
        for segment in SEGMENTS {
            let matches: Vec<_> = self
                .segments
                .iter()
                .filter(|s| s.segment == segment)
                .collect();
            if matches.len() != 1 {
                gaps.push(format!("{segment:?}: missing or duplicate segment"));
                continue;
            }
            gaps.extend(
                matches[0]
                    .unmet(self.config.regime)
                    .into_iter()
                    .map(|reason| format!("{segment:?}: {reason}")),
            );
        }
        gaps
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub(crate) struct Cell {
    pub(crate) policy: Policy,
    pub(crate) regime: Regime,
    pub(crate) repeat: u8,
    pub(crate) seed: u64,
}

pub(crate) fn screening_order() -> Vec<Cell> {
    let mut cells = Vec::new();
    for (regime, policies) in [
        (
            Regime::Resident,
            [Policy::P0, Policy::P1, Policy::P2, Policy::P3],
        ),
        (
            Regime::Pressured,
            [Policy::P3, Policy::P2, Policy::P1, Policy::P0],
        ),
    ] {
        cells.extend(policies.into_iter().map(|policy| Cell {
            policy,
            regime,
            repeat: 1,
            seed: SEEDS[0],
        }));
    }
    cells
}

pub(crate) fn confirmation_order(candidate: Policy) -> Vec<Cell> {
    let candidate = if candidate == Policy::P0 {
        Policy::P1
    } else {
        candidate
    };
    let predecessor = candidate.predecessor().unwrap_or(Policy::P0);
    let mut policies = vec![candidate, predecessor];
    if predecessor != Policy::P0 {
        policies.push(Policy::P0);
    }
    let mut result = Vec::new();
    for repeat in [2_u8, 3] {
        let regimes = if repeat == 2 {
            [Regime::Pressured, Regime::Resident]
        } else {
            policies.reverse();
            REGIMES
        };
        for regime in regimes {
            result.extend(policies.iter().map(|&policy| Cell {
                policy,
                regime,
                repeat,
                seed: SEEDS[usize::from(repeat - 1)],
            }));
        }
    }
    result
}

fn find_cell(
    cells: &[CellReport],
    policy: Policy,
    regime: Regime,
    repeat: u8,
) -> Option<&CellReport> {
    if !(1..=3).contains(&repeat) {
        return None;
    }
    let mut found = cells.iter().filter(|c| {
        c.config.policy == policy
            && c.config.regime == regime
            && c.config.repeat == repeat
            && c.invalid_reason.is_none()
    });
    let cell = found.next()?;
    if found.next().is_some() || cell.config.seed != SEEDS[usize::from(repeat - 1)] {
        return None;
    }
    Some(cell)
}

fn relative_holds(candidate: &CellReport, baseline: &CellReport) -> bool {
    SEGMENTS.into_iter().all(|segment| {
        let Some(c) = candidate.segments.iter().find(|s| s.segment == segment) else {
            return false;
        };
        let Some(b) = baseline.segments.iter().find(|s| s.segment == segment) else {
            return false;
        };
        FAMILIES.into_iter().all(|family| {
            [
                (&c.families, &b.families),
                (&c.first_minute, &b.first_minute),
            ]
            .into_iter()
            .all(|(cm, bm)| {
                let (Some(c), Some(b)) = (cm.get(&family), bm.get(&family)) else {
                    return false;
                };
                u128::from(c.p99_ns) * 5
                    <= (u128::from(b.p99_ns) * 6)
                        .max(u128::from(b.p99_ns.saturating_add(5_000_000)) * 5)
            })
        })
    })
}

fn qualifies(cells: &[CellReport], policy: Policy, repeats: &[u8]) -> bool {
    REGIMES.into_iter().all(|regime| {
        repeats.iter().all(|&repeat| {
            let Some(cell) = find_cell(cells, policy, regime, repeat) else {
                return false;
            };
            cell.unmet().is_empty()
                && (policy == Policy::P0
                    || find_cell(cells, Policy::P0, regime, repeat)
                        .is_some_and(|base| relative_holds(cell, base)))
        })
    })
}

/// One dimension must improve in every matched pair; a different favorable
/// family in each repeat cannot be combined into a material-benefit claim.
fn justified(cells: &[CellReport], policy: Policy, comparator: Policy, repeats: &[u8]) -> bool {
    REGIMES.into_iter().all(|regime| {
        let pairs: Option<Vec<_>> = repeats
            .iter()
            .map(|&repeat| {
                Some((
                    find_cell(cells, policy, regime, repeat)?,
                    find_cell(cells, comparator, regime, repeat)?,
                ))
            })
            .collect();
        let Some(pairs) = pairs else {
            return false;
        };
        if pairs.iter().any(|(_, b)| {
            b.segments.iter().any(|s| {
                !s.qualification.unmet(regime).is_empty()
                    || FAMILIES.iter().any(|family| {
                        !s.families
                            .get(family)
                            .is_some_and(|d| d.accounted(arrivals_per_second(*family) * 150))
                            || !s
                                .first_minute
                                .get(family)
                                .is_some_and(|d| d.accounted(arrivals_per_second(*family) * 60))
                    })
            }) || SEGMENTS
                .iter()
                .any(|segment| b.segments.iter().filter(|s| s.segment == *segment).count() != 1)
        }) {
            return false;
        }
        // A consistent, identical missed joint criterion is an alternative to speed.
        let failures = pairs[0].1.unmet();
        if failures.iter().any(|failure| {
            pairs.iter().all(|(c, b)| {
                c.unmet().is_empty()
                    && b.unmet().contains(failure)
                    && b.segments
                        .iter()
                        .all(|s| s.qualification.unmet(regime).is_empty())
            })
        }) {
            return true;
        }
        for segment in SEGMENTS {
            let reports: Option<Vec<_>> = pairs
                .iter()
                .map(|(c, b)| {
                    Some((
                        c.segments.iter().find(|s| s.segment == segment)?,
                        b.segments.iter().find(|s| s.segment == segment)?,
                    ))
                })
                .collect();
            let Some(reports) = reports else {
                return false;
            };
            for family in FAMILIES {
                let values: Option<Vec<_>> = reports
                    .iter()
                    .map(|(c, b)| {
                        Some((
                            c.families.get(&family)?.p99_ns,
                            b.families.get(&family)?.p99_ns,
                        ))
                    })
                    .collect();
                if values.is_some_and(|values| material(&values, 10, 2_000_000)) {
                    return true;
                }
            }
            for family in [Family::Jobs, Family::Idempotency, Family::Webhook] {
                let values: Option<Vec<_>> = reports
                    .iter()
                    .map(|(c, b)| {
                        Some((
                            c.backlog.get(&family)?.drain_90_percent_ms?,
                            b.backlog.get(&family)?.drain_90_percent_ms?,
                        ))
                    })
                    .collect();
                if segment == Segment::CatchUp
                    && values.is_some_and(|values| material(&values, 20, 0))
                {
                    return true;
                }
            }
            let values: Option<Vec<_>> = reports
                .iter()
                .map(|(c, b)| Some((c.first_minute_contention_ns?, b.first_minute_contention_ns?)))
                .collect();
            if values.is_some_and(|values| material(&values, 20, 0)) {
                return true;
            }
        }
        false
    })
}

fn material(values: &[(u64, u64)], percent: u64, absolute: u64) -> bool {
    if values.is_empty() || values.iter().any(|(c, b)| c >= b || *b == 0) {
        return false;
    }
    let mut improvements: Vec<_> = values.iter().map(|(c, b)| b - c).collect();
    let mut relative: Vec<_> = values
        .iter()
        .map(|(c, b)| u128::from(b - c) * 1_000_000 / u128::from(*b))
        .collect();
    improvements.sort_unstable();
    relative.sort_unstable();
    // Require each retained comparison to remain on the accepted side of the
    // boundary; overlapping the boundary is inconclusive, not a favorable mean.
    values.iter().all(|(c, b)| {
        b - c >= absolute && u128::from(b - c) * 100 >= u128::from(*b) * u128::from(percent)
    }) && improvements[improvements.len() / 2] >= absolute
        && relative[relative.len() / 2] >= u128::from(percent) * 10_000
}

#[derive(Debug, Serialize)]
pub(crate) struct Decision {
    pub(crate) policy: Option<Policy>,
    pub(crate) gaps: Vec<String>,
}

pub(crate) fn shortlist(cells: &[CellReport]) -> Decision {
    let gaps = matrix_gaps(cells);
    if !gaps.is_empty() {
        return Decision { policy: None, gaps };
    }
    if cells.iter().filter(|c| c.invalid_reason.is_some()).count() > 2 {
        return Decision {
            policy: None,
            gaps: vec!["matrix exceeds two invalid-cell replacements".into()],
        };
    }
    if screening_order()
        .iter()
        .any(|c| find_cell(cells, c.policy, c.regime, 1).is_none())
    {
        return Decision {
            policy: None,
            gaps: vec!["screening matrix incomplete or duplicate valid attempts".into()],
        };
    }
    for policy in [Policy::P1, Policy::P2, Policy::P3] {
        let predecessor = policy.predecessor().unwrap_or(Policy::P0);
        if qualifies(cells, policy, &[1])
            && justified(cells, policy, Policy::P0, &[1])
            && (predecessor == Policy::P0 || justified(cells, policy, predecessor, &[1]))
        {
            return Decision {
                policy: Some(policy),
                gaps: Vec::new(),
            };
        }
    }
    if qualifies(cells, Policy::P0, &[1]) {
        return Decision {
            policy: Some(Policy::P0),
            gaps: vec!["confirm P0 against P1 before retaining unchanged policy".into()],
        };
    }
    let mut gaps = vec!["no qualified screening shortlist and P0 does not qualify; retain negative result and reopen laboratory design".into()];
    gaps.extend(cell_gaps(cells));
    Decision { policy: None, gaps }
}

pub(crate) fn select_policy(cells: &[CellReport], shortlisted: Policy) -> Decision {
    let screen = shortlist(cells);
    if screen.policy != Some(shortlisted) {
        return Decision {
            policy: None,
            gaps: vec![
                "confirmation candidate does not match the deterministic screening shortlist"
                    .into(),
            ],
        };
    }
    let mut expected = screening_order();
    expected.extend(confirmation_order(shortlisted));
    if cells
        .iter()
        .any(|cell| !expected.iter().any(|slot| matches_cell(&cell.config, slot)))
    {
        return Decision {
            policy: None,
            gaps: vec!["cell outside the fixed screening/confirmation matrix".into()],
        };
    }
    if confirmation_order(shortlisted)
        .iter()
        .any(|c| find_cell(cells, c.policy, c.regime, c.repeat).is_none())
    {
        return Decision {
            policy: None,
            gaps: vec![
                "confirmation matrix incomplete or duplicate valid attempts; no policy selected"
                    .into(),
            ],
        };
    }
    if cells.iter().filter(|c| c.invalid_reason.is_some()).count() > 2 {
        return Decision {
            policy: None,
            gaps: vec!["matrix exceeds two invalid-cell replacements".into()],
        };
    }
    let confirmed = |policy| qualifies(cells, policy, &[1, 2, 3]);
    let supported = |policy: Policy| {
        let predecessor = policy.predecessor().unwrap_or(Policy::P0);
        confirmed(policy)
            && justified(cells, policy, Policy::P0, &[1, 2, 3])
            && (predecessor == Policy::P0 || justified(cells, policy, predecessor, &[1, 2, 3]))
    };
    if shortlisted != Policy::P0 && supported(shortlisted) {
        return Decision {
            policy: Some(shortlisted),
            gaps: Vec::new(),
        };
    }
    if let Some(predecessor) = shortlisted.predecessor().filter(|p| *p != Policy::P0) {
        if supported(predecessor) {
            return Decision {
                policy: Some(predecessor),
                gaps: Vec::new(),
            };
        }
    }
    if confirmed(Policy::P0) {
        return Decision {
            policy: Some(Policy::P0),
            gaps: vec!["alternative lacks consistent supported benefit; retain baseline".into()],
        };
    }
    let mut gaps = vec!["three qualified P0 repeats or supported candidate comparisons unavailable; reopen laboratory design".into()];
    gaps.extend(cell_gaps(cells));
    Decision { policy: None, gaps }
}

fn cell_gaps(cells: &[CellReport]) -> Vec<String> {
    cells
        .iter()
        .flat_map(|cell| {
            cell.unmet().into_iter().map(|gap| {
                format!(
                    "{} {:?} {:?} repeat {}: {gap}",
                    cell.config.attempt_id,
                    cell.config.policy,
                    cell.config.regime,
                    cell.config.repeat
                )
            })
        })
        .collect()
}

/// Pure report generation: supplied observations remain visible, and absent
/// confirmation is a plan rather than an inferred policy decision.
pub(crate) fn selection_report(cells: &[CellReport]) -> serde_json::Value {
    let mut gaps = matrix_gaps(cells);
    let screen = shortlist(cells);
    let confirmation = screen.policy.map(confirmation_order).unwrap_or_default();
    let mut expected = screening_order();
    expected.extend_from_slice(&confirmation);
    for cell in cells {
        if !expected
            .iter()
            .any(|scheduled| matches_cell(&cell.config, scheduled))
        {
            gaps.push(format!(
                "{}: cell outside the deterministic screening/confirmation plan",
                cell.config.attempt_id
            ));
        }
    }
    let missing: Vec<_> = expected
        .iter()
        .filter(|cell| find_cell(cells, cell.policy, cell.regime, cell.repeat).is_none())
        .copied()
        .collect();
    let complete_screening = screening_order()
        .iter()
        .all(|cell| find_cell(cells, cell.policy, cell.regime, 1).is_some());
    let decision = if gaps.is_empty() && complete_screening && missing.is_empty() {
        screen.policy.map(|policy| select_policy(cells, policy))
    } else {
        None
    };
    let status = if !gaps.is_empty() {
        "inadmissible_matrix"
    } else if !complete_screening {
        "screening_incomplete"
    } else if screen.policy.is_none() {
        "screening_failed"
    } else if !missing.is_empty() {
        "confirmation_required"
    } else if decision.as_ref().is_some_and(|d| d.policy.is_some()) {
        "policy_selected"
    } else {
        "selection_inconclusive"
    };
    let screening: Vec<_> = screening_order()
        .iter()
        .map(|cell| {
            let attempts: Vec<_> = cells
                .iter()
                .filter(|report| matches_cell(&report.config, cell))
                .map(|report| {
                    serde_json::json!({
                        "attempt_id": report.config.attempt_id,
                        "invalid_reason": report.invalid_reason,
                        "unmet": report.unmet(),
                    })
                })
                .collect();
            serde_json::json!({"cell": cell, "attempts": attempts})
        })
        .collect();
    let disposition: Vec<_> = [Policy::P0, Policy::P1, Policy::P2, Policy::P3]
        .into_iter()
        .map(|policy| {
            let confirmed = REGIMES.iter().all(|&regime| {
                [1, 2, 3]
                    .into_iter()
                    .all(|repeat| find_cell(cells, policy, regime, repeat).is_some())
            });
            serde_json::json!({
                "policy": policy,
                "screening_qualifies": complete_screening.then(|| qualifies(cells, policy, &[1])),
                "confirmation_scheduled": confirmation.iter().any(|cell| cell.policy == policy),
                "three_repeats_qualify": confirmed.then(|| qualifies(cells, policy, &[1,2,3])),
                "selected": decision.as_ref().and_then(|d| d.policy) == Some(policy),
            })
        })
        .collect();
    serde_json::json!({
        "scope": "policy-matrix arithmetic from supplied observations; not delivery acceptance",
        "status": status,
        "matrix_gaps": gaps,
        "screening": screening,
        "shortlist": screen,
        "confirmation_plan": confirmation,
        "missing_cells": missing,
        "selection": decision,
        "policy_dispositions": disposition,
        "invalid_attempts": cells.iter().filter(|cell| cell.invalid_reason.is_some()).collect::<Vec<_>>(),
        "retained_runs": cells,
        "per_run_ranges": run_ranges(cells),
        "matched_comparisons": screen.policy.map(|policy| comparison_reports(cells, policy)),
        "percentile_method": "nearest-rank within each run; min/median/max of retained run statistics, never a pooled percentile",
    })
}

fn matches_cell(config: &Config, cell: &Cell) -> bool {
    config.policy == cell.policy
        && config.regime == cell.regime
        && config.repeat == cell.repeat
        && config.seed == cell.seed
}

fn matrix_gaps(cells: &[CellReport]) -> Vec<String> {
    let mut gaps = Vec::new();
    let mut identities = std::collections::BTreeSet::new();
    let mut valid_slots = std::collections::BTreeSet::new();
    let mut regime_inputs: BTreeMap<Regime, &Manifest> = BTreeMap::new();
    let mut policy_sources: BTreeMap<Policy, &Manifest> = BTreeMap::new();
    if cells.len() > 22 {
        gaps.push("more than 20 main cells plus two invalid-cell replacement attempts".into());
    }
    if cells.iter().filter(|c| c.invalid_reason.is_some()).count() > 2 {
        gaps.push("more than two invalid-cell replacements".into());
    }
    if cells.iter().filter(|c| c.invalid_reason.is_none()).count() > 20 {
        gaps.push("more than 20 valid main cells".into());
    }
    for cell in cells {
        let config = &cell.config;
        match &cell.manifest {
            None => gaps.push(format!(
                "{}: immutable manifest provenance missing",
                config.attempt_id
            )),
            Some(manifest) => {
                if serde_json::to_value(&manifest.config).ok() != serde_json::to_value(config).ok()
                {
                    gaps.push(format!(
                        "{}: manifest and report identity disagree",
                        config.attempt_id
                    ));
                }
                if let Err(gap) = inventory_inputs(manifest) {
                    gaps.push(format!("{}: {gap}", config.attempt_id));
                }
                if manifest.source_tree_hash.is_empty()
                    || manifest.executable_sha256.is_empty()
                    || manifest.policy_patch_sha256.is_empty()
                    || manifest.image_digest.is_empty()
                    || manifest.toolchain.is_empty()
                    || manifest.target_identity.is_empty()
                    || manifest.effective_inputs["workload_hash"]
                        .as_str()
                        .is_none_or(str::is_empty)
                {
                    gaps.push(format!(
                        "{}: source/executable/workload provenance missing",
                        config.attempt_id
                    ));
                }
                if let Some(prior) = regime_inputs.insert(config.regime, manifest) {
                    if fixed_inputs(prior) != fixed_inputs(manifest) {
                        gaps.push(format!(
                            "{}: frozen workload/inventory/resources changed within regime",
                            config.attempt_id
                        ));
                    }
                }
                if let Some(prior) = policy_sources.insert(config.policy, manifest) {
                    if prior.source_tree_hash != manifest.source_tree_hash
                        || prior.executable_sha256 != manifest.executable_sha256
                        || prior.policy_patch_sha256 != manifest.policy_patch_sha256
                    {
                        gaps.push(format!(
                            "{}: same-policy source or executable changed between cells",
                            config.attempt_id
                        ));
                    }
                }
            }
        }
        if config.attempt_id.is_empty() || !identities.insert(&config.attempt_id) {
            gaps.push(format!(
                "empty or repeated attempt identity: {}",
                config.attempt_id
            ));
        }
        if !(1..=3).contains(&config.repeat) || config.seed != SEEDS[usize::from(config.repeat - 1)]
        {
            gaps.push(format!("{}: repeat/fixed-seed mismatch", config.attempt_id));
        }
        if cell
            .invalid_reason
            .as_ref()
            .is_some_and(|reason| reason.trim().is_empty())
        {
            gaps.push(format!(
                "{}: invalid attempt has no diagnosed reason",
                config.attempt_id
            ));
        }
        if cell.invalid_reason.is_none()
            && !valid_slots.insert((config.policy, config.regime, config.repeat))
        {
            gaps.push(format!(
                "{}: duplicate valid cell; a criterion failure cannot be rerun away",
                config.attempt_id
            ));
        }
    }
    gaps
}

fn fixed_inputs(manifest: &Manifest) -> Json {
    let mut inputs = manifest.effective_inputs.clone();
    if let Some(object) = inputs.as_object_mut() {
        object.remove("preflight_free_disk_bytes");
    }
    serde_json::json!({"inputs":inputs,"target":manifest.target_identity,"image":manifest.image_digest,"features":manifest.features,"toolchain":manifest.toolchain})
}

fn run_ranges(cells: &[CellReport]) -> Vec<serde_json::Value> {
    let mut groups: BTreeMap<_, Vec<_>> = BTreeMap::new();
    for cell in cells.iter().filter(|cell| cell.invalid_reason.is_none()) {
        for segment in &cell.segments {
            for (window, distributions) in [
                ("full_segment", &segment.families),
                ("first_minute", &segment.first_minute),
            ] {
                for (&family, distribution) in distributions {
                    groups
                        .entry((
                            cell.config.policy,
                            cell.config.regime,
                            segment.segment,
                            family,
                            window,
                        ))
                        .or_default()
                        .push((&cell.config, distribution));
                }
            }
        }
    }
    groups.into_iter().map(|((policy, regime, segment, family, window), mut runs)| {
            runs.sort_by(|(left, _), (right, _)| {
                (left.repeat, &left.attempt_id).cmp(&(right.repeat, &right.attempt_id))
            });
        let per_run: Vec<_> = runs.iter().map(|(config, distribution)| serde_json::json!({"attempt_id": config.attempt_id, "repeat": config.repeat, "seed": config.seed, "distribution": distribution})).collect();
        serde_json::json!({
            "policy": policy, "regime": regime, "segment": segment, "family": family, "window": window,
            "runs": per_run,
            "p50_ns": range(runs.iter().map(|(_, d)| d.p50_ns)),
            "p95_ns": range(runs.iter().map(|(_, d)| d.p95_ns)),
            "p99_ns": range(runs.iter().map(|(_, d)| d.p99_ns)),
            "max_ns": range(runs.iter().map(|(_, d)| d.max_ns)),
        })
    }).collect()
}

fn range(values: impl Iterator<Item = u64>) -> serde_json::Value {
    let mut values: Vec<_> = values.collect();
    values.sort_unstable();
    let Some(&minimum) = values.first() else {
        return serde_json::Value::Null;
    };
    let middle = values.len() / 2;
    let median = if values.len().is_multiple_of(2) {
        values[middle - 1] + (values[middle] - values[middle - 1]) / 2
    } else {
        values[middle]
    };
    serde_json::json!({"min": minimum, "median": median, "max": values.last()})
}

fn comparison_reports(cells: &[CellReport], shortlisted: Policy) -> Vec<Json> {
    let mut comparisons = Vec::new();
    let candidate = if shortlisted == Policy::P0 {
        Policy::P1
    } else {
        shortlisted
    };
    let mut policies = vec![candidate];
    if let Some(predecessor) = candidate.predecessor().filter(|p| *p != Policy::P0) {
        policies.push(predecessor);
    }
    for policy in policies {
        let mut comparators = vec![Policy::P0];
        if let Some(predecessor) = policy.predecessor().filter(|p| *p != Policy::P0) {
            comparators.push(predecessor);
        }
        for comparator in comparators {
            let mut pairs = Vec::new();
            for regime in REGIMES {
                for repeat in 1..=3 {
                    let candidate = find_cell(cells, policy, regime, repeat);
                    let baseline = find_cell(cells, comparator, regime, repeat);
                    pairs.push(serde_json::json!({
                        "regime": regime, "repeat": repeat, "seed": SEEDS[usize::from(repeat - 1)],
                        "candidate": candidate, "comparator": baseline,
                        "relative_interference_holds": candidate.zip(baseline).map(|(c,b)| relative_holds(c,b)),
                        "single_pair_benefit": candidate.zip(baseline).map(|_| pair_benefit(cells, policy, comparator, regime, repeat)),
                    }));
                }
            }
            comparisons.push(serde_json::json!({
                "policy": policy, "comparator": comparator, "pairs": pairs,
                "three_pair_material_or_consistent_criterion_benefit": justified(cells, policy, comparator, &[1,2,3]),
            }));
        }
    }
    comparisons
}

fn pair_benefit(
    cells: &[CellReport],
    policy: Policy,
    comparator: Policy,
    regime: Regime,
    repeat: u8,
) -> Json {
    let (Some(candidate), Some(baseline)) = (
        find_cell(cells, policy, regime, repeat),
        find_cell(cells, comparator, regime, repeat),
    ) else {
        return Json::Null;
    };
    let mut values = Vec::new();
    for segment in SEGMENTS {
        let (Some(c), Some(b)) = (
            candidate.segments.iter().find(|s| s.segment == segment),
            baseline.segments.iter().find(|s| s.segment == segment),
        ) else {
            continue;
        };
        for family in FAMILIES {
            if let (Some(c), Some(b)) = (c.families.get(&family), b.families.get(&family)) {
                values.push(serde_json::json!({"segment":segment,"family":family,"measure":"p99_ns", "candidate":c.p99_ns,"comparator":b.p99_ns,"material":material(&[(c.p99_ns,b.p99_ns)],10,2_000_000)}));
            }
        }
        for family in [Family::Jobs, Family::Idempotency, Family::Webhook] {
            if segment == Segment::CatchUp {
                if let (Some(c), Some(b)) = (
                    c.backlog.get(&family).and_then(|b| b.drain_90_percent_ms),
                    b.backlog.get(&family).and_then(|b| b.drain_90_percent_ms),
                ) {
                    values.push(serde_json::json!({"segment":segment,"family":family,"measure":"drain_90_percent_ms","candidate":c,"comparator":b,"material":material(&[(c,b)],20,0)}));
                }
            }
        }
        if let (Some(c), Some(b)) = (c.first_minute_contention_ns, b.first_minute_contention_ns) {
            values.push(serde_json::json!({"segment":segment,"measure":"first_minute_contention_ns","candidate":c,"comparator":b,"material":material(&[(c,b)],20,0)}));
        }
    }
    serde_json::json!({"measurements":values,"candidate_unmet":candidate.unmet(),"comparator_unmet":baseline.unmet()})
}

type Json = serde_json::Value;
type Assembly<T> = Result<T, String>;
const SEGMENT_NS: u64 = 150_000_000_000;
const ROLES: [&str; 4] = ["service0", "service1", "worker0", "worker1"];
const RETAINED: [&str; 3] = [
    "background_jobs",
    "http_idempotency_records",
    "webhook_receipts",
];

fn inventory_inputs(manifest: &Manifest) -> Assembly<()> {
    if number(&manifest.effective_inputs, "inventory_seed")? != SEEDS[0]
        || number(&manifest.effective_inputs, "inventory_generation")? != 3
        || number(&manifest.effective_inputs, "live_bodies")? == 0
        || number(&manifest.effective_inputs, "idempotency_rows")? == 0
        || number(&manifest.effective_inputs, "receipt_rows")? == 0
        || number(&manifest.effective_inputs, "jobs_rows")? == 0
    {
        return Err("frozen inventory requires seed 41001, three committed generations, and a nonempty live cohort".into());
    }
    if !(1..=3).contains(&manifest.config.repeat)
        || manifest.config.seed != SEEDS[usize::from(manifest.config.repeat - 1)]
    {
        return Err("repeat and fixed arrival seed disagree".into());
    }
    Ok(())
}

/// Estimates only: measured marginal allocation is not a guarantee of the
/// allocator's future high-water mark. Final physical qualification is retained.
pub(crate) fn inventory_report(
    manifest: &Manifest,
    relations: &Json,
    phases: &Json,
    adjustments: u64,
    elapsed_ms: u64,
    cohort_hash: &str,
) -> Assembly<Json> {
    inventory_inputs(manifest)?;
    let names = [
        "background_jobs",
        "http_idempotency_records",
        "webhook_receipts",
    ];
    let read_sizes = |rows: &Json| -> Assembly<[u64; 3]> {
        let mut sizes = [0; 3];
        for (index, name) in names.iter().enumerate() {
            let row = rows
                .as_array()
                .ok_or("phase relations missing")?
                .iter()
                .find(|row| row["relation"] == *name)
                .ok_or("phase family missing")?;
            sizes[index] = number(row, "total_bytes")?;
        }
        Ok(sizes)
    };
    let phase_names = [
        "eligibility",
        "replacements",
        "wide",
        "idempotency",
        "jobs",
        "webhooks",
    ];
    let snapshots = phases
        .as_array()
        .ok_or("physical phase snapshots missing")?;
    if snapshots.len() != phase_names.len() {
        return Err("physical phase snapshots incomplete".into());
    }
    let mut sizes = Vec::new();
    for (snapshot, name) in snapshots.iter().zip(phase_names) {
        if snapshot["phase"] != name {
            return Err("physical phase snapshots out of order".into());
        }
        sizes.push(read_sizes(&snapshot["relations"])?);
    }
    let mut costs = [[0_u64; 3]; 5];
    for phase in 0..5 {
        for family in 0..3 {
            costs[phase][family] = sizes[phase + 1][family]
                .checked_sub(sizes[phase][family])
                .ok_or("relation contracted during inventory; marginal sizing unavailable")?;
        }
    }
    // Count order is wide, ordinary idempotency, jobs, webhook receipts.
    let fields = [
        "live_bodies",
        "idempotency_rows",
        "jobs_rows",
        "receipt_rows",
    ];
    let mut current = [0; 4];
    for (index, field) in fields.iter().enumerate() {
        current[index] = number(&manifest.effective_inputs, field)?;
    }
    let counts_json = |counts: [u64; 4]| serde_json::json!({"live_bodies":counts[0],"idempotency_rows":counts[1],"jobs_rows":counts[2],"receipt_rows":counts[3]});
    let estimate = |counts: [u64; 4], reserve: bool| -> Assembly<[u64; 3]> {
        let mut result = [0; 3];
        for family in 0..3 {
            let mut value = u128::from(sizes[0][family]);
            let factor = if reserve { 5_u128 } else { 2 };
            value += (u128::from(costs[0][family]) * factor).div_ceil(2);
            for cohort in 0..4 {
                let factor = if reserve && cohort != 0 { 5_u128 } else { 2 };
                value +=
                    (u128::from(costs[cohort + 1][family]) * u128::from(counts[cohort]) * factor)
                        .div_ceil(u128::from(current[cohort]) * 2);
            }
            result[family] = u64::try_from(value).map_err(|_| "sizing estimate overflow")?;
        }
        Ok(result)
    };
    let sum = |values: [u64; 3]| -> Assembly<u64> {
        values.into_iter().try_fold(0_u64, |sum, value| {
            sum.checked_add(value)
                .ok_or_else(|| "sizing sum overflow".into())
        })
    };
    let shares_ok = |values: [u64; 3], total: u64| {
        total > 0
            && values
                .into_iter()
                .zip([(15_u128, 35_u128), (55, 75), (0, 20)])
                .all(|(value, (low, high))| {
                    u128::from(value) * 100 >= u128::from(total) * low
                        && u128::from(value) * 100 <= u128::from(total) * high
                })
    };
    let (minimum, target, maximum, step, min_wide, max_wide) = match manifest.config.regime {
        Regime::Resident => (
            96 * 1024 * 1024,
            128 * 1024 * 1024,
            160 * 1024 * 1024,
            8 * 1024 * 1024,
            2,
            u64::MAX,
        ),
        // Pressure is qualified per 150-second measured segment, not by the
        // separate 60-second rate probe. 9600 wide arrivals can cover at least
        // 70% of at most 13714 bodies; 3072 supplies the required 1.5 GiB.
        Regime::Pressured => (
            3 * 1024_u64.pow(3),
            7 * 1024_u64.pow(3) / 2,
            4 * 1024_u64.pow(3),
            128 * 1024 * 1024,
            3072,
            13_714,
        ),
    };
    let measured = read_sizes(relations)?;
    let measured_total = sum(measured)?;
    let current_prediction = estimate(current, true)?;
    let current_initial = estimate(current, false)?;
    let admissible = |counts: [u64; 4], predicted: [u64; 3]| -> Assembly<bool> {
        let total = sum(predicted)?;
        Ok((minimum..=maximum).contains(&total)
            && shares_ok(predicted, total)
            && counts[0] >= min_wide
            && counts[0] <= max_wide)
    };
    let mut proposed = None;
    let mut predicted = current_prediction;
    let mut initial = current_initial;
    let mut gaps = Vec::new();
    if !admissible(current, predicted)? && adjustments == 0 {
        // A fixed finite arithmetic selection, never an additional probe or a
        // retry loop. Prefer midpoint, then nearest accepted physical target.
        'choose: for offset in [0_i8, 1, -1, 2, -2, 3, -3, 4, -4] {
            let goal = if offset >= 0 {
                target + u64::from(offset.unsigned_abs()) * step
            } else {
                target - u64::from(offset.unsigned_abs()) * step
            };
            if !(minimum..=maximum).contains(&goal) {
                continue;
            }
            for receipts in [current[3], 40] {
                for idem in [current[1].div_ceil(40) * 40, 40] {
                    let mut candidate = [0, idem, 0, receipts];
                    let base = estimate(candidate, true)?;
                    let sized = |budget: u64,
                                 cohort: usize,
                                 family: usize,
                                 factor: u128,
                                 quantum: u64,
                                 min: u64|
                     -> Assembly<u64> {
                        let cost = u128::from(costs[cohort + 1][family]) * factor;
                        if cost == 0 {
                            return Err("cohort has no observed allocation for sizing".into());
                        }
                        let count = u128::from(budget) * u128::from(current[cohort]) * 2 / cost;
                        let count = u64::try_from(count).map_err(|_| "sizing count overflow")?;
                        Ok((count / quantum * quantum).max(min))
                    };
                    candidate[2] =
                        sized((goal * 30 / 100).saturating_sub(base[0]), 2, 0, 5, 200, 200)?;
                    let base = estimate(candidate, true)?;
                    let body_budget = goal.saturating_sub(sum(base)?);
                    candidate[0] = sized(body_budget, 0, 1, 2, 2, min_wide)?.min(max_wide);
                    // A large regime fills remaining body storage with ordinary
                    // identities once the physical read coverage bounds wide rows.
                    let with_wide = estimate(candidate, true)?;
                    if sum(with_wide)? < goal && candidate[0] == max_wide {
                        let extra = sized(goal - sum(with_wide)?, 1, 1, 5, 40, 0)?;
                        candidate[1] = candidate[1]
                            .checked_add(extra)
                            .ok_or("sizing count overflow")?;
                    }
                    let next = estimate(candidate, true)?;
                    let next_initial = estimate(candidate, false)?;
                    if admissible(candidate, next)? {
                        proposed = Some(candidate);
                        predicted = next;
                        initial = next_initial;
                        break 'choose;
                    }
                }
            }
        }
        if proposed.is_none() {
            gaps.push("no measured count estimate fits retained floors, three-round reserve, family shares and read coverage".to_owned());
        }
    } else if !admissible(current, predicted)? {
        gaps.push("single adjustment consumed; estimate unresolved and actual final qualification remains required".to_owned());
    }
    let mut storage = Vec::new();
    for (index, name) in names.iter().enumerate() {
        storage.push(serde_json::json!({"relation":name,"bytes":measured[index],"total_bytes":measured_total}));
    }
    let estimate_ready = admissible(proposed.unwrap_or(current), predicted)?;
    Ok(
        serde_json::json!({"scope":"measured preparation and one reset estimate; actual final churn and physical qualification required","status":if proposed.is_some(){"adjustment_proposed"}else if adjustments>0 || estimate_ready{"inventory_prepared"}else{"inventory_unqualified"},"current_counts":counts_json(current),"proposed_counts":proposed.map(counts_json),"family_storage":storage,"retained_bytes":measured_total,"target_retained_bytes":target,"phase_snapshots":phases,"sizing_model":{"fixed_eligibility_bytes":sizes[0],"cohort_allocation_bytes":costs,"cohort_order":["replacements","wide","idempotency","jobs","webhooks"],"family_order":names,"mutable_allocation_reserve_numerator":5,"mutable_allocation_reserve_denominator":2,"assumptions":"marginal bytes scale with cohort counts; three half-cohort admissions reserve 2.5 initial mutable allocations; unchanged wide bodies reserve 1; allocator reuse/compression and actual final qualification are not predicted guarantees"},"predicted_after_churn":{"within_estimated_bounds":estimate_ready,"family_bytes":predicted,"total_bytes":sum(predicted)?,"initial_family_bytes":initial,"initial_total_bytes":sum(initial)?},"adjustments_used":adjustments,"seed_started_unix_ms":number(&manifest.effective_inputs,"seed_started_unix_ms")?,"seed_attempt_started_unix_ms":number(&manifest.effective_inputs,"seed_attempt_started_unix_ms")?,"seed_elapsed_before_attempt_ms":number(&manifest.effective_inputs,"seed_elapsed_before_attempt_ms")?,"seed_elapsed_ms":elapsed_ms,"seed_cohort_hash":cohort_hash,"gaps":gaps}),
    )
}

/// Qualifications for one physical seed, before the lifecycle owner freezes or
/// clones it. A size failure supplies the measured input to its single reset.
pub(crate) fn seed_report(manifest: &Manifest, events: &[Json]) -> Assembly<Json> {
    inventory_inputs(manifest)?;
    let selected: Vec<_> = events.iter().collect();
    let receipt = &one_event(&selected, "preconditioning")?["receipt"];
    let identity = one_event(&selected, "seed_identity")?;
    for field in [
        "seed_started_unix_ms",
        "seed_attempt_started_unix_ms",
        "seed_elapsed_before_attempt_ms",
    ] {
        if number(identity, field)? != number(&manifest.effective_inputs, field)? {
            return Err("seed clock identity changed".into());
        }
    }
    if number(identity, "seed_started_unix_ms")?
        != number(&manifest.effective_inputs, "seed_started_unix_ms")?
        || number(identity, "seed_elapsed_ms")? > 1_500_000
        || string(identity, "seed_cohort_hash")?.is_empty()
    {
        return Err("cumulative seed clock or durable cohort identity missing".into());
    }
    let rounds = receipt["rounds"]
        .as_array()
        .ok_or("preconditioning rounds missing")?;
    let mut gaps = Vec::new();
    if rounds.len() != 3 || number(receipt, "frozen_generation")? != 3 {
        gaps.push("three committed churn generations not observed".to_owned());
    }
    if number(receipt, "native_elapsed_ms")? < 900_000
        || number(receipt, "total_elapsed_ms")? > 1_500_000
    {
        gaps.push("preconditioning requires at least 15 minutes native work within the 25 minute seed bound".into());
    }
    if number(receipt, "wide_native_replays")? == 0
        || string(receipt, "fixture_actions")?.is_empty()
    {
        gaps.push("native wide work or separate fixture action declaration missing".into());
    }
    for (recorded, input) in [
        ("seed", "inventory_seed"),
        ("generation", "inventory_generation"),
        ("live", "live_bodies"),
        ("idempotency_rows", "idempotency_rows"),
        ("receipt_rows", "receipt_rows"),
        ("jobs_rows", "jobs_rows"),
    ] {
        if number(&receipt["inventory"], recorded)? != number(&manifest.effective_inputs, input)? {
            gaps.push(format!(
                "preconditioning {recorded} differs from frozen manifest"
            ));
        }
    }
    if manifest.effective_inputs["workload_hash"]
        .as_str()
        .is_none_or(str::is_empty)
    {
        gaps.push("frozen workload hash missing".into());
    }
    let idem = number(&manifest.effective_inputs, "idempotency_rows")?;
    let jobs = number(&manifest.effective_inputs, "jobs_rows")?;
    let receipts = number(&manifest.effective_inputs, "receipt_rows")?;
    for (field, population, divisor) in [
        ("idempotency_replaced_per_round", idem, 2),
        ("idempotency_deleted_reinserted_per_round", idem, 5),
        ("receipt_deleted_readmitted_per_round", receipts, 2),
        ("jobs_enqueued_per_round", jobs, 2),
    ] {
        if u128::from(number(receipt, field)?) * divisor < u128::from(population) {
            gaps.push(format!(
                "{field}: declared logical churn below accepted fraction"
            ));
        }
    }
    let before = serde_json::json!({"relations":receipt["before"]});
    let initial_bytes = relation_sum(&before, "total_bytes")?;
    let mut previous = before.clone();
    let mut round_deltas = Vec::new();
    for (index, round) in rounds.iter().enumerate() {
        if number(round, "generation")? != index as u64 + 1 {
            gaps.push("churn generations missing, duplicated or out of order".into());
        }
        let current = serde_json::json!({"relations":round["relations"]});
        let mut dml = BTreeMap::new();
        for field in ["insert", "update", "delete"] {
            let observed = delta(
                relation_sum(&previous, field)?,
                relation_sum(&current, field)?,
                field,
            )?;
            if observed == 0 {
                gaps.push(format!("round {}: no observed {field} delta", index + 1));
            }
            dml.insert(field, observed);
        }
        if number(&round["native"], "idempotency_deletions_confirmed")? < idem / 5 + 3240
            || u128::from(number(&round["native"], "jobs_retired")?) * 5 < u128::from(jobs)
            || number(&round["native"], "retired_jobs_remaining")? != 0
            || number(&round["native"], "replacement_fixture_mutated")? < 8100
            || number(&round["native"], "replacement_fixture_deleted_reinserted")? < 3240
            || u128::from(number(round, "receipt_deletions_confirmed")?) * 2 < u128::from(receipts)
        {
            gaps.push(format!(
                "round {}: confirmed deletion/replacement or retired-job inventory incomplete",
                index + 1
            ));
        }
        round_deltas.push(serde_json::json!({"generation":index+1,"observed_relation_deltas":dml,"native_receipt":round["native"],"receipt_deletions_confirmed":round["receipt_deletions_confirmed"]}));
        previous = current;
    }
    let samples = timed_events(&selected, "database_sample")?;
    let after = samples
        .last()
        .ok_or("final physical relation sample missing")?
        .1;
    let final_bytes = relation_sum(after, "total_bytes")?;
    let bounds = match manifest.config.regime {
        Regime::Resident => (96 * 1024 * 1024, 160 * 1024 * 1024),
        Regime::Pressured => (3 * 1024_u64.pow(3), 4 * 1024_u64.pow(3)),
    };
    if final_bytes < bounds.0 || final_bytes > bounds.1 {
        gaps.push(format!(
            "physical seed footprint {final_bytes} outside {}..{} bytes",
            bounds.0, bounds.1
        ));
    }
    let relations = after["relations"]
        .as_array()
        .ok_or("final relations missing")?;
    let mut shares = Vec::new();
    for (name, target) in [
        ("background_jobs", 25_u128),
        ("http_idempotency_records", 65),
        ("webhook_receipts", 10),
    ] {
        let row = relations
            .iter()
            .find(|row| row["relation"] == name)
            .ok_or("family relation missing")?;
        let bytes = number(row, "total_bytes")?;
        let lower = target.saturating_sub(10);
        let upper = target + 10;
        if final_bytes == 0
            || u128::from(bytes) * 100 < u128::from(final_bytes) * lower
            || u128::from(bytes) * 100 > u128::from(final_bytes) * upper
        {
            gaps.push(format!(
                "{name}: physical storage share outside {lower}..{upper}%"
            ));
        }
        shares.push(serde_json::json!({"relation":name,"bytes":bytes,"total_bytes":final_bytes,"target_percent":target,"tolerance_percentage_points":10}));
    }
    let recovery = one_event(&selected, "recovery_checked")?;
    if !boolean(recovery, "readable")?
        || number(recovery, "correctness_violations")? != 0
        || number(recovery, "hidden_retries")? != 0
    {
        gaps.push("seed native custody/recovery check failed".into());
    }
    if !boolean(one_event(&selected, "owners_stopped")?, "complete")? {
        gaps.push("seed owners not stopped before freeze".into());
    }
    Ok(
        serde_json::json!({"scope":"physical seed qualification; no policy or pressure result and no freeze/clone authority", "status":if gaps.is_empty() {"seed_qualified"} else {"seed_unqualified"},"gaps":gaps,"manifest":manifest,"initial_bytes":initial_bytes,"final_bytes":final_bytes,"family_storage":shares,"initial_relations":receipt["before"],"final_relations":after["relations"],"rounds":round_deltas,"preconditioning_receipt":receipt,"seed_cohort_hash":one_event(&selected,"seed_identity")?["seed_cohort_hash"],"seed_started_unix_ms":one_event(&selected,"seed_identity")?["seed_started_unix_ms"],"seed_elapsed_ms":one_event(&selected,"seed_identity")?["seed_elapsed_ms"],"seed_attempt_started_unix_ms":identity["seed_attempt_started_unix_ms"],"seed_elapsed_before_attempt_ms":identity["seed_elapsed_before_attempt_ms"],"freeze_status":"pending lifecycle owner quiescence, immutable export and clone readback"}),
    )
}

/// Report scheduled operations in minute windows, including failed arrivals.
/// The final 30-second main-cell window remains explicitly shorter.
pub(crate) fn segment_series(events: &[Json], segment: Segment, seconds: u64) -> Assembly<Json> {
    if ![60, 120, 150, 1200].contains(&seconds) {
        return Err("unsupported fixed laboratory window".into());
    }
    let selected: Vec<_> = events
        .iter()
        .filter(|event| {
            event
                .get("segment")
                .or_else(|| event.pointer("/record/segment"))
                == Some(&serde_json::json!(segment))
        })
        .collect();
    let records = operation_receipts(segment, &selected, seconds)?;
    let mut minutes = Vec::new();
    for start in (0..seconds).step_by(60) {
        let end = (start + 60).min(seconds);
        let families: BTreeMap<_, _> = FAMILIES
            .into_iter()
            .map(|family| {
                (
                    family,
                    summarize_operations(records.iter().filter(|r| {
                        r.family == family
                            && r.planned_ns >= start * 1_000_000_000
                            && r.planned_ns < end * 1_000_000_000
                    })),
                )
            })
            .collect();
        minutes
            .push(serde_json::json!({"start_seconds":start,"end_seconds":end,"families":families}));
    }
    let mut costs: BTreeMap<String, Vec<u64>> = BTreeMap::new();
    let mut first_pass = BTreeMap::new();
    for event in named_events(&selected, "native_trace") {
        let fields = &event["fields"];
        let message = fields["message"].as_str().unwrap_or_default();
        let subject = match message {
            "postgres_cleanup_pass_finished" => string(fields, "cleanup")?,
            "postgres_maintenance_observation_finished" => string(fields, "population")?,
            _ => continue,
        };
        let elapsed = std::time::Duration::try_from_secs_f64(
            fields["elapsed_seconds"]
                .as_f64()
                .ok_or("native duration absent")?,
        )
        .map_err(|_| "native duration invalid")?;
        let elapsed = u64::try_from(elapsed.as_nanos()).map_err(|_| "native duration overflow")?;
        let role = string(event, "role")?;
        costs
            .entry(format!("{role}/{message}/{subject}"))
            .or_default()
            .push(elapsed);
        if message == "postgres_cleanup_pass_finished" {
            let start = number(event, "elapsed_ns")?.saturating_sub(elapsed);
            first_pass
                .entry(format!("{role}/{subject}"))
                .and_modify(|value: &mut u64| *value = (*value).min(start))
                .or_insert(start);
        }
    }
    let costs: BTreeMap<_, _> = costs
        .into_iter()
        .map(|(name, values)| {
            (
                name,
                serde_json::json!({"attempts":values.len(),"elapsed_ns":range(values.into_iter())}),
            )
        })
        .collect();
    Ok(
        serde_json::json!({"segment":segment,"duration_seconds":seconds,"minutes":minutes,"replacement_physical_outcomes":records.iter().filter(|r| r.expected == "Replace").fold(BTreeMap::<&str,u64>::new(), |mut counts,r| { *counts.entry(r.actual.as_str()).or_default() += 1; counts }),"native_attempt_costs":costs,"first_cleanup_pass_offset_ns":first_pass,"scope":"per-window scheduled-operation distributions and observed native duration range; not pooled percentiles"}),
    )
}

/// Raw window arithmetic shared by the fixed probe and observer calibration.
/// These diagnostic runs never supply a fourth policy repeat.
pub(crate) fn assemble_window(
    manifest: &Manifest,
    events: &[Json],
    duration_seconds: u64,
) -> Assembly<Json> {
    inventory_inputs(manifest)?;
    if ![60, 120].contains(&duration_seconds) || manifest.config.policy != Policy::P0 {
        return Err("probe/calibration requires P0 and the fixed 60/120 second window".into());
    }
    let selected: Vec<_> = events
        .iter()
        .filter(|event| {
            event
                .get("segment")
                .or_else(|| event.pointer("/record/segment"))
                == Some(&serde_json::json!(Segment::Warmup))
        })
        .collect();
    let records = operation_receipts(Segment::Warmup, &selected, duration_seconds)?;
    let resources = timed_events(&selected, "resource_sample")?;
    let counters = timed_events(&selected, "database_counters")?;
    require_window(
        &resources,
        "resource_sample",
        true,
        duration_seconds * 1_000_000_000,
    )?;
    require_window(
        &counters,
        "database_counters",
        true,
        duration_seconds * 1_000_000_000,
    )?;
    let mut driver_cpu_max = 0_f64;
    for (_, event) in &resources {
        let cpu = event["driver_cpu_fraction"]
            .as_f64()
            .filter(|v| v.is_finite() && *v >= 0.0)
            .ok_or("driver CPU observation missing or invalid")?;
        driver_cpu_max = driver_cpu_max.max(cpu);
        if string(event, "inputs_hash")? != string(&manifest.effective_inputs, "inputs_hash")?
            || !boolean(event, "uncontended_host")?
            || !boolean(event, "clock_valid")?
        {
            return Err(
                "diagnostic window has changed inputs, host contention or a clock discontinuity"
                    .into(),
            );
        }
    }
    let families: BTreeMap<_, _> = FAMILIES
        .into_iter()
        .map(|family| {
            (
                family,
                summarize_operations(records.iter().filter(|r| r.family == family)),
            )
        })
        .collect();
    let mut gaps = ordinary_gaps(&families, duration_seconds);
    if duration_seconds == 60 && driver_cpu_max >= 0.70 {
        gaps.push("fixed-rate probe driver CPU is not below 70% of observed capacity".into());
    }
    let per_minute = segment_series(events, Segment::Warmup, duration_seconds)?;
    let mut sampling_failures = 0_f64;
    let instrumentation = manifest.effective_inputs["instrumentation"].as_str();
    let mut metrics = Vec::new();
    for role in ROLES {
        let role_events: Vec<_> = selected
            .iter()
            .copied()
            .filter(|e| e["role"].as_str() == Some(role))
            .collect();
        let samples = timed_events(&role_events, "metrics")?;
        require_window(
            &samples,
            &format!("{role} metrics"),
            false,
            duration_seconds * 1_000_000_000,
        )?;
        let baseline = one_event(&role_events, "metrics_baseline")?;
        let populations = if role.starts_with("worker") {
            ["jobs", "failed_jobs"]
        } else {
            ["http_idempotency", "webhook_receipts"]
        };
        for population in populations {
            sampling_failures += diagnostic_failures(
                role,
                population,
                baseline,
                &samples,
                instrumentation == Some("observers"),
            )?;
        }
        metrics.extend(samples.into_iter().map(|(_, sample)| sample));
    }
    let recovery = one_event(&selected, "recovery_checked")?;
    if !boolean(recovery, "readable")?
        || number(recovery, "correctness_violations")? != 0
        || number(recovery, "hidden_retries")? != 0
    {
        gaps.push("diagnostic recovery/correctness/hidden-retry criterion unmet".into());
    }
    if !boolean(one_event(&selected, "owners_stopped")?, "complete")? {
        gaps.push("diagnostic owners did not reach stopped state".into());
    }
    Ok(serde_json::json!({
        "scope":"fixed-rate diagnostic; not a policy repeat or delivery acceptance",
        "manifest":manifest,"duration_seconds":duration_seconds,"families":families,
        "per_minute":per_minute,"resources":resources.iter().map(|(_,e)| *e).collect::<Vec<_>>(),
        "database_counters":counters.iter().map(|(_,e)| *e).collect::<Vec<_>>(),"metrics":metrics,
        "ordinary_gaps":gaps,"driver_cpu_max_fraction":driver_cpu_max,"sampling_failures":sampling_failures,
    }))
}

fn diagnostic_failures(
    role: &str,
    population: &str,
    baseline: &Json,
    samples: &[(u64, &Json)],
    observers: bool,
) -> Assembly<f64> {
    let labels = [("population", population)];
    let failed_labels = [("population", population), ("outcome", "failed")];
    let baseline_text = string(baseline, "text")?;
    let baseline_failed = metric(
        baseline_text,
        "postgres_maintenance_observations_total",
        &failed_labels,
    )?;
    let mut previous = baseline_failed;
    let baseline_success = metric(
        baseline_text,
        "postgres_maintenance_last_success_timestamp_seconds",
        &labels,
    )?
    .unwrap_or(0.0);
    let mut previous_success = baseline_success;
    let mut succeeded = false;
    for (_, sample) in samples {
        let text = string(sample, "text")?;
        let observed = metric(
            text,
            "postgres_maintenance_observations_total",
            &failed_labels,
        )?;
        match observed {
            Some(value) if value < previous.unwrap_or(0.0) => {
                return Err(format!("{role}/{population}: observer counter reset"));
            }
            Some(value) => previous = Some(value),
            None if observers && previous.is_some() => {
                return Err(format!(
                    "{role}/{population}: observer failure counter disappeared after initialization"
                ));
            }
            None => {}
        }
        if !observers {
            continue;
        }
        let last = metric(
            text,
            "postgres_maintenance_last_success_timestamp_seconds",
            &labels,
        )?;
        if let Some(last) = last.filter(|last| *last > baseline_success) {
            let now = number(sample, "observed_unix_ms")? as f64 / 1000.0;
            if last < previous_success || last > now + 0.001 || now - last > 90.0 {
                return Err(format!(
                    "{role}/{population}: observer timestamp regressed, future or stale"
                ));
            }
            previous_success = last;
            succeeded = true;
        } else if succeeded {
            return Err(format!(
                "{role}/{population}: last-good timestamp disappeared"
            ));
        }
    }
    if observers && (!succeeded || previous.is_none()) {
        return Err(format!(
            "{role}/{population}: successful population or final failure counter missing"
        ));
    }
    Ok(previous.unwrap_or(0.0) - baseline_failed.unwrap_or(0.0))
}

fn ordinary_gaps(families: &BTreeMap<Family, Distribution>, seconds: u64) -> Vec<String> {
    let mut gaps = Vec::new();
    let mut useful = 0_u128;
    for family in FAMILIES {
        match families.get(&family) {
            Some(d) if d.accounted(seconds * arrivals_per_second(family)) => {
                useful += u128::from(d.expected_within_deadline);
                if d.p95_ns > 100_000_000 || d.p99_ns > 250_000_000 {
                    gaps.push(format!("{family:?}: ordinary latency criterion unmet"));
                }
            }
            _ => gaps.push(format!(
                "{family:?}: complete fixed-rate accounting unavailable"
            )),
        }
    }
    if useful * 1000 < u128::from(seconds) * 164 * 999 {
        gaps.push("ordinary useful work below 99.9%".into());
    }
    gaps
}

/// Input is the four `assemble_window` results, in the prescribed execution order.
pub(crate) fn calibration_report(runs: &[Json]) -> Json {
    let result = calibration_comparisons(runs);
    match result {
        Ok(comparisons) => {
            let reopen = comparisons
                .iter()
                .any(|comparison| comparison["reopen"] == true);
            serde_json::json!({"scope":"observer overhead diagnostic; no speedup or delivery acceptance claim", "status":if reopen {"reopen_observer_design"} else {"calibration_criteria_met"},"comparisons":comparisons,"runs":runs,"variance":"one run per instrumentation/regime; run distributions and per-minute variation retained; no statistical repeat estimate"})
        }
        Err(gap) => {
            serde_json::json!({"scope":"observer overhead diagnostic; no policy repeat", "status":"calibration_incomplete","gap":gap,"runs":runs})
        }
    }
}

fn calibration_comparisons(runs: &[Json]) -> Assembly<Vec<Json>> {
    let order = [
        (Regime::Resident, "foundation"),
        (Regime::Resident, "observers"),
        (Regime::Pressured, "observers"),
        (Regime::Pressured, "foundation"),
    ];
    if runs.len() != order.len() {
        return Err("calibration requires exactly four fixed two-minute runs".into());
    }
    let mut manifests = Vec::new();
    let mut identities = std::collections::BTreeSet::new();
    for (run, (regime, instrumentation)) in runs.iter().zip(order) {
        let manifest: Manifest = serde_json::from_value(run["manifest"].clone())
            .map_err(|e| format!("calibration manifest: {e}"))?;
        inventory_inputs(&manifest)?;
        if number(run, "duration_seconds")? != 120
            || manifest.config.policy != Policy::P0
            || manifest.config.regime != regime
            || manifest.config.repeat != 1
            || string(&manifest.effective_inputs, "instrumentation")? != instrumentation
            || !identities.insert(manifest.config.attempt_id.clone())
        {
            return Err(
                "calibration policy/order/duration/identity differs from fixed plan".into(),
            );
        }
        let gaps = run["ordinary_gaps"]
            .as_array()
            .ok_or("calibration ordinary criteria missing")?;
        if !gaps.is_empty() {
            return Err(format!(
                "{}: calibration ordinary criteria unmet: {gaps:?}",
                manifest.config.attempt_id
            ));
        }
        let families: BTreeMap<Family, Distribution> =
            serde_json::from_value(run["families"].clone())
                .map_err(|e| format!("calibration distributions: {e}"))?;
        if !ordinary_gaps(&families, 120).is_empty() {
            return Err("calibration complete ordinary distributions unavailable".into());
        }
        manifests.push(manifest);
    }
    for (left, right) in [(0, 3), (1, 2)] {
        if manifests[left].source_tree_hash != manifests[right].source_tree_hash
            || manifests[left].executable_sha256 != manifests[right].executable_sha256
            || manifests[left].policy_patch_sha256 != manifests[right].policy_patch_sha256
        {
            return Err("same-instrumentation calibration source changed across regimes".into());
        }
    }
    let mut comparisons = Vec::new();
    for (regime, before, after) in [(Regime::Resident, 0, 1), (Regime::Pressured, 3, 2)] {
        let (foundation, observers) = (&manifests[before], &manifests[after]);
        if foundation.target_identity != observers.target_identity
            || foundation.image_digest != observers.image_digest
            || foundation.toolchain != observers.toolchain
            || foundation.features != observers.features
        {
            return Err("calibration changes target/image/toolchain/features".into());
        }
        let mut left = foundation.effective_inputs.clone();
        let mut right = observers.effective_inputs.clone();
        for inputs in [&mut left, &mut right] {
            let object = inputs
                .as_object_mut()
                .ok_or("effective inputs must be an object")?;
            object.remove("instrumentation");
            // Capacity readback is retained per run, not a workload input.
            object.remove("preflight_free_disk_bytes");
        }
        if left != right {
            return Err("calibration changes frozen inputs beyond instrumentation".into());
        }
        let failed = runs[after]["sampling_failures"]
            .as_f64()
            .filter(|v| v.is_finite() && *v >= 0.0)
            .ok_or("observer calibration failures unavailable")?;
        let mut families = Vec::new();
        let mut reopen = failed > 0.0;
        for family in FAMILIES {
            let key = serde_json::to_value(family).map_err(|e| e.to_string())?;
            let key = key.as_str().ok_or("family name unavailable")?;
            let baseline = number(&runs[before]["families"][key], "p99_ns")?;
            let observed = number(&runs[after]["families"][key], "p99_ns")?;
            let regression = u128::from(observed) > u128::from(baseline) + 5_000_000
                && u128::from(observed) * 100 > u128::from(baseline) * 110;
            reopen |= regression;
            families.push(serde_json::json!({"family":family,"foundation_p99_ns":baseline,"observers_p99_ns":observed,"regression":regression}));
        }
        comparisons.push(serde_json::json!({"regime":regime,"families":families,"sampling_failures":failed,"reopen":reopen}));
    }
    Ok(comparisons)
}

fn composed_fault_intervals<'a>(
    selected: &[&'a Json],
    gaps: &mut Vec<String>,
) -> Assembly<BTreeMap<&'static str, (u64, u64, &'a str)>> {
    let mut faults = BTreeMap::new();
    for (kind, role, arms, release) in [
        (
            "sampler_commit",
            "service1",
            &[180, 200, 220, 240, 260, 280, 300][..],
            340,
        ),
        ("later_batch", "service0", &[120][..], 240),
    ] {
        let receipts: Vec<_> = named_events(selected, "composed_fault")
            .filter(|event| event["kind"] == kind)
            .collect();
        if receipts.len() != arms.len() + 1 {
            return Err(format!(
                "{kind}: complete declared arm series and release receipt required"
            ));
        }
        let mut boundaries = Vec::new();
        for (stage, second) in arms
            .iter()
            .map(|second| ("armed", *second))
            .chain(std::iter::once(("released", release)))
        {
            let matches: Vec<_> = receipts
                .iter()
                .copied()
                .filter(|event| event["stage"] == stage && event["scheduled_seconds"] == second)
                .collect();
            if matches.len() != 1 || string(matches[0], "role")? != role {
                return Err(format!(
                    "{kind}/{stage}/{second}: unique scheduled receipt from {role} required"
                ));
            }
            let observed = number(matches[0], "elapsed_ns")?;
            let planned = second * 1_000_000_000;
            if !(planned..=planned + 5_000_000_000).contains(&observed) {
                gaps.push(format!(
                    "{kind}/{stage}/{second}: observed fault time outside declared schedule"
                ));
            }
            boundaries.push(observed);
        }
        if boundaries.windows(2).any(|pair| pair[0] >= pair[1])
            || boundaries.last().is_some_and(|at| *at >= 1_200_000_000_000)
        {
            return Err(format!("{kind}: invalid fault order or end"));
        }
        faults.insert(
            kind,
            (boundaries[0], boundaries[boundaries.len() - 1], role),
        );
    }
    Ok(faults)
}

/// The composed disturbance proof is deliberately excluded from policy capacity
/// distributions. Missing proof is returned as explicit gaps, never inferred.
pub(crate) fn composed_report(manifest: &Manifest, events: &[Json]) -> Assembly<Json> {
    inventory_inputs(manifest)?;
    if manifest.config.regime != Regime::Pressured {
        return Err("composed proof requires the pressured regime".into());
    }
    let selected: Vec<_> = events
        .iter()
        .filter(|event| event["segment"] == serde_json::json!(Segment::Steady))
        .collect();
    let records = operation_receipts(Segment::Steady, &selected, 1200)?;
    let mut gaps = Vec::new();
    for name in ["resource_sample", "database_counters"] {
        require_window(
            &timed_events(&selected, name)?,
            name,
            true,
            1_200_000_000_000,
        )?;
    }
    for event in named_events(&selected, "resource_sample") {
        if string(event, "inputs_hash")? != string(&manifest.effective_inputs, "inputs_hash")?
            || !boolean(event, "uncontended_host")?
            || !boolean(event, "clock_valid")?
        {
            gaps.push("composed effective inputs, host qualification or clock changed".into());
            break;
        }
    }
    let recovery = one_event(&selected, "recovery_checked")?;
    if !boolean(recovery, "readable")?
        || number(recovery, "correctness_violations")? != 0
        || number(recovery, "hidden_retries")? != 0
    {
        gaps.push("composed recovery/readability/correctness criterion unmet".to_owned());
    }
    if !boolean(one_event(&selected, "owners_stopped")?, "complete")? {
        gaps.push("composed owners did not reach verified stopped state".into());
    }
    let final_cohort = one_event(&selected, "composed_final_cohort")?;
    if number(&final_cohort["inventory"], "first_batch_remaining")? != 0
        || number(&final_cohort["inventory"], "rejected_remaining")? != 0
    {
        gaps.push("composed confirmed-first-batch/rejected-later-row inventory did not drain after recovery".into());
    }
    let faults = composed_fault_intervals(&selected, &mut gaps)?;
    let sampler = faults["sampler_commit"];
    let later_batch = faults["later_batch"];
    let mut pass_intervals: BTreeMap<(&str, &str), Vec<(u64, u64)>> = BTreeMap::new();
    let mut failed_progress = None;
    let mut cleanup_resumed = false;
    let mut durations: BTreeMap<String, Vec<u64>> = BTreeMap::new();
    for event in named_events(&selected, "native_trace") {
        let fields = &event["fields"];
        if fields["message"].as_str() != Some("postgres_cleanup_pass_finished") {
            continue;
        }
        let role = string(event, "role")?;
        let cleanup = string(fields, "cleanup")?;
        let at = number(event, "elapsed_ns")?;
        let elapsed = std::time::Duration::try_from_secs_f64(
            fields["elapsed_seconds"]
                .as_f64()
                .ok_or("cleanup elapsed seconds absent")?,
        )
        .map_err(|_| "cleanup elapsed seconds invalid")?;
        let elapsed = u64::try_from(elapsed.as_nanos()).map_err(|_| "cleanup elapsed overflow")?;
        pass_intervals
            .entry((role, cleanup))
            .or_default()
            .push((at.saturating_sub(elapsed), at));
        durations
            .entry(format!("{role}/{cleanup}"))
            .or_default()
            .push(elapsed);
        if role == later_batch.2
            && at >= later_batch.0
            && at <= later_batch.1
            && string(fields, "outcome")? == "failed"
            && number(fields, "committed_batches")? > 0
            && number(fields, "removed_rows")? >= 500
        {
            failed_progress = Some((at, cleanup));
        }
    }
    if let Some((failed_at, cleanup)) = failed_progress {
        cleanup_resumed = named_events(&selected, "native_trace").any(|event| {
            let fields = &event["fields"];
            fields["message"] == "postgres_cleanup_pass_finished"
                && fields["cleanup"].as_str() == Some(cleanup)
                && event["role"].as_str() == Some(later_batch.2)
                && event["elapsed_ns"]
                    .as_u64()
                    .is_some_and(|at| at > failed_at && at >= later_batch.1)
                && fields["outcome"] == "completed"
        });
    }
    if failed_progress.is_none() {
        gaps.push("later failed pass with prior confirmed batch/rows not observed".into());
    }
    if !cleanup_resumed {
        gaps.push("cleanup resumption after later-batch fault not observed".into());
    }
    let mut stale = false;
    let mut fresh_again = false;
    for role in ROLES {
        let role_events: Vec<_> = selected
            .iter()
            .copied()
            .filter(|e| e["role"].as_str() == Some(role))
            .collect();
        for name in ["role_started", "role_stopped"] {
            one_event(&role_events, name)?;
        }
        if role.starts_with("worker") {
            let drained = one_event(&role_events, "worker_drained")?;
            if !false_or_zero(drained, "uncertain")? || !false_or_zero(drained, "timed_out")? {
                gaps.push(format!("{role}: worker drain uncertain or timed out"));
            }
        }
        let metrics = timed_events(&role_events, "metrics")?;
        require_window(
            &metrics,
            &format!("{role} metrics"),
            false,
            1_200_000_000_000,
        )?;
        let cleanup_names: &[&str] = if role.starts_with("worker") {
            &["jobs"]
        } else {
            &["http_idempotency", "webhook_receipts"]
        };
        for cleanup in cleanup_names {
            let key = (role, *cleanup);
            let Some(intervals) = pass_intervals.get_mut(&key) else {
                gaps.push(format!(
                    "{role}/{cleanup}: terminal cleanup intervals missing"
                ));
                continue;
            };
            intervals.sort_unstable();
            if intervals
                .windows(2)
                .any(|pair| pair[1].0.saturating_add(1000) < pair[0].1)
            {
                gaps.push(format!("{role}/{cleanup}: observed overlapping passes"));
            }
            let last = string(metrics.last().ok_or("final metrics missing")?.1, "text")?;
            if metric(
                last,
                "postgres_cleanup_active_passes",
                &[("cleanup", cleanup)],
            )? != Some(0.0)
            {
                gaps.push(format!(
                    "{role}/{cleanup}: final active passes are not zero"
                ));
            }
        }
        let populations = if role.starts_with("worker") {
            ["jobs", "failed_jobs"]
        } else {
            ["http_idempotency", "webhook_receipts"]
        };
        for population in populations {
            if role == "service1" && population == "http_idempotency" {
                continue;
            }
            for (at, event) in metrics
                .iter()
                .filter(|(at, _)| *at >= sampler.0 && *at <= sampler.1)
            {
                let labels = [("population", population)];
                let text = string(event, "text")?;
                let observed = number(event, "observed_unix_ms")? as f64 / 1000.0;
                let success = metric(
                    text,
                    "postgres_maintenance_last_success_timestamp_seconds",
                    &labels,
                )?;
                if !success
                    .is_some_and(|last| last > 0.0 && observed >= last && observed - last <= 90.0)
                    || metric(text, "postgres_maintenance_last_attempt_success", &labels)?
                        != Some(1.0)
                {
                    gaps.push(format!("{role}/{population} at {at}: another sampler unhealthy during the one-owner disturbance"));
                    break;
                }
            }
        }
        if role == "service1" {
            let labels = [("population", "http_idempotency")];
            let mut last_good = None;
            for (at, event) in &metrics {
                let text = string(event, "text")?;
                let success = metric(
                    text,
                    "postgres_maintenance_last_success_timestamp_seconds",
                    &labels,
                )?;
                let attempted = metric(text, "postgres_maintenance_last_attempt_success", &labels)?;
                let observed = number(event, "observed_unix_ms")? as f64 / 1000.0;
                if *at <= sampler.1 && attempted == Some(1.0) && !stale {
                    last_good = success.filter(|v| *v > 0.0);
                }
                if *at >= sampler.0
                    && *at <= sampler.1
                    && last_good.is_some()
                    && success == last_good
                    && attempted == Some(0.0)
                    && success.is_some_and(|last| observed - last > 90.0)
                {
                    stale = true;
                }
                if *at > sampler.1
                    && stale
                    && attempted == Some(1.0)
                    && success.zip(last_good).is_some_and(|(now, before)| {
                        now > before && observed >= now && observed - now <= 90.0
                    })
                {
                    fresh_again = true;
                }
            }
        }
    }
    if !stale {
        gaps.push(
            "one failed stale sampler retaining its last-good timestamp was not observed".into(),
        );
    }
    if !fresh_again {
        gaps.push("stale sampler did not resume fresh successful observations".into());
    }
    let distributions: BTreeMap<_, _> = FAMILIES
        .into_iter()
        .map(|family| {
            (
                family,
                summarize_operations(records.iter().filter(|r| r.family == family)),
            )
        })
        .collect();
    Ok(
        serde_json::json!({"scope":"20-minute composed behavior demonstration; disturbances retained and excluded from policy capacity comparisons; not delivery acceptance","status":if gaps.is_empty() {"composed_observations_complete"} else {"composed_proof_incomplete"},"gaps":gaps,"manifest":manifest,"duration_seconds":1200,"fault_intervals":faults,"fault_receipts":named_events(&selected,"composed_fault").collect::<Vec<_>>(),"confirmed_progress_before_failure":failed_progress,"cleanup_resumed":cleanup_resumed,"final_cohort":final_cohort["inventory"],"stale_last_good_observed":stale,"fresh_sampler_resumed":fresh_again,"disturbed_operation_accounting":distributions,"cleanup_duration_ns":durations.into_iter().map(|(name,values)| (name,range(values.into_iter()))).collect::<BTreeMap<_,_>>() }),
    )
}

/// Reconstructs a cell only from complete raw receipts. Missing observations
/// return an error; observed failed criteria remain in the returned report.
pub(crate) fn assemble_cell(manifest: &Manifest, events: &[Json]) -> Assembly<CellReport> {
    inventory_inputs(manifest)?;
    let mut segments = Vec::new();
    let mut gaps = Vec::new();
    for segment in SEGMENTS {
        let selected: Vec<_> = events
            .iter()
            .filter(|event| {
                event
                    .get("segment")
                    .or_else(|| event.pointer("/record/segment"))
                    .is_some_and(|value| {
                        serde_json::from_value::<Segment>(value.clone()).ok() == Some(segment)
                    })
            })
            .collect();
        match assemble_segment(manifest, segment, &selected) {
            Ok(report) => segments.push(report),
            Err(gap) => gaps.push(format!("{segment:?}: {gap}")),
        }
    }
    if !gaps.is_empty() {
        return Err(gaps.join("; "));
    }
    Ok(CellReport {
        config: manifest.config.clone(),
        manifest: Some(manifest.clone()),
        segments,
        invalid_reason: None,
    })
}

fn assemble_segment(
    manifest: &Manifest,
    segment: Segment,
    events: &[&Json],
) -> Assembly<SegmentReport> {
    let records = operation_receipts(segment, events, 150)?;
    let resources = observed_stream(events, "resource_sample")?;
    let relations = observed_stream(events, "database_sample")?;
    let counters = observed_stream(events, "database_counters")?;
    let mut inventory = timed_events(events, "inventory")?;
    inventory.extend(timed_events(events, "final_inventory")?);
    inventory.sort_by_key(|(at, _)| *at);
    require_interval(&inventory, "inventory", true)?;
    let recovery = one_event(events, "recovery_checked")?;
    let custody = one_event(events, "owners_stopped")?;
    let provenance = one_event(events, "native_dml_provenance")?;
    if boolean(provenance, "fixture_dml_during_measurement")? {
        return Err("native relation deltas are contaminated by measured fixture DML".into());
    }
    for role in ROLES {
        for name in ["role_started", "role_stopped"] {
            if named_events(events, name)
                .filter(|event| event["role"].as_str() == Some(role))
                .count()
                != 1
            {
                return Err(format!("{role}: missing or repeated {name}"));
            }
        }
    }
    for role in ["worker0", "worker1"] {
        let drains: Vec<_> = named_events(events, "worker_drained")
            .filter(|event| event["role"].as_str() == Some(role))
            .collect();
        if drains.len() != 1 {
            return Err(format!(
                "{role}: worker_drained receipt missing or repeated"
            ));
        }
        if !false_or_zero(drains[0], "uncertain")? || !false_or_zero(drains[0], "timed_out")? {
            return Err(format!("{role}: worker drain was uncertain or timed out"));
        }
    }
    let input_hash = string(&manifest.effective_inputs, "inputs_hash")?;
    let mut unchanged = true;
    let mut uncontended = true;
    let mut clock = true;
    for (_, sample) in &resources {
        unchanged &= string(sample, "inputs_hash")? == input_hash;
        uncontended &= boolean(sample, "uncontended_host")?;
        clock &= boolean(sample, "clock_valid")?;
    }
    let first_counters = &counters[0].1["sample"];
    let last_counters = &counters[counters.len() - 1].1["sample"];
    for authority in ["database", "wal", "checkpointer"] {
        let path = format!("/sample/progress/{authority}/stats_reset");
        let reset = counters[0].1.pointer(&path);
        if authority == "database" && reset.is_none() {
            return Err("database stats_reset boundary missing".into());
        }
        if counters
            .iter()
            .any(|(_, value)| value.pointer(&path) != reset)
        {
            return Err(format!(
                "{authority} statistics reset or reset boundary disappeared during segment; counter deltas unavailable"
            ));
        }
    }
    let mut sizes = Vec::new();
    for (_, sample) in &relations {
        sizes.push(relation_sum(sample, "total_bytes")?);
    }
    let retained_bytes = match manifest.config.regime {
        Regime::Resident => sizes.into_iter().max(),
        Regime::Pressured => sizes.into_iter().min(),
    }
    .ok_or("retained relation sizes missing")?;
    let before = relations[0].1;
    let after = relations[relations.len() - 1].1;
    let native_inserts = delta(
        relation_sum(before, "insert")?,
        relation_sum(after, "insert")?,
        "native insert",
    )?;
    let native_updates = delta(
        relation_sum(before, "update")?,
        relation_sum(after, "update")?,
        "native update",
    )?;
    let native_deletes = delta(
        relation_sum(before, "delete")?,
        relation_sum(after, "delete")?,
        "native delete",
    )?;
    let actions = action_receipts(events)?;
    let live_bodies = number(&manifest.effective_inputs, "live_bodies")?;
    let mut bodies = BTreeMap::new();
    for record in records
        .iter()
        .filter(|record| record.family == Family::WideReplay && record.outcome == Outcome::Expected)
    {
        let action = actions
            .get(&record.id)
            .ok_or_else(|| format!("operation {}: action receipt missing", record.id))?;
        if string(action, "kind")? != "Wide" || number(action, "size")? != 512 * 1024 {
            return Err(format!(
                "operation {}: wide replay action/size mismatch",
                record.id
            ));
        }
        if number(action, "generation")? != 0
            || !(10_000_000
                ..10_000_000_u64
                    .checked_add(live_bodies)
                    .ok_or("live cohort identity overflow")?)
                .contains(&number(action, "identity")?)
        {
            return Err(format!(
                "operation {}: replay outside frozen inventory cohort",
                record.id
            ));
        }
        bodies.insert(number(action, "identity")?, number(action, "size")?);
    }
    let metrics = observation_metrics(events)?;
    let mut backlog = BTreeMap::new();
    for (family, key, cleanup) in [
        (Family::Jobs, "jobs", "jobs"),
        (Family::Idempotency, "idempotency", "http_idempotency"),
        (Family::Webhook, "receipts", "webhook_receipts"),
    ] {
        let mut delays = Vec::new();
        for (_, event) in &inventory {
            delays.push(seconds_to_ms(
                &event["inventory"]["delay"][key],
                &format!("inventory.delay.{key}"),
            )?);
        }
        delays.sort_unstable();
        let initial = number(&inventory[0].1["inventory"]["eligible"], key)?;
        let final_count = number(
            &inventory[inventory.len() - 1].1["inventory"]["eligible"],
            key,
        )?;
        let cohort_size = number(&inventory[0].1["inventory"]["catchup"], key)?;
        let remaining = |deadline| -> Assembly<u64> {
            let event = inventory
                .iter()
                .rev()
                .find(|(at, _)| *at <= deadline)
                .ok_or_else(|| format!("{key}: cohort observation at/before deadline missing"))?;
            number(&event.1["inventory"]["catchup"], key)
        };
        let mut drain = None;
        for (at, event) in &inventory {
            if number(&event["inventory"]["catchup"], key)? <= 1000 && drain.is_none() {
                drain = Some(at.div_ceil(1_000_000));
            }
        }
        backlog.insert(
            family,
            Backlog {
                initial,
                final_count,
                delay_p95_ms: percentile(&delays, 95),
                delay_max_ms: delays.last().copied().ok_or("delay samples missing")?,
                completed_cycle_troughs: cycle_troughs(events, &inventory, cleanup, key)?,
                initial_cohort_size: cohort_size,
                initial_cohort_remaining_120s: remaining(120_000_000_000)?,
                initial_cohort_remaining_150s: remaining(SEGMENT_NS)?,
                drain_90_percent_ms: drain,
            },
        );
    }
    let maintenance_overlapped = maintenance_overlap(events, &records)?;
    let qualification = Qualification {
        retained_bytes,
        shared_buffers_bytes: number(&manifest.effective_inputs, "shared_buffers_bytes")?,
        pg_memory_limit_bytes: number(&manifest.effective_inputs, "pg_memory_limit_bytes")?,
        relation_toast_hits: delta(
            number(first_counters, "buffer_hits")?,
            number(last_counters, "buffer_hits")?,
            "buffer hits",
        )?,
        relation_toast_reads: delta(
            number(first_counters, "buffer_reads")?,
            number(last_counters, "buffer_reads")?,
            "buffer reads",
        )?,
        distinct_full_body_bytes: bodies.values().sum(),
        live_body_cohort_bytes: number(&manifest.effective_inputs, "live_bodies")?
            .checked_mul(512 * 1024)
            .ok_or("live body cohort size overflow")?,
        container_block_read_bytes: delta(
            number(resources[0].1, "container_block_read_bytes")?,
            number(
                resources[resources.len() - 1].1,
                "container_block_read_bytes",
            )?,
            "container block reads",
        )?,
        native_inserts,
        native_updates,
        native_deletes,
        full_body_content_checked: !bodies.is_empty(),
        jobs_and_receipts_observed: [Family::Jobs, Family::Webhook].iter().all(|family| {
            records
                .iter()
                .any(|record| record.family == *family && record.outcome == Outcome::Expected)
        }),
        maintenance_overlapped,
        inputs_unchanged: unchanged,
        records_complete: true,
        uncontended_host: uncontended,
        monotonic_clock: clock,
    };
    Ok(SegmentReport {
        segment,
        qualification,
        families: FAMILIES
            .into_iter()
            .map(|family| {
                (
                    family,
                    summarize_operations(records.iter().filter(|record| record.family == family)),
                )
            })
            .collect(),
        first_minute: FAMILIES
            .into_iter()
            .map(|family| {
                (
                    family,
                    summarize_operations(records.iter().filter(|record| {
                        record.family == family && record.planned_ns < 60_000_000_000
                    })),
                )
            })
            .collect(),
        backlog,
        correctness_violations: number(recovery, "correctness_violations")?,
        hidden_retries: number(recovery, "hidden_retries")?,
        maximum_sample_age_ms: Some(metrics),
        all_expected_populations_observed: true,
        sampling_deadline_failures: 0,
        recovery_readable: boolean(recovery, "readable")?,
        owners_stopped: boolean(custody, "complete")?,
        // Existing five-second activity samples define sampled lock contention;
        // incomplete coverage is unavailable, never zero contention.
        first_minute_contention_ns: first_minute_lock_contention(events),
    })
}

/// Trapezoidal integral of Lock-waiting database sessions over the first
/// minute, in session-nanoseconds. This is a sampled pressure estimate, not
/// individual lock-wait latency; it requires both endpoints and every interval.
fn first_minute_lock_contention(events: &[&Json]) -> Option<u64> {
    let mut samples = Vec::new();
    for event in named_events(events, "database_counters") {
        let at = event["elapsed_ns"].as_u64()?;
        let activity = event["sample"]["activity"].as_array()?;
        let mut locks = 0_u64;
        for row in activity {
            if row["wait_class"].as_str()? == "Lock" {
                locks = locks.checked_add(row["sessions"].as_u64()?)?;
            }
        }
        samples.push((at, locks));
    }
    samples.sort_unstable();
    samples.dedup_by_key(|sample| sample.0);
    if samples.first()?.0 != 0 || samples.last()?.0 < 60_000_000_000 {
        return None;
    }
    let mut integral = 0_f64;
    for pair in samples.windows(2) {
        let [(a, left), (b, right)] = pair else {
            return None;
        };
        if *a >= 60_000_000_000 {
            break;
        }
        let width = b.checked_sub(*a)?;
        if width == 0 || width > 10_000_000_000 {
            return None;
        }
        let end = (*b).min(60_000_000_000);
        let clipped_right =
            *left as f64 + (*right as f64 - *left as f64) * (end - a) as f64 / width as f64;
        integral += (*left as f64 + clipped_right) * 0.5 * (end - a) as f64;
    }
    if !integral.is_finite() || integral < 0.0 || integral >= u64::MAX as f64 {
        return None;
    }
    Some(integral.round() as u64)
}

fn named_events<'a, 's>(
    events: &'s [&'a Json],
    name: &'s str,
) -> impl Iterator<Item = &'a Json> + 's {
    events
        .iter()
        .copied()
        .filter(move |event| event["event"].as_str() == Some(name))
}

fn one_event<'a>(events: &[&'a Json], name: &str) -> Assembly<&'a Json> {
    let found: Vec<_> = events
        .iter()
        .copied()
        .filter(|event| event["event"].as_str() == Some(name))
        .collect();
    if found.len() != 1 {
        return Err(format!(
            "{name}: expected one receipt, observed {}",
            found.len()
        ));
    }
    Ok(found[0])
}

fn number(value: &Json, field: &str) -> Assembly<u64> {
    value[field]
        .as_u64()
        .ok_or_else(|| format!("{field}: missing nonnegative integer"))
}

fn string<'a>(value: &'a Json, field: &str) -> Assembly<&'a str> {
    value[field]
        .as_str()
        .ok_or_else(|| format!("{field}: missing string"))
}

fn boolean(value: &Json, field: &str) -> Assembly<bool> {
    value[field]
        .as_bool()
        .ok_or_else(|| format!("{field}: missing boolean"))
}

fn false_or_zero(value: &Json, field: &str) -> Assembly<bool> {
    if let Some(value) = value[field].as_bool() {
        return Ok(!value);
    }
    number(value, field).map(|value| value == 0)
}

fn delta(before: u64, after: u64, name: &str) -> Assembly<u64> {
    after
        .checked_sub(before)
        .ok_or_else(|| format!("{name}: counter reset; delta unavailable"))
}

fn seconds_to_ms(value: &Json, name: &str) -> Assembly<u64> {
    let seconds = value
        .as_f64()
        .ok_or_else(|| format!("{name}: missing seconds"))?;
    let duration = std::time::Duration::try_from_secs_f64(seconds)
        .map_err(|_| format!("{name}: invalid finite nonnegative seconds"))?;
    u64::try_from(duration.as_millis()).map_err(|_| format!("{name}: milliseconds overflow"))
}

fn timed_events<'a>(events: &[&'a Json], name: &str) -> Assembly<Vec<(u64, &'a Json)>> {
    let mut result = Vec::new();
    for event in events
        .iter()
        .copied()
        .filter(|event| event["event"].as_str() == Some(name))
    {
        result.push((number(event, "elapsed_ns")?, event));
    }
    result.sort_by_key(|(at, _)| *at);
    Ok(result)
}

fn observed_stream<'a>(events: &[&'a Json], name: &str) -> Assembly<Vec<(u64, &'a Json)>> {
    let result = timed_events(events, name)?;
    require_interval(&result, name, true)?;
    Ok(result)
}

fn require_interval(events: &[(u64, &Json)], name: &str, exact_start: bool) -> Assembly<()> {
    require_window(events, name, exact_start, SEGMENT_NS)
}

fn require_window(
    events: &[(u64, &Json)],
    name: &str,
    exact_start: bool,
    end_ns: u64,
) -> Assembly<()> {
    let Some(first) = events.first() else {
        return Err(format!("{name}: observations missing"));
    };
    let last = events[events.len() - 1].0;
    // Relation size reads run every 30 seconds and may finish after their
    // scheduled tick. The independently polled resource stream owns the
    // strict 30-second observer-loss bound.
    let maximum_gap = if name == "database_sample" {
        35_000_000_000
    } else {
        30_000_000_000
    };
    if (exact_start && first.0 != 0)
        || (!exact_start && first.0 > 5_000_000_000)
        || last < end_ns
        || events
            .windows(2)
            .any(|pair| pair[1].0 <= pair[0].0 || pair[1].0 - pair[0].0 > maximum_gap)
    {
        return Err(format!(
            "{name}: incomplete 0..{}s coverage, repeated timestamp or observation gap over {}s",
            end_ns / 1_000_000_000,
            maximum_gap / 1_000_000_000
        ));
    }
    Ok(())
}

fn relation_sum(event: &Json, field: &str) -> Assembly<u64> {
    let relations = event["relations"]
        .as_array()
        .ok_or("relations array missing")?;
    let mut result = 0_u64;
    for expected in RETAINED {
        let rows: Vec<_> = relations
            .iter()
            .filter(|row| row["relation"].as_str() == Some(expected))
            .collect();
        if rows.len() != 1 {
            return Err(format!(
                "{expected}: relation observation missing or repeated"
            ));
        }
        result = result
            .checked_add(number(rows[0], field)?)
            .ok_or("relation aggregate overflow")?;
    }
    Ok(result)
}

fn action_receipts<'a>(events: &'a [&'a Json]) -> Assembly<BTreeMap<u64, &'a Json>> {
    let mut actions = BTreeMap::new();
    for event in named_events(events, "operation_action") {
        let id = number(event, "id")?;
        let action = event
            .get("action")
            .ok_or("operation_action.action missing")?;
        if actions.insert(id, action).is_some() {
            return Err(format!("operation {id}: duplicate action"));
        }
    }
    Ok(actions)
}

fn operation_receipts(
    segment: Segment,
    events: &[&Json],
    seconds: u64,
) -> Assembly<Vec<OperationRecord>> {
    let mut planned = BTreeMap::new();
    let mut started = BTreeMap::new();
    let mut completed = BTreeMap::new();
    for event in named_events(events, "planned") {
        let id = number(event, "id")?;
        if planned.insert(id, event).is_some() {
            return Err(format!("operation {id}: duplicate planned receipt"));
        }
    }
    for event in named_events(events, "started") {
        let id = number(event, "id")?;
        if started.insert(id, number(event, "started_ns")?).is_some() {
            return Err(format!("operation {id}: duplicate started receipt"));
        }
    }
    for event in named_events(events, "completed") {
        let record: OperationRecord = serde_json::from_value(event["record"].clone())
            .map_err(|error| format!("completed operation: {error}"))?;
        let id = record.id;
        if completed.insert(id, record).is_some() {
            return Err(format!("operation {id}: duplicate completion"));
        }
    }
    let scheduled = usize::try_from(seconds * 164).map_err(|_| "operation count overflow")?;
    if planned.len() != scheduled || completed.len() != planned.len() {
        return Err(format!(
            "scheduled accounting incomplete: {} planned, {} completed; expected {scheduled}",
            planned.len(),
            completed.len()
        ));
    }
    let mut mixed_times = Vec::new();
    let mut wide_times = Vec::new();
    let actions = action_receipts(events)?;
    if actions.len() != planned.len() || actions.keys().any(|id| !planned.contains_key(id)) {
        return Err("action accounting does not match scheduled operations".into());
    }
    for (&id, plan) in &planned {
        let record = completed
            .get(&id)
            .ok_or_else(|| format!("operation {id}: completion missing"))?;
        let family: Family = serde_json::from_value(plan["family"].clone())
            .map_err(|error| format!("planned family: {error}"))?;
        if record.segment != segment
            || record.family != family
            || record.planned_ns != number(plan, "planned_ns")?
            || record.expected != string(plan, "expected")?
            || record.started_ns != started.get(&id).copied()
            || (record.completed_ns < record.planned_ns
                && !(record.outcome == Outcome::NotStarted
                    && record.actual == "cancelled_before_admission"))
            || record
                .started_ns
                .is_some_and(|at| at < record.planned_ns || at > record.completed_ns)
            || (record.outcome == Outcome::NotStarted) != record.started_ns.is_none()
            || record.expected.is_empty()
            || record.actual.is_empty()
        {
            return Err(format!(
                "operation {id}: planned/start/completed receipts disagree"
            ));
        }
        let action = actions[&id];
        if action["family"] != plan["family"] || string(action, "kind")? != record.expected {
            return Err(format!(
                "operation {id}: declared action differs from planned oracle"
            ));
        }
        if record.family == Family::WideReplay {
            wide_times.push(record.planned_ns);
        } else {
            mixed_times.push(record.planned_ns);
        }
    }
    if started.keys().any(|id| !planned.contains_key(id)) {
        return Err("orphan started operation".into());
    }
    for (times, count, interval) in [
        (&mut mixed_times, seconds as usize * 100, 10_000_000),
        (&mut wide_times, seconds as usize * 64, 15_625_000),
    ] {
        times.sort_unstable();
        if times.len() != count
            || times
                .iter()
                .enumerate()
                .any(|(index, at)| *at != index as u64 * interval)
        {
            return Err("arrival timeline does not cover the fixed 100/s + 64/s schedule".into());
        }
    }
    for family in FAMILIES {
        let count = completed
            .values()
            .filter(|record| record.family == family)
            .count() as u64;
        if count != seconds * arrivals_per_second(family) {
            return Err(format!(
                "{family:?}: scheduled family mix differs from fixed input"
            ));
        }
    }
    Ok(completed.into_values().collect())
}

fn metric(text: &str, name: &str, labels: &[(&str, &str)]) -> Assembly<Option<f64>> {
    let mut result = None;
    for line in text.lines().filter(|line| !line.starts_with('#')) {
        let mut parts = line.split_whitespace();
        let Some(key) = parts.next() else {
            continue;
        };
        let Some((metric_name, raw_labels)) = key.split_once('{') else {
            continue;
        };
        if metric_name != name {
            continue;
        }
        let raw_labels = raw_labels
            .strip_suffix('}')
            .ok_or("malformed metric labels")?;
        if !labels.iter().all(|(wanted, expected)| {
            raw_labels.split(',').any(|label| {
                label.split_once('=').is_some_and(|(key, value)| {
                    key == *wanted && value.trim_matches('"') == *expected
                })
            })
        }) {
            continue;
        }
        let value: f64 = parts
            .next()
            .ok_or("metric value missing")?
            .parse()
            .map_err(|_| "metric numeric value malformed")?;
        if !value.is_finite() {
            return Err(format!("{name}: non-finite metric value"));
        }
        if result.replace(value).is_some() {
            return Err(format!("{name}: ambiguous duplicated metric series"));
        }
    }
    Ok(result)
}

fn observation_metrics(events: &[&Json]) -> Assembly<u64> {
    let mut maximum_age = 0;
    for role in ROLES {
        let selected: Vec<_> = events
            .iter()
            .copied()
            .filter(|event| event["role"].as_str() == Some(role))
            .collect();
        let samples = timed_events(&selected, "metrics")?;
        require_interval(&samples, &format!("{role} metrics"), false)?;
        let baseline = one_event(&selected, "metrics_baseline")?;
        if number(baseline, "elapsed_ns")? != 0 {
            return Err(format!(
                "{role}: metrics baseline is not at the segment boundary"
            ));
        }
        let baseline_text = string(baseline, "text")?;
        let populations = if role.starts_with("worker") {
            ["jobs", "failed_jobs"]
        } else {
            ["http_idempotency", "webhook_receipts"]
        };
        for population in populations {
            let mut succeeded = false;
            let mut final_failed = None;
            let labels = [("population", population)];
            // The controller's recorder survives segment restarts. An absent
            // baseline series is zero only in this before-owner-start receipt.
            let baseline_failed = metric(
                baseline_text,
                "postgres_maintenance_observations_total",
                &[("population", population), ("outcome", "failed")],
            )?
            .unwrap_or_default();
            let baseline_success = metric(
                baseline_text,
                "postgres_maintenance_last_success_timestamp_seconds",
                &labels,
            )?
            .unwrap_or_default();
            let mut previous_success = baseline_success;
            for (_, event) in &samples {
                let text = string(event, "text")?;
                let last = metric(
                    text,
                    "postgres_maintenance_last_success_timestamp_seconds",
                    &labels,
                )?;
                final_failed = metric(
                    text,
                    "postgres_maintenance_observations_total",
                    &[("population", population), ("outcome", "failed")],
                )?;
                if final_failed.is_some_and(|failed| failed < baseline_failed) {
                    return Err(format!("{role}/{population}: failure counter reset"));
                }
                if final_failed.is_some_and(|failed| failed > baseline_failed) {
                    return Err(format!(
                        "{role}/{population}: sampling failures observed without deadline classification"
                    ));
                }
                if let Some(last) = last.filter(|last| *last > baseline_success) {
                    if last < previous_success {
                        return Err(format!(
                            "{role}/{population}: last-good timestamp regressed"
                        ));
                    }
                    let observed =
                        std::time::Duration::from_millis(number(event, "observed_unix_ms")?)
                            .as_secs_f64();
                    // Event timestamps have millisecond precision; the metric
                    // retains fractional seconds from the same wall clock.
                    if last > observed + 0.001 {
                        return Err(format!(
                            "{role}/{population}: success timestamp is in the future"
                        ));
                    }
                    maximum_age = maximum_age.max(seconds_to_ms(
                        &Json::from((observed - last).max(0.0)),
                        "sample age",
                    )?);
                    succeeded = true;
                    previous_success = last;
                } else if succeeded {
                    return Err(format!("{role}/{population}: last-good timestamp vanished"));
                }
            }
            if !succeeded || final_failed.is_none() {
                return Err(format!(
                    "{role}/{population}: successful sample or final failure counter missing"
                ));
            }
        }
    }
    Ok(maximum_age)
}

fn cycle_troughs(
    events: &[&Json],
    inventory: &[(u64, &Json)],
    cleanup: &str,
    key: &str,
) -> Assembly<Vec<u64>> {
    let mut boundaries = Vec::new();
    for role in if cleanup == "jobs" {
        ["worker0", "worker1"]
    } else {
        ["service0", "service1"]
    } {
        let selected: Vec<_> = events
            .iter()
            .copied()
            .filter(|event| event["role"].as_str() == Some(role))
            .collect();
        let baseline = one_event(&selected, "metrics_baseline")?;
        let mut previous = metric(
            string(baseline, "text")?,
            "postgres_cleanup_passes_total",
            &[("cleanup", cleanup), ("outcome", "completed")],
        )?
        .unwrap_or_default();
        for (at, sample) in timed_events(&selected, "metrics")? {
            if let Some(completed) = metric(
                string(sample, "text")?,
                "postgres_cleanup_passes_total",
                &[("cleanup", cleanup), ("outcome", "completed")],
            )? {
                if completed < previous {
                    return Err(format!("{role}/{cleanup}: cleanup counter reset"));
                }
                if completed > previous {
                    boundaries.push(at);
                }
                previous = completed;
            }
        }
    }
    boundaries.sort_unstable();
    boundaries.dedup();
    let mut troughs = Vec::new();
    let mut previous = 0;
    for boundary in boundaries {
        let mut minimum = None;
        for (_, event) in inventory
            .iter()
            .filter(|(at, _)| *at >= previous && *at <= boundary)
        {
            let count = number(&event["inventory"]["eligible"], key)?;
            minimum = Some(minimum.map_or(count, |prior: u64| prior.min(count)));
        }
        if let Some(minimum) = minimum {
            troughs.push(minimum);
        }
        previous = boundary;
    }
    Ok(troughs)
}

fn maintenance_overlap(events: &[&Json], records: &[OperationRecord]) -> Assembly<bool> {
    let starts = timed_events(events, "vacuum_started")?;
    let ends = timed_events(events, "vacuum_finished")?;
    if starts.len() != 1 || ends.len() != 1 {
        return Err("ordinary VACUUM start/completion receipt missing or duplicated".into());
    }
    let start = starts[0].0;
    let end = ends[0].0;
    Ok(boolean(ends[0].1, "complete")?
        && end > start
        && end <= SEGMENT_NS
        && records.iter().any(|record| {
            record.outcome == Outcome::Expected
                && record.planned_ns < end
                && record.completed_ns > start
        }))
}

#[cfg(test)]
mod tests {
    use super::{
        Backlog, CellReport, Config, Distribution, FAMILIES, Family, OperationRecord, Outcome,
        Policy, Qualification, REGIMES, Regime, SEGMENTS, Segment, SegmentReport, select_policy,
        selection_report, shortlist, summarize_operations,
    };

    #[test]
    fn composed_fault_report_requires_the_complete_declared_arm_series() {
        let mut events: Vec<_> = [180, 200, 220, 240, 260, 280, 300].into_iter()
            .map(|second| serde_json::json!({"event":"composed_fault","kind":"sampler_commit","stage":"armed","role":"service1","scheduled_seconds":second,"elapsed_ns":second*1_000_000_000_u64+1_000})).collect();
        for (kind, stage, role, second) in [
            ("sampler_commit", "released", "service1", 340),
            ("later_batch", "armed", "service0", 120),
            ("later_batch", "released", "service0", 240),
        ] {
            events.push(serde_json::json!({"event":"composed_fault","kind":kind,"stage":stage,"role":role,"scheduled_seconds":second,"elapsed_ns":second*1_000_000_000_u64+1_000}));
        }
        let check = |events: &[serde_json::Value]| {
            let refs: Vec<_> = events.iter().collect();
            let mut gaps = Vec::new();
            super::composed_fault_intervals(&refs, &mut gaps)
                .map(|faults| (faults["sampler_commit"].0, faults["sampler_commit"].1, gaps))
        };
        let (start, end, gaps) =
            check(&events).expect("the declared seven-arm fixture must be reportable");
        assert_eq!((start, end), (180_000_001_000, 340_000_001_000));
        assert!(gaps.is_empty());
        let mut missing = events.clone();
        missing.remove(3);
        assert!(check(&missing).is_err());
        let mut duplicate = events.clone();
        duplicate.push(events[0].clone());
        assert!(check(&duplicate).is_err());
        let mut shifted = events.clone();
        shifted[3]["elapsed_ns"] = serde_json::json!(247_000_000_000_u64);
        assert!(!check(&shifted).unwrap().2.is_empty());
        let mut early_end = events.clone();
        early_end[7]["elapsed_ns"] = serde_json::json!(305_000_000_000_u64);
        assert!(!check(&early_end).unwrap().2.is_empty());
    }

    #[test]
    fn diagnostic_observers_allow_only_preinitialization_absence() {
        let baseline = serde_json::json!({"elapsed_ns":0,"text":""});
        let sample = |second: u64, failed: Option<u64>| {
            serde_json::json!({
                "elapsed_ns":second*1_000_000_000,"observed_unix_ms":(1000+second)*1000,
                "text":failed.map(|failed| format!("postgres_maintenance_observations_total{{population=\"http_idempotency\",outcome=\"failed\"}} {failed}\npostgres_maintenance_last_success_timestamp_seconds{{population=\"http_idempotency\"}} {}\npostgres_maintenance_last_attempt_success{{population=\"http_idempotency\"}} 1\n",1000+second)).unwrap_or_default()
            })
        };
        let check = |events: &[serde_json::Value]| {
            let samples: Vec<_> = events
                .iter()
                .map(|event| (event["elapsed_ns"].as_u64().unwrap(), event))
                .collect();
            super::diagnostic_failures("service0", "http_idempotency", &baseline, &samples, true)
        };
        assert_eq!(
            check(&[sample(0, None), sample(5, Some(0)), sample(60, Some(0))]).unwrap(),
            0.0
        );
        assert_eq!(
            check(&[sample(0, None), sample(5, Some(0)), sample(60, Some(1))]).unwrap(),
            1.0
        );
        assert!(check(&[sample(0, None), sample(60, None)]).is_err());
        assert!(
            check(&[
                sample(0, None),
                sample(5, Some(0)),
                sample(10, None),
                sample(60, Some(0))
            ])
            .is_err()
        );
        assert!(check(&[sample(0, None), sample(5, Some(1)), sample(60, Some(0))]).is_err());
    }

    #[test]
    fn measured_job_excess_can_shrink_once_without_qualifying_the_seed_early() {
        let manifest: super::Manifest=serde_json::from_value(serde_json::json!({
            "config":{"attempt_id":"sizing","policy":"P0","regime":"Resident","repeat":1,"seed":41001},
            "source_tree_hash":"source","executable_sha256":"executable","policy_patch_sha256":"patch",
            "image_digest":"image","toolchain":"pinned","features":["integration"],"target_identity":"task",
            "effective_inputs":{"inventory_seed":41001,"inventory_generation":3,"live_bodies":128,"idempotency_rows":260,"receipt_rows":256,"jobs_rows":2000,"seed_started_unix_ms":1000,"seed_attempt_started_unix_ms":100000,"seed_elapsed_before_attempt_ms":45457}
        })).unwrap();
        let families = [
            "background_jobs",
            "http_idempotency_records",
            "webhook_receipts",
        ];
        let phases:Vec<_>=[("eligibility",[22,23,3]),("replacements",[22,43,3]),("wide",[22,75,3]),("idempotency",[22,83,3]),("jobs",[62,83,3]),("webhooks",[65,83,4])]
            .into_iter().map(|(phase,values)|serde_json::json!({"phase":phase,"relations":families.into_iter().zip(values).map(|(relation,size)|serde_json::json!({"relation":relation,"total_bytes":size*1024_u64*1024})).collect::<Vec<_>>()})).collect();
        let phases = serde_json::json!(phases);
        let report = super::inventory_report(
            &manifest,
            &phases[5]["relations"],
            &phases,
            0,
            46000,
            "cohort",
        )
        .unwrap();
        assert_eq!(report["status"], "adjustment_proposed");
        assert!(report["proposed_counts"]["jobs_rows"].as_u64().unwrap() < 2000);
        assert_eq!(
            report["predicted_after_churn"]["within_estimated_bounds"],
            true
        );
        let consumed = super::inventory_report(
            &manifest,
            &phases[5]["relations"],
            &phases,
            1,
            46000,
            "cohort",
        )
        .unwrap();
        assert!(consumed["proposed_counts"].is_null());
        assert_eq!(consumed["status"], "inventory_prepared");
        assert_ne!(consumed["status"], "seed_qualified");
    }

    #[test]
    fn lock_contention_integrates_session_time_and_refuses_missing_coverage() {
        // Two waiting sessions for one minute are 120 session-seconds;
        // a linear zero-to-twelve ramp is 360, independent of sample count.
        for (ramp, expected) in [(false, 120_000_000_000), (true, 360_000_000_000)] {
            let samples: Vec<_> = (0..=12_u64).map(|sample|serde_json::json!({
                "event":"database_counters","elapsed_ns":sample*5_000_000_000_u64,
                "sample":{"activity":[{"wait_class":"Lock","sessions":if ramp {sample} else {2}},{"wait_class":"running","sessions":8}]}
            })).collect();
            let full: Vec<_> = samples.iter().collect();
            assert_eq!(super::first_minute_lock_contention(&full), Some(expected));
            let incomplete: Vec<_> = samples
                .iter()
                .enumerate()
                .filter(|(index, _)| ![5, 6].contains(index))
                .map(|(_, value)| value)
                .collect();
            assert_eq!(super::first_minute_lock_contention(&incomplete), None);
        }
    }

    #[test]
    fn dropped_and_timed_out_arrivals_remain_in_denominator_and_latency_tail() {
        let records = [
            OperationRecord {
                id: 1,
                family: Family::Jobs,
                segment: Segment::Steady,
                planned_ns: 100,
                started_ns: Some(20_000_100),
                completed_ns: 70_000_100,
                outcome: Outcome::Expected,
                expected: "inspect".into(),
                actual: "expected".into(),
            },
            OperationRecord {
                id: 2,
                family: Family::Jobs,
                segment: Segment::Steady,
                planned_ns: 100,
                started_ns: None,
                completed_ns: 100,
                outcome: Outcome::NotStarted,
                expected: "inspect".into(),
                actual: "no slot".into(),
            },
            OperationRecord {
                id: 3,
                family: Family::Jobs,
                segment: Segment::Steady,
                planned_ns: 100,
                started_ns: Some(100),
                completed_ns: 2_000_000_100,
                outcome: Outcome::TimedOut,
                expected: "inspect".into(),
                actual: "unknown after deadline".into(),
            },
        ];
        let summary = summarize_operations(&records);
        assert_eq!(summary.scheduled, 3);
        assert_eq!(summary.expected_within_deadline, 1);
        assert_eq!((summary.not_started, summary.timed_out), (1, 1));
        assert_eq!(
            (summary.p50_ns, summary.p99_ns, summary.max_ns),
            (2_000_000_000, 2_000_000_000, 2_000_000_000)
        );
        // The successful operation spent 50 ms executing and 20 ms waiting for
        // its scheduled dispatch. Both belong to the user's latency.
        assert_eq!(summarize_operations(&records[..1]).p99_ns, 70_000_000);
    }

    #[test]
    fn pressured_inventory_alone_cannot_qualify_without_body_coverage_and_physical_reads() {
        let mut observation = qualified_observation(Regime::Pressured);
        observation.distinct_full_body_bytes = 0;
        observation.container_block_read_bytes = 0;
        assert!(!observation.unmet(Regime::Pressured).is_empty());
        observation.distinct_full_body_bytes = 1536 * 1024 * 1024;
        assert!(!observation.unmet(Regime::Pressured).is_empty());
        observation.container_block_read_bytes = 256 * 1024 * 1024;
        assert!(observation.unmet(Regime::Pressured).is_empty());
    }

    #[test]
    fn unsupported_last_dimension_does_not_promote_an_unmeasured_predecessor() {
        // Final P3 comparison has three runs of P0/P2/P3, but only screening
        // for P1. P3 ties P2. P2's baseline benefit cannot supply its missing
        // incremental P1 comparison, so only the qualified baseline remains.
        let mut reports = Vec::new();
        for regime in REGIMES {
            for repeat in 1..=3 {
                reports.push(qualified_cell(Policy::P0, regime, repeat, 80));
                reports.push(qualified_cell(
                    Policy::P2,
                    regime,
                    repeat,
                    if repeat == 1 { 80 } else { 60 },
                ));
                reports.push(qualified_cell(Policy::P3, regime, repeat, 60));
            }
            reports.push(qualified_cell(Policy::P1, regime, 1, 80));
        }
        assert_eq!(select_policy(&reports, Policy::P3).policy, Some(Policy::P0));
    }

    #[test]
    fn incomplete_or_out_of_plan_evidence_cannot_select_a_policy() {
        let mut reports = Vec::new();
        for regime in REGIMES {
            for policy in [Policy::P0, Policy::P1, Policy::P2, Policy::P3] {
                reports.push(qualified_cell(policy, regime, 1, 80));
            }
            for repeat in 2..=3 {
                reports.push(qualified_cell(Policy::P0, regime, repeat, 80));
                reports.push(qualified_cell(Policy::P1, regime, repeat, 80));
            }
        }
        assert_eq!(selection_report(&reports)["selection"]["policy"], "P0");
        let mut missing_arrival = reports.clone();
        missing_arrival[0].segments[0]
            .families
            .get_mut(&Family::Jobs)
            .unwrap()
            .scheduled -= 1;
        assert!(select_policy(&missing_arrival, Policy::P0).policy.is_none());
        let mut duplicate = reports.clone();
        duplicate.push(reports[0].clone());
        assert_eq!(
            selection_report(&duplicate)["status"],
            "inadmissible_matrix"
        );
        reports.push(qualified_cell(Policy::P2, Regime::Resident, 2, 50));
        assert!(select_policy(&reports, Policy::P0).policy.is_none());
        assert_eq!(selection_report(&reports)["status"], "inadmissible_matrix");
    }

    #[test]
    fn cancelled_future_arrivals_stay_visible_and_a_missing_receipt_prevents_a_report() {
        let mut events = Vec::new();
        for (count, interval, wide) in [(6000, 10_000_000, false), (3840, 15_625_000, true)] {
            for tick in 0..count {
                let family = if wide {
                    Family::WideReplay
                } else {
                    match tick % 100 {
                        0..=29 => Family::Jobs,
                        30..=79 => Family::Idempotency,
                        _ => Family::Webhook,
                    }
                };
                let id = tick * 2 + u64::from(wide);
                let planned = tick * interval;
                events.push(serde_json::json!({"event":"operation_action","segment":"Warmup","id":id,"action":{"kind":"cancelled_fixture","family":family}}));
                events.push(serde_json::json!({"event":"planned","segment":"Warmup","id":id,"family":family,"planned_ns":planned,"expected":"cancelled_fixture"}));
                events.push(serde_json::json!({"event":"completed","segment":"Warmup","record":OperationRecord {id,family,segment:Segment::Warmup,planned_ns:planned,started_ns:None,completed_ns:0,outcome:Outcome::NotStarted,expected:"cancelled_fixture".into(),actual:"cancelled_before_admission".into()}}));
            }
        }
        let report = super::segment_series(&events, Segment::Warmup, 60).unwrap();
        assert_eq!(report["minutes"][0]["families"]["Jobs"]["scheduled"], 1800);
        assert_eq!(
            report["minutes"][0]["families"]["Jobs"]["not_started"],
            1800
        );
        assert_eq!(
            report["minutes"][0]["families"]["WideReplay"]["p99_ns"],
            2_000_000_000_u64
        );
        events.pop();
        assert!(
            super::segment_series(&events, Segment::Warmup, 60)
                .unwrap_err()
                .contains("scheduled accounting incomplete")
        );
    }

    #[test]
    fn calibration_requires_both_overhead_thresholds_and_the_fixed_paired_inputs() {
        let mut runs = calibration_runs(84);
        // 4 ms on 80 ms crosses neither threshold. A 9 ms increase crosses both.
        assert_eq!(
            super::calibration_report(&runs)["status"],
            "calibration_criteria_met"
        );
        runs[1]["families"]["Jobs"]["p99_ns"] = serde_json::json!(89_000_000);
        runs[1]["families"]["Jobs"]["max_ns"] = serde_json::json!(89_000_000);
        assert_eq!(
            super::calibration_report(&runs)["status"],
            "reopen_observer_design"
        );
        runs[1]["manifest"]["effective_inputs"]["inventory_seed"] = serde_json::json!(41002);
        assert_eq!(
            super::calibration_report(&runs)["status"],
            "calibration_incomplete"
        );
        let mut timeout = calibration_runs(80);
        timeout[2]["sampling_failures"] = serde_json::json!(1);
        assert_eq!(
            super::calibration_report(&timeout)["status"],
            "reopen_observer_design"
        );
        timeout.swap(2, 3);
        assert_eq!(
            super::calibration_report(&timeout)["status"],
            "calibration_incomplete"
        );
    }

    fn calibration_runs(observed_ms: u64) -> Vec<serde_json::Value> {
        [(Regime::Resident,"foundation"),(Regime::Resident,"observers"),(Regime::Pressured,"observers"),(Regime::Pressured,"foundation")].into_iter().map(|(regime,instrumentation)| {
            let p99 = if instrumentation == "foundation" {80} else {observed_ms};
            let families: std::collections::BTreeMap<_,_> = FAMILIES.into_iter().map(|family| {
                let scheduled = super::arrivals_per_second(family) * 120;
                (family,Distribution {scheduled,expected_within_deadline:scheduled,p50_ns:20_000_000,p95_ns:50_000_000,p99_ns:p99*1_000_000,max_ns:p99*1_000_000,..Distribution::default()})
            }).collect();
            serde_json::json!({
                "manifest":super::Manifest {
                    config:Config { attempt_id:format!("{regime:?}-{instrumentation}"),policy:Policy::P0,regime,repeat:1,seed:41001 },
                    source_tree_hash:format!("source-{instrumentation}"),executable_sha256:format!("executable-{instrumentation}"),policy_patch_sha256:"P0-patch".into(),image_digest:"image".into(),toolchain:"fixed".into(),features:vec!["integration".into()],target_identity:"one-target".into(),
                    effective_inputs:serde_json::json!({"inventory_seed":41001,"inventory_generation":3,"live_bodies":4096,"jobs_rows":5000,"receipt_rows":5000,"idempotency_rows":5000,"inputs_hash":"frozen","instrumentation":instrumentation}),
                },"duration_seconds":120,"ordinary_gaps":[],"families":families,"sampling_failures":0,
            })
        }).collect()
    }

    #[test]
    fn two_favorable_pairs_cannot_hide_an_opposing_third_pair() {
        let mut reports = Vec::new();
        for regime in REGIMES {
            for (repeat, candidate_ms) in [(1, 60), (2, 60), (3, 82)] {
                reports.push(qualified_cell(Policy::P0, regime, repeat, 80));
                reports.push(qualified_cell(Policy::P1, regime, repeat, candidate_ms));
            }
            reports.push(qualified_cell(Policy::P2, regime, 1, 80));
            reports.push(qualified_cell(Policy::P3, regime, 1, 80));
        }
        assert_eq!(shortlist(&reports).policy, Some(Policy::P1));
        let report = selection_report(&reports);
        assert_eq!(report["status"], "policy_selected");
        assert_eq!(report["selection"]["policy"], "P0");
    }

    fn qualified_observation(regime: Regime) -> Qualification {
        let pressured = regime == Regime::Pressured;
        Qualification {
            retained_bytes: if pressured {
                3 * 1024 * 1024 * 1024
            } else {
                128 * 1024 * 1024
            },
            shared_buffers_bytes: 512 * 1024 * 1024,
            pg_memory_limit_bytes: 1024 * 1024 * 1024,
            relation_toast_hits: if pressured { 95 } else { 100 },
            relation_toast_reads: if pressured { 5 } else { 0 },
            distinct_full_body_bytes: if pressured {
                1536 * 1024 * 1024
            } else {
                64 * 1024 * 1024
            },
            live_body_cohort_bytes: if pressured {
                2048 * 1024 * 1024
            } else {
                64 * 1024 * 1024
            },
            container_block_read_bytes: if pressured { 256 * 1024 * 1024 } else { 0 },
            native_inserts: 1,
            native_updates: 1,
            native_deletes: 1,
            full_body_content_checked: true,
            jobs_and_receipts_observed: true,
            maintenance_overlapped: true,
            inputs_unchanged: true,
            records_complete: true,
            uncontended_host: true,
            monotonic_clock: true,
        }
    }

    fn qualified_cell(policy: Policy, regime: Regime, repeat: u8, p99_ms: u64) -> CellReport {
        let distribution = |family, first_minute| {
            let per_second = match family {
                Family::Jobs => 30,
                Family::Idempotency => 50,
                Family::Webhook => 20,
                Family::WideReplay => 64,
            };
            let scheduled = per_second * if first_minute { 60 } else { 150 };
            Distribution {
                scheduled,
                expected_within_deadline: scheduled,
                p50_ns: 20_000_000,
                p95_ns: 50_000_000,
                p99_ns: p99_ms * 1_000_000,
                max_ns: p99_ms * 1_000_000,
                ..Distribution::default()
            }
        };
        let segments = SEGMENTS
            .into_iter()
            .map(|segment| SegmentReport {
                segment,
                qualification: qualified_observation(regime),
                families: FAMILIES
                    .into_iter()
                    .map(|family| (family, distribution(family, false)))
                    .collect(),
                first_minute: FAMILIES
                    .into_iter()
                    .map(|family| (family, distribution(family, true)))
                    .collect(),
                backlog: [Family::Jobs, Family::Idempotency, Family::Webhook]
                    .into_iter()
                    .map(|family| {
                        (
                            family,
                            Backlog {
                                initial: if segment == Segment::CatchUp {
                                    10_000
                                } else {
                                    0
                                },
                                final_count: 0,
                                delay_p95_ms: 60_000,
                                delay_max_ms: 100_000,
                                completed_cycle_troughs: vec![100, 50, 0],
                                initial_cohort_size: if segment == Segment::CatchUp {
                                    10_000
                                } else {
                                    0
                                },
                                initial_cohort_remaining_120s: 0,
                                initial_cohort_remaining_150s: 0,
                                drain_90_percent_ms: if segment == Segment::CatchUp {
                                    Some(90_000)
                                } else {
                                    None
                                },
                            },
                        )
                    })
                    .collect(),
                correctness_violations: 0,
                hidden_retries: 0,
                maximum_sample_age_ms: Some(30_000),
                all_expected_populations_observed: true,
                sampling_deadline_failures: 0,
                recovery_readable: true,
                owners_stopped: true,
                first_minute_contention_ns: None,
            })
            .collect();
        let config = Config {
            attempt_id: format!("{policy:?}-{regime:?}-{repeat}"),
            policy,
            regime,
            repeat,
            seed: 41000 + u64::from(repeat),
        };
        let manifest = super::Manifest {
            config: config.clone(),
            source_tree_hash: format!("source-{policy:?}"),
            executable_sha256: format!("executable-{policy:?}"),
            policy_patch_sha256: format!("patch-{policy:?}"),
            image_digest: "image".into(),
            toolchain: "fixed".into(),
            features: vec!["integration".into()],
            target_identity: "one-target".into(),
            effective_inputs: serde_json::json!({"inventory_seed":41001,"inventory_generation":3,"live_bodies":4096,"jobs_rows":5000,"receipt_rows":5000,"idempotency_rows":5000,"inputs_hash":"frozen","workload_hash":"workload"}),
        };
        CellReport {
            config,
            manifest: Some(manifest),
            segments,
            invalid_reason: None,
        }
    }
}
