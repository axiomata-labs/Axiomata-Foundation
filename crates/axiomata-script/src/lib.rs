//! Scripts for the Axiomata apps: a model writes a short JavaScript or TypeScript that calls the host's tools as
//! functions, and only what the script prints or returns comes back — a large result is filtered inside the script
//! instead of riding along in every later step. Started as Glut's code mode (`../Axiomata-Glut`, `codemode.rs`, after
//! Pi's); planned in `docs/plans/plugins.md` and Lirvel's `companion.md` §8x.
//!
//! * **QuickJS through `rquickjs`**: the script is the body of an async function, so `await` and `return` work at the
//!   top. TypeScript loses its types first (oxc). No Node, no `fetch`, no `require`, no file system, no timers but
//!   `wait(seconds)` ([`Limits::max_wait`]), which the host may cut short ([`Nudge`]) — the only way out is
//!   `tools.<name>(args)`.
//! * **The host answers every call** ([`Request`]): this crate calls no tool of its own. Whatever guards a host has —
//!   its switches, its gate, asking its owner — a call from a script passes them as a direct call would.
//! * **No regular expressions**: the bundled quickjs-ng has an open advisory, GHSA-rxpx-wj25-jhqq (2026-07):
//!   compiling a regex after an out-of-memory writes past its buffer — in the host's process, from a script a prompt
//!   can steer. Neither `RegExp` nor regex literals exist until a fixed engine ships; scripts use the string methods.
//! * **Ending** ([`Ending`]): a script ends when it returns or throws, when the host stops it or its deadline passes
//!   (a busy loop, a pending call or a wait included), or when it waits for nothing — a promise no call and no wait
//!   will ever settle.
//!
//! [`run`] needs a tokio runtime with its timer, on a thread with at least 1 MiB of stack. The host answers its
//! [`Request`]s off the task that runs it (another task or thread): a tool that blocks it would hold the watch, and
//! with it the deadline and the stop. How many scripts run at once is the host's to decide: each holds a whole
//! JavaScript runtime of up to [`Limits::memory_bytes`].
//!
//! TypeScript is parsed as a module and then run inside a function: `namespace` is not supported.

mod catalog;
mod typescript;

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use rquickjs::context::intrinsic;
use rquickjs::prelude::Async;
use rquickjs::{
    AsyncContext, AsyncRuntime, CatchResultExt, Ctx, Function, Promise, Value as JsValue,
};
use serde_json::{Value, json};
use tokio::sync::{Notify, mpsc, oneshot};

pub use catalog::{Catalog, ToolSpec, declarations};

/// `store()`: one value, and all values together, in characters.
const MAX_STORED_VALUE_CHARS: usize = 262_144;
const MAX_STORED_TOTAL_CHARS: usize = 1_048_576;
/// `store()`: a key's length and how many keys are kept.
const MAX_STORED_KEY_CHARS: usize = 200;
const MAX_STORED_KEYS: usize = 1_000;
/// How often the watch looks whether a script was stopped, ran out of time or waits for nothing.
const TICK: Duration = Duration::from_millis(50);
/// Ticks in a row with no call open, no wait running and nothing happening before a script counts as waiting for
/// nothing: a script that can go on is polled at once when its call returns, so two quiet ticks are no slow script.
const QUIET_TICKS: u32 = 2;
/// The stack QuickJS may use: a thread running [`run`] needs this and its own frames, so at least 1 MiB.
const JS_STACK_BYTES: usize = 256 * 1024;
/// The line that stands for output dropped once the buffer is full.
const DROPPED: &str = "[… more output dropped: the script printed too much]";

/// The JavaScript a script gets: what QuickJS offers **but regular expressions** (see the crate's documentation).
type Intrinsics = (
    intrinsic::Date,
    intrinsic::Eval,
    intrinsic::Json,
    intrinsic::Proxy,
    intrinsic::MapSet,
    intrinsic::TypedArrays,
    intrinsic::Promise,
    intrinsic::WeakRef,
    intrinsic::DOMException,
);

/// The language a script is written in.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Language {
    /// JavaScript as QuickJS runs it.
    #[default]
    JavaScript,
    /// TypeScript: its types are removed before it runs; nothing else changes.
    TypeScript,
}

/// A script to run: its source and its language.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Script {
    /// The body of an async function: `await` and `return` work at the top.
    pub code: String,
    /// What `code` is written in.
    pub language: Language,
}

