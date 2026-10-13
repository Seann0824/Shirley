//! The "intent" description for sandboxed execution.
//!
//! This layer is deliberately platform-independent: it only describes "what I want to run and what
//! it is allowed to touch"; which mechanism implements it (bwrap / sandbox-exec / a bare process)
//! is left to the backend.

// `use` brings in external types, avoiding full paths everywhere.
// PathBuf: a "file path" type that can represent things like /Users/xxx/a.txt.
use std::path::PathBuf;
// Duration: a "time span" type, e.g. 60 seconds, 200 milliseconds.
use std::time::Duration;

/// Network egress policy. Defaults to `Disabled` — matching "the fake world has no network by
/// default".
///
/// An `enum` is a "choose one of these" type: a value can only be one of the variants below.
#[derive(Debug, Clone, Default)] // Debug=printable; Clone=copyable; Default=has a default value
pub enum NetworkPolicy {
    /// No network at all. The backend must guarantee the process cannot open any connection.
    #[default] // marks "this is the default" — so NetworkPolicy::default() yields Disabled
    Disabled,

    /// Only allow egress through the given proxy (e.g. `127.0.0.1:8888`).
    /// In a real implementation this is the sole permitted egress; the proxy handles domain
    /// allow-listing and auditing.
    Proxy {
        // The brace form means this variant carries data: an address string.
        // E.g. NetworkPolicy::Proxy { addr: "127.0.0.1:8888".into() }
        addr: String,
    },
}

/// The full specification of one command execution.
///
/// Note: there is no "command string" here. `program` and `args` are separate, avoiding the
/// `bash -c` problem of "hiding semantics inside a string that cannot be statically reasoned
/// about".
#[derive(Debug, Clone)] // printable, copyable (note: no Default; it is hand-written below)
pub struct SandboxSpec {
    // A `struct` is a "bundle of fields" type. `pub` means it can be read directly from outside.

    /// The program to execute, e.g. `/usr/bin/python3`.
    pub program: String, // String: a variable-length string

    /// The argument list, passed item by item with no string splitting.
    pub args: Vec<String>, // Vec<T>: a list of T, here a list of strings

    /// Working directory. Defaults to `workspace_root`.
    pub cwd: Option<PathBuf>, // Option<T>: either has a value Some(T) or none None

    /// Injected environment variables. Unlisted environment variables are not inherited by default
    /// (to avoid leaking API keys).
    pub env: Vec<(String, String)>, // a list of "key-value pairs", represented as tuples (String, String)

    /// Wall-clock timeout. On timeout the outer layer force-kills the process; it does not rely on
    /// the process exiting on its own.
    pub timeout: Duration, // a time span, e.g. 60 seconds

    /// Workspace root. All relative paths are bounded by this, and the backend should constrain the
    /// cwd accordingly.
    pub workspace_root: Option<PathBuf>,

    /// Paths that may be written (relative or absolute). Everything else is read-only or invisible.
    pub writable_paths: Vec<PathBuf>,

    /// Paths that may be read. When empty, the backend uses its own default read-only set.
    pub read_only_paths: Vec<PathBuf>,

    /// Network policy.
    pub network: NetworkPolicy,

    /// Memory cap (MiB). The backend implements it as its capabilities allow; unsupported cases
    /// should be recorded as degradation.
    pub memory_limit_mb: Option<u64>, // u64: unsigned integer (cannot be negative)

    /// Maximum number of processes (guards against fork bombs).
    pub max_processes: Option<u32>, // u32: unsigned 32-bit integer
}

// `impl Default for X` means "define the default value for X".
// With it, `SandboxSpec::default()` yields an instance with "every field at its default".
impl Default for SandboxSpec {
    // fn default() is the function the Default trait requires; it returns Self (i.e. SandboxSpec)
    fn default() -> Self {
        Self {
            // Self { ... } "constructs an instance"; every field must be given a value.
            program: String::new(),              // empty string ""
            args: Vec::new(),                    // empty list []
            cwd: None,                           // no working directory
            env: Vec::new(),                     // empty list
            timeout: Duration::from_secs(60),    // 60 seconds
            workspace_root: None,
            writable_paths: Vec::new(),
            read_only_paths: Vec::new(),
            network: NetworkPolicy::default(),   // uses the #[default] Disabled above
            memory_limit_mb: None,
            max_processes: None,
        }
    }
}

// `impl SandboxSpec` means "add methods to SandboxSpec".
// The methods here are all "chained": each returns Self, so you can keep going
// .arg().env().timeout().
impl SandboxSpec {
    // `impl Into<String>` means "anything that can turn into a String",
    // so both &str ("python3") and String work. This is idiomatic Rust.
    pub fn new(program: impl Into<String>) -> Self {
        Self {
            program: program.into(), // .into() turns the argument into a String
            ..Default::default()     // `..` means "fill the remaining fields with defaults" (from Default above)
        }
    }

    // `mut self` means "take ownership and allow mutation", then return it (chained style).
    pub fn arg(mut self, arg: impl Into<String>) -> Self {
        self.args.push(arg.into()); // push: append one to the end of the list
        self                        // return self so .xxx() can continue
    }

    // Generics: I is "anything iterable", S is "the element it yields".
    // The where clause is a constraint: I yields a series of S, and S can turn into a String.
    pub fn args<I, S>(mut self, args: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        // extend: append a whole series. .map(Into::into) turns each S into a String.
        self.args.extend(args.into_iter().map(Into::into));
        self
    }

    // Add one environment-variable key-value pair at a time.
    pub fn env(mut self, key: impl Into<String>, value: impl Into<String>) -> Self {
        self.env.push((key.into(), value.into())); // pack into a tuple (key, value) and push into the list
        self
    }

    // Set the working directory.
    pub fn cwd(mut self, dir: impl Into<PathBuf>) -> Self {
        self.cwd = Some(dir.into()); // wrap in Some(...), meaning "it now has a value"
        self
    }

    // Set the workspace root.
    pub fn workspace_root(mut self, dir: impl Into<PathBuf>) -> Self {
        self.workspace_root = Some(dir.into());
        self
    }

    // Append one writable path.
    pub fn writable(mut self, path: impl Into<PathBuf>) -> Self {
        self.writable_paths.push(path.into());
        self
    }

    // Set the timeout.
    pub fn timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout; // overwrite directly
        self
    }

    // Set the network policy.
    pub fn network(mut self, policy: NetworkPolicy) -> Self {
        self.network = policy;
        self
    }
}
