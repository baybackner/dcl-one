//! The log ring `/scene_logs` reads: one process-wide ring of the last [`CAPACITY`] lines, each
//! with a monotonic `seq`, its severity and its source, a scene (by hash) or the client (by
//! tracing target). Scenes feed it from `RendererSceneContext::log`; the client feeds it through
//! [`LogRingLayer`], warnings and errors only, so the engine's own info lines never evict a
//! scene's. A reader passes the `latestSeq` of its last read as `since` and gets only newer lines;
//! `dropped` counts the lines newer than its cursor that the ring evicted before it read them.

use std::{
    collections::VecDeque,
    fmt::{Debug, Write},
    sync::{LazyLock, Mutex},
};

use bevy::log::{
    tracing::{
        field::{Field, Visit},
        Event, Level, Subscriber,
    },
    tracing_subscriber::{layer::Context, Layer},
};

/// Lines the ring keeps.
pub const CAPACITY: usize = 1000;

/// The target a scene's own lines are echoed to stdout under; the layer leaves them to the scene
/// path, which records them with their scene.
pub const SCENE_ECHO_TARGET: &str = "scene_runner::renderer_context";

static RING: LazyLock<Mutex<LogRing>> = LazyLock::new(|| Mutex::new(LogRing::new(CAPACITY)));

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Severity {
    Trace,
    Debug,
    Info,
    Warn,
    Error,
}

impl Severity {
    pub fn parse(name: &str) -> Option<Self> {
        Some(match name {
            "trace" => Self::Trace,
            "debug" => Self::Debug,
            "info" => Self::Info,
            "warn" => Self::Warn,
            "error" => Self::Error,
            _ => return None,
        })
    }