/// Why a tool call's arguments hold no script.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum ArgumentsError {
    /// The arguments are no JSON object.
    #[error("the arguments must be a JSON object")]
    NotAnObject,
    /// `code` is missing or empty.
    #[error("`code` (the script to run) is required")]
    NoCode,
    /// `language` names neither JavaScript nor TypeScript.
    #[error("`language` must be \"javascript\" or \"typescript\", not {0:?}")]
    Language(String),
}

impl Script {
    /// A JavaScript script.
    #[must_use]
    pub fn javascript(code: impl Into<String>) -> Self {
        Self {
            code: code.into(),
            language: Language::JavaScript,
        }
    }

    /// A TypeScript script.
    #[must_use]
    pub fn typescript(code: impl Into<String>) -> Self {
        Self {
            code: code.into(),
            language: Language::TypeScript,
        }
    }

    /// The script in a tool call's arguments, `{"code": "...", "language": "typescript"}` (`language` may be left
    /// out, or be `javascript`/`js`/`typescript`/`ts`). A markdown fence around the code is forgiven.
    ///
    /// # Errors
    ///
    /// [`ArgumentsError`] when the arguments are no object, hold no code or name another language.
    pub fn from_arguments(arguments: &Value) -> Result<Self, ArgumentsError> {
        let Value::Object(fields) = arguments else {
            return Err(ArgumentsError::NotAnObject);
        };
        let code = fields
            .get("code")
            .and_then(Value::as_str)
            .filter(|code| !code.trim().is_empty());
        let Some(code) = code else {
            return Err(ArgumentsError::NoCode);
        };
        let named = fields.get("language").and_then(Value::as_str);
        let language = match named.map(str::to_lowercase).as_deref() {
            None | Some("javascript" | "js") => Language::JavaScript,
            Some("typescript" | "ts") => Language::TypeScript,
            Some(_) => {
                return Err(ArgumentsError::Language(
                    named.unwrap_or_default().to_owned(),
                ));
            }
        };
        let code = strip_fence(code);
        if code.trim().is_empty() {
            return Err(ArgumentsError::NoCode);
        }
        Ok(Self {
            code: code.to_owned(),
            language,
        })
    }
}

/// A model that wraps the code in a markdown fence anyway is forgiven.
fn strip_fence(code: &str) -> &str {
    let trimmed = code.trim();
    let Some(rest) = trimmed.strip_prefix("```") else {
        return code;
    };
    let body = rest.split_once('\n').map_or("", |(_, body)| body);
    body.trim_end()
        .strip_suffix("```")
        .unwrap_or(body)
        .trim_end()
}

/// How far one script may go.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Limits {
    /// The memory its JavaScript runtime may take, in bytes.
    pub memory_bytes: usize,
    /// Tool calls it may make; past them a call is refused, the script goes on.
    pub max_calls: usize,
    /// Characters of its output the host gets; beyond them head and tail stay, with one line between that says how
    /// much was left out, and [`Ended::cut`] says so.
    pub max_output_chars: usize,
    /// The longest script, in characters: TypeScript is parsed on the host's side of the sandbox.
    pub max_source_chars: usize,
    /// The longest one `wait(seconds)` may ask for; a longer one is refused.
    pub max_wait: Duration,
    /// The largest arguments of one tool call, in bytes: text the host holds while it works.
    pub max_argument_bytes: usize,
}

impl Default for Limits {
    /// Lirvel's limits for a script in a conversation (`companion.md` §8x Q7, Q16).
    fn default() -> Self {
        Self {
            memory_bytes: 64 * 1024 * 1024,
            max_calls: 30,
            max_output_chars: 8_000,
            max_source_chars: 100_000,
            max_wait: Duration::from_secs(30),
            max_argument_bytes: 1024 * 1024,
        }
    }
}

