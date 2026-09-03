use std::collections::BTreeMap;
use std::env;
use std::hint::black_box;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use agentwerk::{Event, Query, Task, Werk};
use serde_json::json;

const EVENTS_PER_TASK: usize = 10;
const FIXTURE_SIZES: [usize; 3] = [100, 1_000, 10_000];
const DEFAULT_SAMPLES: usize = 10;
const CALIBRATION_TIME: Duration = Duration::from_millis(20);
const WARM_UP_TIME: Duration = Duration::from_millis(200);
const TAIL_EVENT: &str = "benchmark_tail";
const SYNTHETIC_EVENTS: [&str; EVENTS_PER_TASK - 1] = [
    Event::TOOL_CALL_STARTED,
    Event::TOOL_CALL_FINISHED,
    Event::TOOL_CALL_FAILED,
    Event::REQUEST_STARTED,
    Event::REQUEST_FINISHED,
    Event::TURN_STARTED,
    "document_selected",
    "document_indexed",
    "document_archived",
];

fn main() {
    let config = Config::from_args().unwrap_or_else(|message| {
        eprintln!("{message}");
        eprintln!("Run with --help for usage.");
        std::process::exit(2);
    });
    if config.help {
        print_help();
        return;
    }
    let mut runner = Runner::new(config).unwrap_or_else(|message| {
        eprintln!("{message}");
        std::process::exit(2);
    });
    benchmarks(&mut runner);
    runner.finish().unwrap_or_else(|error| {
        eprintln!("could not save benchmark baseline: {error}");
        std::process::exit(1);
    });
}

fn benchmarks(runner: &mut Runner) {
    benchmark_compilation(runner);

    let fixtures = FIXTURE_SIZES.map(LazyFixture::new);
    benchmark_scaling(runner, &fixtures);
    let large = fixtures.last().expect("at least one AQL fixture");
    benchmark_task_selection(runner, large);
    benchmark_event_selection(runner, large);
    benchmark_cross_origin_and_joined(runner, large);
}

struct Fixture {
    _dir: TempDir,
    werk: Arc<Werk>,
    events_path: PathBuf,
    task_count: usize,
    event_count: usize,
    scan_count: usize,
    rare_count: usize,
}

struct LazyFixture {
    task_count: usize,
    fixture: OnceLock<Fixture>,
}

impl LazyFixture {
    fn new(task_count: usize) -> Self {
        Self {
            task_count,
            fixture: OnceLock::new(),
        }
    }

    fn get(&self) -> &Fixture {
        self.fixture.get_or_init(|| Fixture::new(self.task_count))
    }
}

impl Fixture {
    fn new(task_count: usize) -> Self {
        assert_eq!(task_count % 100, 0, "fixture counts assume full cycles");

        eprintln!(
            "Building fixture with {task_count} tasks and {} events...",
            task_count * EVENTS_PER_TASK
        );
        let dir = TempDir::new().expect("create AQL benchmark directory");
        let werk = Werk::new();
        werk.set_dir(dir.path());
        werk.on_event(|_, _| {});

        let task_body = "t".repeat(512);
        let event_body = "e".repeat(256);
        for task_index in 0..task_count {
            let marker = if task_index % 10 == 0 {
                "needle"
            } else {
                "haystack"
            };
            let id = werk.add_task(Task::labeled(
                task_label(task_index),
                json!({
                    "index": task_index,
                    "marker": marker,
                    "body": task_body,
                }),
            ));

            for (event_index, event_name) in SYNTHETIC_EVENTS.iter().enumerate() {
                let event_name =
                    if task_index + 1 == task_count && event_index + 1 == SYNTHETIC_EVENTS.len() {
                        TAIL_EVENT
                    } else {
                        event_name
                    };
                let marker = if (task_index + event_index) % 10 == 0 {
                    "needle"
                } else {
                    "haystack"
                };
                werk.emit_event(
                    Event::new(event_name)
                        .task_id(&id)
                        .agent_id(format!("agent-{}", task_index % 8))
                        .data(json!({
                            "index": event_index,
                            "marker": marker,
                            "body": event_body,
                        })),
                );
            }
        }

        let fixture = Self {
            _dir: dir,
            events_path: werk.get_dir().join("events.jsonl"),
            werk,
            task_count,
            event_count: task_count * EVENTS_PER_TASK,
            scan_count: task_count / 4 - task_count / 100,
            rare_count: task_count / 100,
        };
        fixture.validate();
        fixture
    }