    pub fn name(self) -> &'static str {
        match self {
            Self::Trace => "trace",
            Self::Debug => "debug",
            Self::Info => "info",
            Self::Warn => "warn",
            Self::Error => "error",
        }
    }

    fn of(level: &Level) -> Self {
        match *level {
            Level::TRACE => Self::Trace,
            Level::DEBUG => Self::Debug,
            Level::INFO => Self::Info,
            Level::WARN => Self::Warn,
            _ => Self::Error,
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum Source {
    /// a scene, by its hash
    Scene(String),
    /// the client, by the event's tracing target
    Client(String),
}

#[derive(Clone, Debug)]
pub struct LogLine {
    pub seq: u64,
    pub severity: Severity,
    pub source: Source,
    /// a scene line's scene time, in seconds
    pub time: Option<f64>,
    /// a scene line's level as the scene runtime names it (`Log`, `SceneError`, `SystemError`)
    pub label: &'static str,
    pub text: String,
}

/// What a read selects.
#[derive(Clone, Debug)]
pub struct LogQuery {
    /// the scene whose lines are read
    pub scene: Option<String>,
    /// the client's lines too
    pub client: bool,
    /// only lines after this seq
    pub since: Option<u64>,
    /// the least severity read
    pub min: Severity,
    /// at most this many lines, the newest
    pub count: Option<usize>,
}

/// A read: the lines, oldest first, and what the header reports.
#[derive(Debug)]
pub struct LogPage {
    /// the newest seq in the ring, of any source; 0 while it is empty
    pub latest_seq: u64,
    /// lines newer than `since` (of any source) the ring evicted
    pub dropped: u64,
    /// lines the query selected that `count` left out (the oldest)
    pub omitted: usize,
    pub client: bool,
    pub lines: Vec<LogLine>,
}

pub struct LogRing {
    lines: VecDeque<LogLine>,
    next_seq: u64,
    capacity: usize,
}

impl LogRing {
    pub fn new(capacity: usize) -> Self {
        Self {
            lines: VecDeque::with_capacity(capacity),
            next_seq: 1,
            capacity,
        }
    }

    /// Adds a line and returns its seq.
    pub fn push(
        &mut self,
        severity: Severity,
        source: Source,
        time: Option<f64>,
        label: &'static str,
        text: String,
    ) -> u64 {
        let seq = self.next_seq;
        self.next_seq += 1;
        if self.lines.len() == self.capacity {
            self.lines.pop_front();
        }
        self.lines.push_back(LogLine {
            seq,
            severity,
            source,
            time,
            label,
            text,
        });
        seq
    }

    pub fn read(&self, query: &LogQuery) -> LogPage {
        let since = query.since.unwrap_or(0);
        let oldest = self.lines.front().map_or(self.next_seq, |line| line.seq);
        let selected: Vec<&LogLine> = self
            .lines
            .iter()
            .filter(|line| line.seq > since && line.severity >= query.min)
            .filter(|line| match &line.source {
                Source::Scene(hash) => query.scene.as_ref() == Some(hash),
                Source::Client(_) => query.client,
            })
            .collect();
        let keep = query.count.unwrap_or(usize::MAX).min(selected.len());
        let omitted = selected.len() - keep;
        LogPage {
            latest_seq: self.next_seq - 1,
            dropped: oldest.saturating_sub(since + 1),
            omitted,
            client: query.client,
            lines: selected[omitted..]
                .iter()
                .map(|line| (*line).clone())
                .collect(),
        }
    }
}

/// Records a scene's line in the process ring.
pub fn push_scene(hash: &str, severity: Severity, label: &'static str, time: f64, text: &str) {
    if let Ok(mut ring) = RING.lock() {
        ring.push(
            severity,
            Source::Scene(hash.to_owned()),
            Some(time),
            label,
            text.to_owned(),
        );
    }
}

/// Reads the process ring.
pub fn read(query: &LogQuery) -> LogPage {
    RING.lock()
        .map(|ring| ring.read(query))
        .unwrap_or_else(|poisoned| poisoned.into_inner().read(query))
}

impl LogPage {
    /// The reply: a header line `latestSeq=N dropped=N source=scene|scene+client` (and
    /// ` omitted=N` when `count` cut lines), then one line per entry. `plain` keeps the line shape
    /// `/scene_logs` had before the cursor (`<seq> [<time>] <Level>: <text>`); otherwise a line is
    /// `<seq> [scene]|[client:<target>] <severity> <text>`. A line's text continues on lines
    /// indented four spaces, so no text can pass for an entry or a header.
    pub fn render(&self, plain: bool) -> String {
        let mut out = format!(
            "latestSeq={} dropped={} source={}",
            self.latest_seq,
            self.dropped,
            if self.client { "scene+client" } else { "scene" }
        );
        if self.omitted > 0 {
            let _ = write!(out, " omitted={}", self.omitted);
        }
        for line in &self.lines {
            let text = line.text.trim_end().replace('\n', "\n    ");
            let _ = match (&line.source, plain) {
                (Source::Scene(_), true) => write!(
                    out,
                    "\n{} [{:.2}] {}: {text}",
                    line.seq,
                    line.time.unwrap_or_default(),
                    line.label
                ),
                (Source::Scene(_), false) => {
                    write!(
                        out,
                        "\n{} [scene] {} {text}",
                        line.seq,
                        line.severity.name()
                    )
                }
                (Source::Client(target), _) => write!(
                    out,
                    "\n{} [client:{target}] {} {text}",
                    line.seq,
                    line.severity.name()
                ),
            };
        }
        out
    }
}

/// The tracing layer that records the client's warnings and errors in the process ring.
pub struct LogRingLayer;

impl<S: Subscriber> Layer<S> for LogRingLayer {
    fn on_event(&self, event: &Event<'_>, _: Context<'_, S>) {
        let metadata = event.metadata();
        // tracing orders levels by verbosity: WARN is "greater" than ERROR
        if *metadata.level() > Level::WARN || metadata.target() == SCENE_ECHO_TARGET {
            return;
        }
        let mut message = Message::default();
        event.record(&mut message);
        let target = message
            .log_target
            .take()
            .unwrap_or_else(|| metadata.target().to_owned());
        let text = format!("{}{}", message.text, message.others);
        let text = perf_output::scrub::credentials(&text).into_owned();
        if let Ok(mut ring) = RING.lock() {
            ring.push(
                Severity::of(metadata.level()),
                Source::Client(target),
                None,
                "",
                text,
            );
        }
    }
}

/// An event's message and its other fields as ` name=value`, and the `log` bridge's target.
#[derive(Default)]
struct Message {
    text: String,
    others: String,
    log_target: Option<String>,
}

impl Visit for Message {
    fn record_str(&mut self, field: &Field, value: &str) {
        match field.name() {
            "message" => self.text.push_str(value),
            "log.target" => self.log_target = Some(value.to_owned()),
            name if name.starts_with("log.") => {}
            _ => self.record_debug(field, &value),
        }
    }

    fn record_debug(&mut self, field: &Field, value: &dyn Debug) {
        match field.name() {
            "message" => {
                let _ = write!(self.text, "{value:?}");
            }
            name if name.starts_with("log.") => {}
            name => {
                let _ = write!(self.others, " {name}={value:?}");
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ring() -> LogRing {
        let mut ring = LogRing::new(4);
        let scene = || Source::Scene("bafyA".to_owned());
        ring.push(Severity::Info, scene(), Some(1.0), "Log", "a".into());
        ring.push(
            Severity::Warn,
            Source::Client("comms".into()),
            None,
            "",
            "w".into(),
        );
        ring.push(
            Severity::Error,
            scene(),
            Some(2.0),
            "SceneError",
            "b\nc".into(),
        );
        ring.push(
            Severity::Info,
            Source::Scene("bafyB".into()),
            Some(3.0),
            "Log",
            "x".into(),
        );
        ring
    }

    fn query() -> LogQuery {
        LogQuery {
            scene: Some("bafyA".into()),
            client: false,
            since: None,
            min: Severity::Trace,
            count: None,
        }
    }

    #[test]
    fn a_read_selects_the_scene_and_the_client_on_request() {
        let ring = ring();
        let page = ring.read(&query());
        assert_eq!(page.latest_seq, 4);
        assert_eq!(page.lines.iter().map(|l| l.seq).collect::<Vec<_>>(), [1, 3]);
        let page = ring.read(&LogQuery {
            client: true,
            ..query()
        });
        assert_eq!(
            page.lines.iter().map(|l| l.seq).collect::<Vec<_>>(),
            [1, 2, 3]
        );
    }

    #[test]
    fn since_severity_and_count_narrow_a_read() {
        let ring = ring();
        let after = ring.read(&LogQuery {
            since: Some(1),
            client: true,
            ..query()
        });
        assert_eq!(
            after.lines.iter().map(|l| l.seq).collect::<Vec<_>>(),
            [2, 3]
        );
        let errors = ring.read(&LogQuery {
            min: Severity::Error,
            client: true,
            ..query()
        });
        assert_eq!(errors.lines.iter().map(|l| l.seq).collect::<Vec<_>>(), [3]);
        let last = ring.read(&LogQuery {
            count: Some(1),
            ..query()
        });
        assert_eq!((last.omitted, last.lines[0].seq), (1, 3));
    }

    #[test]
    fn dropped_counts_what_the_ring_evicted_after_the_cursor() {
        let mut ring = ring();
        assert_eq!(ring.read(&query()).dropped, 0);
        for _ in 0..3 {
            ring.push(
                Severity::Info,
                Source::Client("x".into()),
                None,
                "",
                "n".into(),
            );
        }
        // seqs 1 to 3 are gone
        assert_eq!(ring.read(&query()).dropped, 3);
        assert_eq!(
            ring.read(&LogQuery {
                since: Some(2),
                ..query()
            })
            .dropped,
            1
        );
        assert_eq!(
            ring.read(&LogQuery {
                since: Some(5),
                ..query()
            })
            .dropped,
            0
        );
    }

    #[test]
    fn a_page_renders_a_header_and_indents_continuations() {
        let page = ring().read(&LogQuery {
            client: true,
            ..query()
        });
        assert_eq!(
            page.render(false),
            "latestSeq=4 dropped=0 source=scene+client\n1 [scene] info a\n2 [client:comms] warn \
             w\n3 [scene] error b\n    c"
        );
        let plain = ring().read(&query()).render(true);
        assert_eq!(
            plain,
            "latestSeq=4 dropped=0 source=scene\n1 [1.00] Log: a\n3 [2.00] SceneError: b\n    c"
        );
    }
}