/// What a script tells a model about itself — the host puts it into its tool's description, with its own rules and
/// [`declarations`] of its tools.
#[must_use]
pub fn guide(limits: &Limits) -> String {
    format!(
        "Run JavaScript or TypeScript — the body of an async function: use `await` and `return` — in a sandbox \
         without file system, network or timers. Call tools as `await tools.<name>({{...args}})`: each resolves to \
         the tool's text output — JSON.parse it when it is JSON — and rejects with an Error when the call fails or is \
         refused. Only what you print (`text(...)`, `console.log(...)`) and `return` comes back to you — filter and \
         sum up large results inside the script, and run independent calls together with `Promise.all`. At most {} \
         tool calls per script. `await wait(seconds)` pauses, at most {} s at a time; it resolves to `true` when the \
         host cut it short (go on at once), else `false`. `store(key, value)` / `load(key)` keep small JSON values \
         for later scripts. `searchTools(query)` → [{{name, description}}], \
         `describeTool(name)` → its argument schema, `ALL_TOOLS` → every name. Regular expressions are not available, \
         and with them neither `match`, `matchAll` nor `search` — use `includes`, `indexOf`, `split` and \
         `replaceAll`. Error lines count from your first line (in TypeScript only roughly).",
        limits.max_calls,
        limits.max_wait.as_secs()
    )
}

/// One tool call a script makes: the tool, its arguments as JSON text, and where the host sends its answer.
///
/// A call the script never awaits may never be sent; one sent just before the script was stopped or ended arrives
/// with nobody left to hear it — a host asks [`Request::is_cancelled`] right before a tool that changes anything.
#[derive(Debug)]
pub struct Request {
    /// The tool's name: one of the [`Catalog`]'s, checked before it is sent.
    pub name: String,
    /// Its arguments: a JSON object, parsed and written anew before it is sent — never text a host's parsers could
    /// read two ways.
    pub arguments: String,
    /// Where the answer goes; dropping it fails the call with "the host ended".
    pub reply: oneshot::Sender<Reply>,
    interrupt: Interrupt,
}

impl Request {
    /// Whether nobody waits for this call any more: the script was stopped, ran out of time or ended.
    #[must_use]
    pub fn is_cancelled(&self) -> bool {
        self.reply.is_closed() || self.interrupt.over()
    }
}

/// The host's answer to a [`Request`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Reply {
    /// The tool's text output: the call resolves to it.
    Done(String),
    /// Why it failed or was refused: the call rejects with it.
    Failed(String),
}

/// What stops a script from outside: the host's stop flag and the deadline — and what cuts its waits short.
#[derive(Debug, Clone)]
pub struct Interrupt {
    stop: Arc<AtomicBool>,
    deadline: Instant,
    wake: Arc<Notify>,
}

/// Ends the `wait` a script is in at once, or its next one should it be in none — a host's "go on" (a speaker's
/// "weiter" during a slideshow). The `wait` then resolves to `true`. Nudges given while no wait runs count once.
#[derive(Debug, Clone)]
pub struct Nudge(Arc<Notify>);

impl Nudge {
    /// Cuts the script's current or next wait short.
    pub fn nudge(&self) {
        self.0.notify_one();
    }
}

impl Interrupt {
    /// A script that may run until `deadline`.
    #[must_use]
    pub fn until(deadline: Instant) -> Self {
        Self {
            stop: Arc::default(),
            deadline,
            wake: Arc::default(),
        }
    }

    /// A script that may run until `deadline` or until `stop` is set — the host's own flag, set from anywhere.
    #[must_use]
    pub fn with_stop(stop: Arc<AtomicBool>, deadline: Instant) -> Self {
        Self {
            stop,
            deadline,
            wake: Arc::default(),
        }
    }

    /// The flag that stops the script when set.
    #[must_use]
    pub fn stopper(&self) -> Arc<AtomicBool> {
        Arc::clone(&self.stop)
    }

    /// What cuts the script's waits short.
    #[must_use]
    pub fn nudge(&self) -> Nudge {
        Nudge(Arc::clone(&self.wake))
    }

    fn stopped(&self) -> bool {
        self.stop.load(Ordering::SeqCst)
    }

    fn over(&self) -> bool {
        self.stopped() || Instant::now() >= self.deadline
    }
}

/// The values scripts keep with `store()`, shared between the scripts a host runs; a script's writes count only
/// when it ends well.
pub type Kept = Arc<Mutex<HashMap<String, String>>>;

/// How a script ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Ending {
    /// It returned (or ran to its end).
    Returned,
    /// It threw, or could not start — the text says why.
    Failed,
    /// The host stopped it, or its deadline passed.
    Interrupted,
    /// It waited for something no call and no wait would ever bring.
    Stuck,
}