    fn validate(&self) {
        assert_eq!(self.werk.get_tasks().len(), self.task_count);
        assert_eq!(read_event_log(&self.events_path).len(), self.event_count);
        assert_eq!(
            self.werk.find_tasks("task.label = scan").len(),
            self.scan_count
        );
        assert_eq!(
            self.werk.find_tasks("task.label = rare").len(),
            self.rare_count
        );
        assert_eq!(
            self.werk.find_events("event.name = tool_call_failed").len(),
            self.task_count
        );
        assert_eq!(
            self.werk
                .find_events("task.label = scan AND event.name = tool_call_failed")
                .len(),
            self.scan_count
        );
        assert_eq!(
            self.werk
                .find_events(format!("event.name = {TAIL_EVENT}"))
                .len(),
            1
        );
    }
}

fn task_label(index: usize) -> &'static str {
    if index.is_multiple_of(100) {
        return "rare";
    }
    match index % 4 {
        0 => "scan",
        1 => "review",
        2 => "report",
        _ => "archive",
    }
}

fn read_event_log(path: &Path) -> Vec<Event> {
    std::fs::read_to_string(path)
        .expect("read benchmark event log")
        .lines()
        .map(|line| serde_json::from_str(line).expect("deserialize benchmark event"))
        .collect()
}