/// A script's end: how, its output for the model, whether that was cut, and how many tool calls it made.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Ended {
    /// How it ended.
    pub ending: Ending,
    /// What it printed and returned, or why it failed — never empty.
    pub text: String,
    /// Whether `text` was cut to [`Limits::max_output_chars`].
    pub cut: bool,
    /// The tool calls it made (refused ones past the limit not counted).
    pub calls: usize,
}

impl Ended {
    /// Whether it returned.
    #[must_use]
    pub fn ok(&self) -> bool {
        self.ending == Ending::Returned
    }
}

/// What the watch sees of a running script.
#[derive(Default)]
struct Activity {
    /// Calls and waits not yet settled.
    open: AtomicUsize,
    /// Rises with everything a script does through the host: a call, an answer, a wait, a line.
    steps: AtomicU64,
    /// Tool calls made.
    calls: AtomicUsize,
}

impl Activity {
    fn step(&self) {
        self.steps.fetch_add(1, Ordering::SeqCst);
    }
}

/// While a call or a wait is open: closes it however its future ends, dropped included.
struct Open<'a>(&'a Activity);

impl<'a> Open<'a> {
    fn new(activity: &'a Activity) -> Self {
        activity.open.fetch_add(1, Ordering::SeqCst);
        activity.step();
        Self(activity)
    }
}

impl Drop for Open<'_> {
    fn drop(&mut self) {
        self.0.open.fetch_sub(1, Ordering::SeqCst);
        self.0.step();
    }
}

/// The JavaScript every script runs behind: the globals, built on the few Rust functions below.
const PRELUDE: &str = r#"
(() => {
  // The Rust helpers, held here and taken off the globals: a script reaches the host only through what follows.
  const emit = __emit, call = __call, pause = __wait, find = __search, describe = __describe, names = __names,
        keep = __store, fetch = __load, MAX_CALLS = __max_calls;
  for (const k of ["__emit", "__call", "__wait", "__search", "__describe", "__names", "__store", "__load", "__max_calls"])
    delete globalThis[k];
  const str = (v) => {
    if (typeof v === "string") return v;
    if (v === undefined) return "undefined";
    try {
      if (v instanceof Error) return String(v);
      const plain = (_, x) => x instanceof Map ? Object.fromEntries(x) : (x instanceof Set ? [...x] : x);
      return JSON.stringify(v, plain, 2) ?? String(v);
    } catch (_) {
      try { return String(v); } catch (_) { return "[a value that cannot be printed]"; }
    }
  };
  globalThis.__str = str;
  // Builtins that walk an array's length natively never meet the interrupt: a long one would hold the host's thread
  // past every deadline (security audit 2026-11-18). No real array of a script comes near this length.
  const MAX_LENGTH = 5000000;
  const guard = (owner, name, lengthOf) => {
    const original = owner[name];
    if (typeof original !== "function") return;
    Object.defineProperty(owner, name, {
      value: function (...a) {
        if (lengthOf(this, a) > MAX_LENGTH) throw new RangeError("arrays longer than 5000000 are not supported");
        return original.apply(this, a);
      },
      writable: true, configurable: true,
    });
  };
  const length = (x) => { try { return Number(x?.length) || 0; } catch (_) { return Infinity; } };
  for (const name of Object.getOwnPropertyNames(Array.prototype))
    if (name !== "constructor") guard(Array.prototype, name, (self) => length(self));
  guard(Array, "from", (_, a) => length(a[0]));
  const say = (...a) => emit(a.map(str).join(" "));
  globalThis.text = say;
  globalThis.console = Object.freeze({
    log: say,
    info: say,
    warn: (...a) => emit("warn: " + a.map(str).join(" ")),
    error: (...a) => emit("error: " + a.map(str).join(" ")),
  });
  let calls = 0;
  globalThis.tools = new Proxy({}, {
    get(_, name) {
      if (typeof name !== "string") return undefined;
      return async (args) => {
        // Refused here, before any work on the host's side.
        if (calls >= MAX_CALLS) throw new Error("too many tool calls: at most " + MAX_CALLS + " per script");
        calls++;
        const [ok, out] = JSON.parse(await call(name, JSON.stringify(args ?? {}) ?? "{}"));
        if (!ok) throw new Error(out);
        return out;
      };
    },
  });
  let waiting = 0;
  globalThis.wait = async (seconds) => {
    if (waiting >= 16) throw new Error("too many waits at once");
    waiting++;
    try {
      const [ok, out] = JSON.parse(await pause(Number(seconds)));
      if (!ok) throw new Error(out);
      return out;
    } finally {
      waiting--;
    }
  };
  globalThis.searchTools = (q) => JSON.parse(find(String(q ?? "")));
  globalThis.describeTool = (n) => JSON.parse(describe(String(n)));
  globalThis.ALL_TOOLS = Object.freeze(JSON.parse(names()));
  globalThis.store = (k, v) => {
    const refused = keep(String(k), JSON.stringify(v ?? null) ?? "null");
    if (refused) throw new Error(refused);
  };
  globalThis.load = (k) => { const s = fetch(String(k)); return s == null ? undefined : JSON.parse(s); };
})();
"#;

/// Runs `script`; every tool call goes out through `requests` and is answered by the host. Returns how it ended.
pub async fn run(
    script: Script,
    catalog: Arc<Catalog>,
    kept: Kept,
    limits: Limits,
    interrupt: Interrupt,
    requests: mpsc::Sender<Request>,
) -> Ended {
    if script.code.chars().count() > limits.max_source_chars {
        return failed(
            format!(
                "the script is longer than {} characters",
                limits.max_source_chars
            ),
            &limits,
        );
    }
    // Wrapped before TypeScript is stripped: a `return` at the top is no valid program on its own. The script's
    // first line stays line 1, so an error's line is the model's; whatever it throws reaches the host as an Error.
    let wrapped = format!(
        "(async () => {{{}\n}})().catch((e) => {{ throw e instanceof Error ? e : new Error(__str(e)); }})",
        script.code
    );
    let source = match script.language {
        Language::JavaScript => wrapped,
        Language::TypeScript => {
            let stripped = tokio::select! {
                stripped = typescript::strip(wrapped) => stripped,
                () = over(&interrupt) => return interrupted(&interrupt),
            };
            match stripped {
                Ok(source) => source,
                Err(why) => {
                    return failed(format!("the TypeScript could not be read: {why}"), &limits);
                }
            }
        }
    };
    let runtime = match AsyncRuntime::new() {
        Ok(runtime) => runtime,
        Err(err) => {
            return failed(
                format!("the script runtime could not start: {err}"),
                &limits,
            );
        }
    };
    runtime.set_memory_limit(limits.memory_bytes).await;
    // A deep recursion ends as a RangeError inside the script, never as an overflow of the host thread's stack.
    runtime.set_max_stack_size(JS_STACK_BYTES).await;
    let busy = interrupt.clone();
    runtime
        .set_interrupt_handler(Some(Box::new(move || busy.over())))
        .await;
    let context = match AsyncContext::custom::<Intrinsics>(&runtime).await {
        Ok(context) => context,
        Err(err) => {
            return failed(
                format!("the script runtime could not start: {err}"),
                &limits,
            );
        }
    };
    let output: Arc<Mutex<Output>> = Arc::default();
    let stores = Arc::new(Stores {
        kept: Arc::clone(&kept),
        written: Mutex::default(),
    });
    let activity = Arc::new(Activity::default());
    let host = Host {
        interrupt: interrupt.clone(),
        catalog,
        output: Arc::clone(&output),
        stores: Arc::clone(&stores),
        requests,
        activity: Arc::clone(&activity),
        limits,
    };
    let evaluation = context.async_with(async move |ctx| evaluate(ctx, source, host).await);
    // Biased: a script that can finish finishes before the watch looks again.
    let result = tokio::select! {
        biased;
        result = evaluation => Ok(result),
        ending = watch(&interrupt, &activity) => Err(ending),
    };
    let mut text = output
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .lines
        .join("\n");
    let mut add = |line: &str| {
        if !text.is_empty() {
            text.push('\n');
        }
        text.push_str(line);
    };
    let ending = match result {
        Ok(Ok(Some(returned))) => {
            add(&returned);
            Ending::Returned
        }
        Ok(Ok(None)) => Ending::Returned,
        Ok(Err(_)) | Err(Ending::Interrupted) if interrupt.over() => {
            add(if interrupt.stopped() {
                "Script stopped."
            } else {
                "Script interrupted: its time ran out."
            });
            Ending::Interrupted
        }
        Ok(Err(error)) => {
            add(&format!("Script error: {error}"));
            Ending::Failed
        }
        Err(ending) => {
            add("Script ended: it waits for something no tool call and no wait will bring.");
            ending
        }
    };
    if ending == Ending::Returned {
        stores.keep();
    }
    if text.trim().is_empty() {
        text = "(the script printed and returned nothing)".into();
    }
    let (text, cut) = bounded(text, limits.max_output_chars);
    Ended {
        ending,
        text,
        cut,
        calls: activity.calls.load(Ordering::SeqCst),
    }
}