fn benchmark_compilation(runner: &mut Runner) {
    let queries = [
        ("compile/simple", "task.label = scan".to_string()),
        (
            "compile/compound",
            "NOT (task.label IN (scan, review) OR task.status = failed) \
             AND task.assignee IS EMPTY ORDER BY task.created DESC"
                .to_string(),
        ),
        (
            "compile/joined",
            "task.label = scan AND event.name = tool_call_failed \
             ORDER BY event.created DESC"
                .to_string(),
        ),
        (
            "compile/membership_64",
            format!(
                "task.id IN ({})",
                (1..=64)
                    .map(|id| format!("t-{id}"))
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
        ),
    ];
    for (name, query) in queries {
        let work = Work::bytes(query.len());
        runner.bench(name, work, || {
            black_box(Query::new(black_box(&query)).expect("valid benchmark query"));
        });
    }
}

fn benchmark_task_selection(runner: &mut Runner, lazy: &LazyFixture) {
    const NAMES: [&str; 9] = [
        "task/find_tasks/label_closure",
        "task/find_tasks/label_aql_compiled",
        "task/find_tasks/label_aql_string",
        "task/find_tasks/json_text_compiled",
        "task/find_tasks/rare_default_order",
        "task/find_tasks/broad_default_order",
        "task/find_tasks/label_explicit_order",
        "task/find_task/rare_default_order",
        "task/find_task/rare_explicit_order",
    ];
    if !runner.any_selected(&NAMES) {
        return;
    }
    let fixture = lazy.get();
    let work = Work::records(fixture.task_count);
    let label = Query::new("task.label = scan").unwrap();
    let input = Query::new("task.input ~ needle").unwrap();
    let rare = Query::new("task.label = rare").unwrap();
    let all = Query::new("task.status = todo").unwrap();
    let rare_ordered = Query::new("task.label = rare ORDER BY task.id DESC").unwrap();
    let scan_ordered = Query::new("task.label = scan ORDER BY task.id DESC").unwrap();

    runner.bench(NAMES[0], work, || {
        black_box(
            fixture
                .werk
                .find_tasks(|task: &Task| task.get_label() == Some("scan")),
        );
    });
    runner.bench(NAMES[1], work, || {
        black_box(fixture.werk.find_tasks(label.clone()));
    });
    runner.bench(NAMES[2], work, || {
        black_box(fixture.werk.find_tasks("task.label = scan"));
    });
    runner.bench(NAMES[3], work, || {
        black_box(fixture.werk.find_tasks(input.clone()));
    });
    runner.bench(NAMES[4], work, || {
        black_box(fixture.werk.find_tasks(rare.clone()));
    });
    runner.bench(NAMES[5], work, || {
        black_box(fixture.werk.find_tasks(all.clone()));
    });
    runner.bench(NAMES[6], work, || {
        black_box(fixture.werk.find_tasks(scan_ordered.clone()));
    });
    runner.bench(NAMES[7], work, || {
        black_box(fixture.werk.find_task(rare.clone()));
    });
    runner.bench(NAMES[8], work, || {
        black_box(fixture.werk.find_task(rare_ordered.clone()));
    });
}

fn benchmark_event_selection(runner: &mut Runner, lazy: &LazyFixture) {
    const NAMES: [&str; 8] = [
        "event/storage/read_file",
        "event/storage/read_and_deserialize",
        "event/find_events/name_closure",
        "event/find_events/name_aql_compiled",
        "event/find_events/json_text_compiled",
        "event/find_events/name_explicit_order",
        "event/find_event/first_match",
        "event/find_event/tail_match",
    ];
    if !runner.any_selected(&NAMES) {
        return;
    }
    let fixture = lazy.get();
    let work = Work::records(fixture.event_count);
    let name = Query::new("event.name = tool_call_failed").unwrap();
    let data = Query::new("event.data ~ needle").unwrap();
    let ordered = Query::new("event.name = tool_call_failed ORDER BY event.created DESC").unwrap();
    let first = Query::new("event.name = task_created").unwrap();
    let tail = Query::new(&format!("event.name = {TAIL_EVENT}")).unwrap();

    runner.bench(NAMES[0], work, || {
        black_box(std::fs::read_to_string(&fixture.events_path).unwrap());
    });
    runner.bench(NAMES[1], work, || {
        black_box(read_event_log(&fixture.events_path));
    });
    runner.bench(NAMES[2], work, || {
        black_box(
            fixture
                .werk
                .find_events(|event: &Event| event.get_name() == Event::TOOL_CALL_FAILED),
        );
    });
    runner.bench(NAMES[3], work, || {
        black_box(fixture.werk.find_events(name.clone()));
    });
    runner.bench(NAMES[4], work, || {
        black_box(fixture.werk.find_events(data.clone()));
    });
    runner.bench(NAMES[5], work, || {
        black_box(fixture.werk.find_events(ordered.clone()));
    });
    runner.bench(NAMES[6], work, || {
        black_box(fixture.werk.find_event(first.clone()));
    });
    runner.bench(NAMES[7], work, || {
        black_box(fixture.werk.find_event(tail.clone()));
    });
}

fn benchmark_cross_origin_and_joined(runner: &mut Runner, lazy: &LazyFixture) {
    const NAMES: [&str; 7] = [
        "joined/find_events/task_selected",
        "joined/find_tasks/event_selected_deduplicated",
        "joined/find_events/scan_and_failed",
        "joined/find_tasks/scan_and_failed_deduplicated",
        "joined/find_events/rare_and_failed",
        "joined/find_events/scan_and_failed_ordered",
        "joined/find_tasks/scan_and_failed_ordered_deduplicated",
    ];
    if !runner.any_selected(&NAMES) {
        return;
    }
    let fixture = lazy.get();
    let work = Work::records(fixture.event_count);
    let task_to_events = Query::new("task.label = scan").unwrap();
    let event_to_tasks = Query::new("event.name = tool_call_failed").unwrap();
    let joined = Query::new("task.label = scan AND event.name = tool_call_failed").unwrap();
    let joined_rare = Query::new("task.label = rare AND event.name = tool_call_failed").unwrap();
    let joined_ordered = Query::new(
        "task.label = scan AND event.name = tool_call_failed ORDER BY event.created DESC",
    )
    .unwrap();

    runner.bench(NAMES[0], work, || {
        black_box(fixture.werk.find_events(task_to_events.clone()));
    });
    runner.bench(NAMES[1], work, || {
        black_box(fixture.werk.find_tasks(event_to_tasks.clone()));
    });
    runner.bench(NAMES[2], work, || {
        black_box(fixture.werk.find_events(joined.clone()));
    });
    runner.bench(NAMES[3], work, || {
        black_box(fixture.werk.find_tasks(joined.clone()));
    });
    runner.bench(NAMES[4], work, || {
        black_box(fixture.werk.find_events(joined_rare.clone()));
    });
    runner.bench(NAMES[5], work, || {
        black_box(fixture.werk.find_events(joined_ordered.clone()));
    });
    runner.bench(NAMES[6], work, || {
        black_box(fixture.werk.find_tasks(joined_ordered.clone()));
    });
}

fn benchmark_scaling(runner: &mut Runner, fixtures: &[LazyFixture]) {
    let task = Query::new("task.label = scan").unwrap();
    let event = Query::new("event.name = tool_call_failed").unwrap();
    let joined = Query::new("task.label = scan AND event.name = tool_call_failed").unwrap();

    for lazy in fixtures {
        let names = [
            format!("scale/task/find_tasks/{}", lazy.task_count),
            format!(
                "scale/event/find_events/{}",
                lazy.task_count * EVENTS_PER_TASK
            ),
            format!(
                "scale/joined/find_events/{}",
                lazy.task_count * EVENTS_PER_TASK
            ),
        ];
        if !names.iter().any(|name| runner.selected(name)) {
            continue;
        }
        let fixture = lazy.get();
        runner.bench(&names[0], Work::records(fixture.task_count), || {
            black_box(fixture.werk.find_tasks(task.clone()));
        });
        runner.bench(&names[1], Work::records(fixture.event_count), || {
            black_box(fixture.werk.find_events(event.clone()));
        });
        runner.bench(&names[2], Work::records(fixture.event_count), || {
            black_box(fixture.werk.find_events(joined.clone()));
        });
    }
}

#[derive(Clone, Copy)]
struct Work {
    amount: u64,
    unit: &'static str,
}

impl Work {
    fn records(amount: usize) -> Self {
        Self {
            amount: amount as u64,
            unit: "records/s",
        }
    }

    fn bytes(amount: usize) -> Self {
        Self {
            amount: amount as u64,
            unit: "bytes/s",
        }
    }
}

struct Runner {
    config: Config,
    baseline: BTreeMap<String, f64>,
    results: BTreeMap<String, f64>,
}

impl Runner {
    fn new(config: Config) -> Result<Self, String> {
        let baseline = match &config.baseline {
            Some(name) => load_baseline(name)?,
            None => BTreeMap::new(),
        };
        Ok(Self {
            config,
            baseline,
            results: BTreeMap::new(),
        })
    }

    fn selected(&self, name: &str) -> bool {
        match &self.config.filter {
            Some(filter) => name.contains(filter),
            None => true,
        }
    }

    fn any_selected(&self, names: &[&str]) -> bool {
        names.iter().any(|name| self.selected(name))
    }

    fn bench(&mut self, name: &str, work: Work, mut run: impl FnMut()) {
        if !self.selected(name) {
            return;
        }
        println!("Benchmarking {name}");
        if let Some(duration) = self.config.profile_time {
            let started = Instant::now();
            let mut iterations = 0_u64;
            while started.elapsed() < duration {
                run();
                iterations += 1;
            }
            let elapsed = started.elapsed();
            println!(
                "  profile: {iterations} iterations in {} ({})",
                format_duration(elapsed.as_nanos() as f64),
                format_rate(iterations as f64 / elapsed.as_secs_f64(), "iterations/s")
            );
            return;
        }

        let repetitions = calibrate(&mut run);
        let warm_up = Instant::now();
        while warm_up.elapsed() < WARM_UP_TIME {
            run_batch(repetitions, &mut run);
        }

        let mut samples = Vec::with_capacity(self.config.samples);
        for _ in 0..self.config.samples {
            let started = Instant::now();
            run_batch(repetitions, &mut run);
            samples.push(started.elapsed().as_nanos() as f64 / repetitions as f64);
        }
        samples.sort_by(f64::total_cmp);
        let median = median(&samples);
        let low = percentile(&samples, 10);
        let high = percentile(&samples, 90);
        let throughput = work.amount as f64 / (median / 1_000_000_000.0);
        println!(
            "  time: [{} {} {}]  throughput: {}",
            format_duration(low),
            format_duration(median),
            format_duration(high),
            format_rate(throughput, work.unit)
        );
        if let Some(previous) = self.baseline.get(name) {
            println!("  change: {:+.2}%", (median / previous - 1.0) * 100.0);
        }
        self.results.insert(name.to_string(), median);
    }

    fn finish(&self) -> io::Result<()> {
        if let Some(name) = &self.config.save_baseline {
            let path = baseline_path(name);
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent)?;
            }
            std::fs::write(&path, serde_json::to_vec_pretty(&self.results)?)?;
            println!("Saved baseline to {}", path.display());
        }
        Ok(())
    }
}