fn failed(reason: String, limits: &Limits) -> Ended {
    let (text, cut) = bounded(format!("error: {reason}"), limits.max_output_chars);
    Ended {
        ending: Ending::Failed,
        text,
        cut,
        calls: 0,
    }
}

fn interrupted(interrupt: &Interrupt) -> Ended {
    let text = if interrupt.stopped() {
        "Script stopped."
    } else {
        "Script interrupted: its time ran out."
    };
    Ended {
        ending: Ending::Interrupted,
        text: text.into(),
        cut: false,
        calls: 0,
    }
}

/// Settles once the script is stopped or its time is up.
async fn over(interrupt: &Interrupt) {
    while !interrupt.over() {
        tokio::time::sleep(TICK).await;
    }
}

/// Ends a script that was stopped, ran out of time, or waits with nothing open and nothing happening.
async fn watch(interrupt: &Interrupt, activity: &Activity) -> Ending {
    let mut seen = activity.steps.load(Ordering::SeqCst);
    let mut quiet = 0;
    loop {
        tokio::time::sleep(TICK).await;
        if interrupt.over() {
            return Ending::Interrupted;
        }
        let steps = activity.steps.load(Ordering::SeqCst);
        if activity.open.load(Ordering::SeqCst) == 0 && steps == seen {
            quiet += 1;
            if quiet >= QUIET_TICKS {
                return Ending::Stuck;
            }
        } else {
            quiet = 0;
        }
        seen = steps;
    }
}

/// What the script's globals reach on the Rust side.
struct Host {
    interrupt: Interrupt,
    catalog: Arc<Catalog>,
    output: Arc<Mutex<Output>>,
    stores: Arc<Stores>,
    requests: mpsc::Sender<Request>,
    activity: Arc<Activity>,
    limits: Limits,
}

/// The script, run: `Ok(Some(text))` for a returned value, `Ok(None)` for none, `Err` with the error's message.
async fn evaluate<'js>(
    ctx: Ctx<'js>,
    source: String,
    host: Host,
) -> Result<Option<String>, String> {
    install(&ctx, host).map_err(|err| format!("the script runtime could not start: {err}"))?;
    let promise: Promise = ctx
        .eval(source)
        .catch(&ctx)
        .map_err(|err| err.to_string())?;
    let value: JsValue = promise
        .into_future()
        .await
        .catch(&ctx)
        .map_err(|err| err.to_string())?;
    if value.is_undefined() {
        return Ok(None);
    }
    let shown: Function = ctx.globals().get("__str").map_err(|err| err.to_string())?;
    let text: String = shown
        .call((value,))
        .catch(&ctx)
        .map_err(|err| err.to_string())?;
    Ok(Some(text))
}