fn calibrate(run: &mut impl FnMut()) -> u64 {
    let mut repetitions = 1_u64;
    loop {
        let started = Instant::now();
        run_batch(repetitions, run);
        if started.elapsed() >= CALIBRATION_TIME || repetitions >= 1 << 30 {
            return repetitions;
        }
        repetitions *= 2;
    }
}

fn run_batch(repetitions: u64, run: &mut impl FnMut()) {
    for _ in 0..repetitions {
        run();
    }
}

fn median(samples: &[f64]) -> f64 {
    let middle = samples.len() / 2;
    if samples.len().is_multiple_of(2) {
        (samples[middle - 1] + samples[middle]) / 2.0
    } else {
        samples[middle]
    }
}

fn percentile(samples: &[f64], percentile: usize) -> f64 {
    samples[(samples.len() - 1) * percentile / 100]
}

fn format_duration(nanos: f64) -> String {
    if nanos >= 1_000_000_000.0 {
        format!("{:.3} s", nanos / 1_000_000_000.0)
    } else if nanos >= 1_000_000.0 {
        format!("{:.3} ms", nanos / 1_000_000.0)
    } else if nanos >= 1_000.0 {
        format!("{:.3} us", nanos / 1_000.0)
    } else {
        format!("{nanos:.3} ns")
    }
}

fn format_rate(rate: f64, unit: &str) -> String {
    if rate >= 1_000_000.0 {
        format!("{:.3} M{unit}", rate / 1_000_000.0)
    } else if rate >= 1_000.0 {
        format!("{:.3} K{unit}", rate / 1_000.0)
    } else {
        format!("{rate:.3} {unit}")
    }
}

#[derive(Default)]
struct Config {
    filter: Option<String>,
    samples: usize,
    profile_time: Option<Duration>,
    save_baseline: Option<String>,
    baseline: Option<String>,
    help: bool,
}

impl Config {
    fn from_args() -> Result<Self, String> {
        let mut config = Self {
            samples: DEFAULT_SAMPLES,
            ..Self::default()
        };
        let mut args = env::args().skip(1);
        while let Some(argument) = args.next() {
            match argument.as_str() {
                "-h" | "--help" => config.help = true,
                // Cargo passes this to every custom benchmark harness.
                "--bench" => {}
                "--samples" => {
                    config.samples = value(&mut args, "--samples")?
                        .parse()
                        .map_err(|_| "--samples must be a positive integer".to_string())?;
                    if config.samples == 0 {
                        return Err("--samples must be a positive integer".to_string());
                    }
                }
                "--profile-time" => {
                    let seconds: u64 = value(&mut args, "--profile-time")?
                        .parse()
                        .map_err(|_| "--profile-time must be positive seconds".to_string())?;
                    if seconds == 0 {
                        return Err("--profile-time must be positive seconds".to_string());
                    }
                    config.profile_time = Some(Duration::from_secs(seconds));
                }
                "--save-baseline" => {
                    config.save_baseline =
                        Some(baseline_name(value(&mut args, "--save-baseline")?)?);
                }
                "--baseline" => {
                    config.baseline = Some(baseline_name(value(&mut args, "--baseline")?)?);
                }
                option if option.starts_with('-') => {
                    return Err(format!("unknown option {option:?}"));
                }
                filter if config.filter.is_none() => config.filter = Some(filter.to_string()),
                filter => return Err(format!("unexpected second filter {filter:?}")),
            }
        }
        Ok(config)
    }
}