/// The Rust side of the globals — emit, call, wait, search, describe, names, store, load — then the prelude.
fn install<'js>(ctx: &Ctx<'js>, host: Host) -> rquickjs::Result<()> {
    let Host {
        interrupt,
        catalog,
        output,
        stores,
        requests,
        activity,
        limits,
    } = host;
    let globals = ctx.globals();
    // Taken before the calls' closure takes the interrupt itself.
    let wake = Arc::clone(&interrupt.wake);
    globals.set("__max_calls", limits.max_calls)?;
    // A loop printing forever must not fill the memory outside QuickJS's limit; the host gets less anyway.
    let max_buffered = limits.max_output_chars.saturating_mul(10);
    let printing = Arc::clone(&activity);
    globals.set(
        "__emit",
        Function::new(ctx.clone(), move |line: String| {
            printing.step();
            output
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .push(line, max_buffered);
        })?,
    )?;
    let calling = Arc::clone(&activity);
    let offered = Arc::clone(&catalog);
    globals.set(
        "__call",
        Function::new(
            ctx.clone(),
            Async(move |name: String, arguments: String| {
                let requests = requests.clone();
                let activity = Arc::clone(&calling);
                let interrupt = interrupt.clone();
                let offered = Arc::clone(&offered);
                async move {
                    let asked = Asked { name, arguments };
                    let answer =
                        call(&activity, &requests, &limits, &offered, &interrupt, asked).await;
                    let (ok, text) = match answer {
                        Reply::Done(text) => (true, text),
                        Reply::Failed(why) => (false, why),
                    };
                    Ok::<String, rquickjs::Error>(json!([ok, text]).to_string())
                }
            }),
        )?,
    )?;
    let waiting = Arc::clone(&activity);
    globals.set(
        "__wait",
        Function::new(
            ctx.clone(),
            // `[true, woken]` when it waited — `woken` when the host cut it short — or `[false, why]`.
            Async(move |seconds: f64| {
                let activity = Arc::clone(&waiting);
                let wake = Arc::clone(&wake);
                async move {
                    if !seconds.is_finite() || seconds < 0.0 {
                        return Ok::<String, rquickjs::Error>(
                            json!([false, "wait needs a number of seconds"]).to_string(),
                        );
                    }
                    // Compared as seconds first: a huge number is no Duration at all.
                    if seconds > limits.max_wait.as_secs_f64() {
                        let most = limits.max_wait.as_secs();
                        return Ok(json!([
                            false,
                            format!("wait at most {most} seconds at a time")
                        ])
                        .to_string());
                    }
                    let asked = Duration::from_secs_f64(seconds);
                    let _open = Open::new(&activity);
                    let woken = tokio::select! {
                        () = tokio::time::sleep(asked) => false,
                        () = wake.notified() => true,
                    };
                    Ok(json!([true, woken]).to_string())
                }
            }),
        )?,
    )?;
    let search = Arc::clone(&catalog);
    globals.set(
        "__search",
        Function::new(ctx.clone(), move |query: String| {
            search.search(&query).to_string()
        })?,
    )?;
    let describe = Arc::clone(&catalog);
    globals.set(
        "__describe",
        Function::new(ctx.clone(), move |name: String| {
            describe.describe(&name).to_string()
        })?,
    )?;
    globals.set(
        "__names",
        Function::new(ctx.clone(), move || catalog.names().to_string())?,
    )?;
    let writing = Arc::clone(&stores);
    globals.set(
        "__store",
        Function::new(
            ctx.clone(),
            move |key: String, value: String| -> Option<String> {
                writing.store(key, value).err().map(str::to_owned)
            },
        )?,
    )?;
    globals.set(
        "__load",
        Function::new(ctx.clone(), move |key: String| -> Option<String> {
            stores.load(&key)
        })?,
    )?;
    ctx.eval::<(), _>(PRELUDE)
}

/// A tool call as the script made it, before it is checked.
struct Asked {
    name: String,
    arguments: String,
}

/// The longest tool name a call may carry.
const MAX_NAME_CHARS: usize = 64;

/// One tool call, checked, sent to the host and answered — or refused here: a tool the catalog does not hold, a name
/// no tool could have, arguments that are no JSON object or too large, a call past the limit.
async fn call(
    activity: &Activity,
    requests: &mpsc::Sender<Request>,
    limits: &Limits,
    catalog: &Catalog,
    interrupt: &Interrupt,
    asked: Asked,
) -> Reply {
    let Asked { name, arguments } = asked;
    let plain = !name.is_empty()
        && name.chars().count() <= MAX_NAME_CHARS
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '.' | '-'));
    if !plain || !catalog.specs.iter().any(|spec| spec.name == name) {
        let shown: String = name
            .chars()
            .take(MAX_NAME_CHARS)
            .filter(|c| !c.is_control())
            .collect();
        return Reply::Failed(format!("there is no tool {shown} for scripts"));
    }
    if arguments.len() > limits.max_argument_bytes {
        return Reply::Failed(format!(
            "the arguments are larger than {} bytes",
            limits.max_argument_bytes
        ));
    }
    // Read once and written anew: the host gets one plain JSON object, never text two parsers could read apart.
    let arguments = match serde_json::from_str::<Value>(&arguments) {
        Ok(object @ Value::Object(_)) => object.to_string(),
        Ok(_) => return Reply::Failed("the arguments must be an object".into()),
        Err(err) => return Reply::Failed(format!("the arguments are no JSON: {err}")),
    };
    // Counted before the call goes out: two calls started together must not both pass the last place.
    if activity.calls.fetch_add(1, Ordering::SeqCst) >= limits.max_calls {
        activity.calls.fetch_sub(1, Ordering::SeqCst);
        return Reply::Failed(format!(
            "too many tool calls: at most {} per script",
            limits.max_calls
        ));
    }
    let _open = Open::new(activity);
    let (reply, answer) = oneshot::channel();
    let request = Request {
        name,
        arguments,
        reply,
        interrupt: interrupt.clone(),
    };
    match requests.send(request).await {
        Ok(()) => answer
            .await
            .unwrap_or_else(|_| Reply::Failed("the host ended".into())),
        Err(_) => Reply::Failed("the host ended".into()),
    }
}

/// What a script printed: its lines and their characters, a newline each, counted as they come.
#[derive(Default)]
struct Output {
    lines: Vec<String>,
    chars: usize,
}

impl Output {
    /// Keeps `line` while all lines stay within `max` characters; past it one [`DROPPED`] line stands for the rest.
    fn push(&mut self, line: String, max: usize) {
        let size = line.chars().count() + 1;
        if self.chars + size <= max {
            self.chars += size;
            self.lines.push(line);
        } else if self.lines.last().is_none_or(|last| last != DROPPED) {
            self.lines.push(DROPPED.to_owned());
        }
    }
}

/// The values a script keeps: what was kept before it, and its own writes, which join the kept ones only when it
/// returns — so two scripts at once lose nothing of each other's.
struct Stores {
    kept: Kept,
    written: Mutex<HashMap<String, String>>,
}

impl Stores {
    fn load(&self, key: &str) -> Option<String> {
        if let Some(value) = self
            .written
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .get(key)
        {
            return Some(value.clone());
        }
        self.kept
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .get(key)
            .cloned()
    }

    /// Writes one value for this script, within the limits on keys, values and all together.
    fn store(&self, key: String, value: String) -> Result<(), &'static str> {
        let kept = self.kept.lock().unwrap_or_else(|p| p.into_inner());
        let mut written = self.written.lock().unwrap_or_else(|p| p.into_inner());
        let mut all: HashMap<&String, &String> = kept.iter().collect();
        all.extend(written.iter());
        fits(&all, &key, &value)?;
        drop(all);
        written.insert(key, value);
        Ok(())
    }

    /// The script returned: its writes join the kept values, each as long as it still fits.
    fn keep(&self) {
        let mut kept = self.kept.lock().unwrap_or_else(|p| p.into_inner());
        let written = std::mem::take(&mut *self.written.lock().unwrap_or_else(|p| p.into_inner()));
        for (key, value) in written {
            let all: HashMap<&String, &String> = kept.iter().collect();
            if fits(&all, &key, &value).is_ok() {
                kept.insert(key, value);
            }
        }
    }
}

/// Whether `key` with `value` fits beside `all`: the limits on one value, one key, the keys and all together.
fn fits(all: &HashMap<&String, &String>, key: &str, value: &str) -> Result<(), &'static str> {
    if value.chars().count() > MAX_STORED_VALUE_CHARS {
        return Err("store: the value is too large");
    }
    let new_key = !all.keys().any(|k| k.as_str() == key);
    if key.chars().count() > MAX_STORED_KEY_CHARS || (new_key && all.len() >= MAX_STORED_KEYS) {
        return Err("store: too long a key, or too many keys");
    }
    // Keys count too: many large keys with empty values would grow the map as well.
    let total: usize = all
        .iter()
        .filter(|(k, _)| k.as_str() != key)
        .map(|(k, v)| k.chars().count() + v.chars().count())
        .sum();
    if total + key.chars().count() + value.chars().count() > MAX_STORED_TOTAL_CHARS {
        return Err("store: the store is full");
    }
    Ok(())
}

/// The output within `max` characters: whole when it fits; else two thirds head, one third tail and a line between
/// that says how much was left out.
fn bounded(text: String, max: usize) -> (String, bool) {
    let length = text.chars().count();
    if length <= max {
        return (text, false);
    }
    let head = max * 2 / 3;
    let tail = max - head;
    let left_out = length - head - tail;
    let first: String = text.chars().take(head).collect();
    let last: String = text.chars().skip(length - tail).collect();
    (
        format!("{first}\n[… {left_out} characters left out …]\n{last}"),
        true,
    )
}

#[cfg(test)]
mod tests;