fn value(args: &mut impl Iterator<Item = String>, option: &str) -> Result<String, String> {
    args.next()
        .ok_or_else(|| format!("{option} requires a value"))
}

fn baseline_name(name: String) -> Result<String, String> {
    if !name.is_empty()
        && name
            .chars()
            .all(|character| character.is_ascii_alphanumeric() || "-_.".contains(character))
    {
        return Ok(name);
    }
    Err("baseline names may contain only letters, digits, '-', '_', and '.'".to_string())
}

fn load_baseline(name: &str) -> Result<BTreeMap<String, f64>, String> {
    let path = baseline_path(name);
    let body = std::fs::read_to_string(&path)
        .map_err(|error| format!("could not read baseline {}: {error}", path.display()))?;
    serde_json::from_str(&body)
        .map_err(|error| format!("could not parse baseline {}: {error}", path.display()))
}

fn baseline_path(name: &str) -> PathBuf {
    target_dir().join("aql-bench").join(format!("{name}.json"))
}

fn target_dir() -> PathBuf {
    env::var_os("CARGO_TARGET_DIR").map_or_else(
        || {
            Path::new(env!("CARGO_MANIFEST_DIR"))
                .parent()
                .and_then(Path::parent)
                .expect("agentwerk belongs to the workspace")
                .join("target")
        },
        PathBuf::from,
    )
}

fn print_help() {
    println!(
        "AQL performance benchmark\n\n\
         Usage: cargo bench -p agentwerk --bench aql -- [FILTER] [OPTIONS]\n\n\
         Options:\n\
           --samples N          Samples per benchmark (default: {DEFAULT_SAMPLES})\n\
           --profile-time SEC   Repeat matching benchmarks for the profiler\n\
           --save-baseline NAME Save median timings under target/aql-bench\n\
           --baseline NAME      Compare medians with a saved baseline\n\
           -h, --help           Print this help"
    );
}

struct TempDir {
    path: PathBuf,
}

impl TempDir {
    fn new() -> io::Result<Self> {
        let base = env::temp_dir();
        for _ in 0..16 {
            let candidate = base.join(format!("agentwerk-aql-{}-{}", std::process::id(), unique()));
            match std::fs::create_dir(&candidate) {
                Ok(()) => return Ok(Self { path: candidate }),
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
                Err(error) => return Err(error),
            }
        }
        Err(io::Error::new(
            io::ErrorKind::AlreadyExists,
            "could not allocate an AQL benchmark directory",
        ))
    }

    fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.path);
    }
}

fn unique() -> u64 {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_nanos() as u64)
        .unwrap_or(0);
    nanos.wrapping_add(COUNTER.fetch_add(1, Ordering::Relaxed))
}
